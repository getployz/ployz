import { createPrivateKey, sign } from "node:crypto";
import { it } from "@effect/vitest";
import { asc, sql } from "drizzle-orm";
import { type Context, Effect, Fiber, Layer } from "effect";
import { Inngest } from "inngest";
import { Client } from "pg";
import { expect, vi } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { organizationMachine } from "#/modules/machines/tables";
import { observePushMessage, verifyObservePush } from "#/modules/observe-ingest/ingest.server";
import { visitorId } from "#/modules/observe-ingest/ingest";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { operationOutbox } from "#/modules/operations/tables";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { handleObservePush } from "#/routes/api/machines/-handlers";
import { forgetPartitions } from "#/modules/web-analytics/postgres/partitions";
import { closeDays, rebuildDirtyDays } from "#/modules/web-analytics/postgres/rollup";
import { dailyTotals, dailyTotalsSql, type StoreQuery } from "#/modules/web-analytics/postgres/store";
import { saltWindow, setSiteAnalytics, siteDay } from "#/modules/web-analytics/salts.server";
import { ClickHouse } from "#/modules/web-analytics/clickhouse/client";
import { clickHouseRow } from "#/modules/web-analytics/clickhouse/writer";
import { analyticsBotDaily, analyticsGapDaily, analyticsOutbox, analyticsPageview, analyticsRollupState } from "#/modules/web-analytics/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { machineId } from "#/test/ids";
import type { MachineId } from "@ployz/sdk";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f201";
const PRODUCTION = "00000000-0000-4000-8000-00000000f203";
const STAGING = "00000000-0000-4000-8000-00000000f204";
const MACHINE = machineId("0");
const WEB = "c".repeat(32);
const API = "d".repeat(32);

const marker = (service: string, exitCode: number) => ({
  machine_id: MACHINE,
  container_id: "b".repeat(64),
  service_id: service,
  service: service === WEB ? "prod/web" : "prod/api",
  replica: "1",
  ts: 1_760_000_000_000_000_000,
  kind: "exit",
  exit_code: exitCode,
  oom: false,
  stopped: false,
  future_field: "ignored",
});

type PushFields = {
  readonly stream?: string;
  readonly seq?: string | number;
  readonly events?: ReadonlyArray<{ readonly seq: string; readonly marker: { readonly kind: string } }>;
};

const push = (events: ReadonlyArray<readonly [seq: number, service: string, exitCode?: number]>, fields: PushFields = {}) =>
  JSON.stringify({
    stream: "crash",
    seq: String(Math.max(...events.map(([seq]) => seq))),
    dropped: 0,
    events: events.map(([seq, service, exitCode = 1]) => ({ seq: String(seq), marker: marker(service, exitCode) })),
    ...fields,
  });

// The known-answer vector core signs with too (contract "Auth").
const SEED = "07".repeat(32);
const PUBLIC_KEY = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const OTHER_SEED = "08".repeat(32);

const capability = (publicKey: string) => `ployz1:${Buffer.concat([Buffer.from(publicKey, "hex"), Buffer.alloc(32, 9)]).toString("base64url")}`;
const privateKey = (seed: string) =>
  createPrivateKey({ key: Buffer.from(`302e020100300506032b657004220420${seed}`, "hex"), format: "der", type: "pkcs8" });

type Signing = {
  readonly machine?: MachineId;
  readonly seed?: string;
  readonly at?: number;
  readonly signed?: string;
};

const headersFor = (body: string, { machine = MACHINE, seed = SEED, at = Math.floor(Date.now() / 1000), signed = body }: Signing = {}) => {
  const signature = sign(null, observePushMessage(machine, BigInt(at), Buffer.from(signed)), privateKey(seed));
  return { "ployz-timestamp": String(at), "ployz-signature": `v1=${signature.toString("base64url")}` };
};

it("the known-answer vector verifies, and not once its timestamp or body changes", () => {
  const body = Buffer.from('{"stream":"crash","seq":"1760000000123456789","dropped":0,"events":[]}');
  const signature = Buffer.from("rOUkHmKtJRTf53MNT4z0t9gV3yOkQnJ_yriYa5G7ebHo5-VrHUa0spxdC56PowBMo9JEMEy_JXT97yIG11RPAw", "base64url");
  const message = observePushMessage("a".repeat(32), 1_760_000_000n, body);
  expect(verifyObservePush(PUBLIC_KEY, message, signature)).toBe(true);
  expect(sign(null, message, privateKey(SEED)).equals(signature)).toBe(true);
  expect(verifyObservePush(PUBLIC_KEY, observePushMessage("a".repeat(32), 1_760_000_001n, body), signature)).toBe(false);
  expect(verifyObservePush(PUBLIC_KEY, observePushMessage("a".repeat(32), 1_760_000_000n, Buffer.concat([body, Buffer.from(" ")])), signature)).toBe(false);
});

const setup = Effect.fn(function* (env: Readonly<Record<string, string>> = { ANALYTICS_ENABLED: "true" }) {
  const inngest = new Inngest({ id: "observe-ingest-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  forgetPartitions();
  const services = yield* Layer.build(yield* storeTestCloud({ inngest, env }));
  const provide = <A, E>(effect: Effect.Effect<A, E, Database>) => effect.pipe(Effect.provide(services));
  yield* provide(seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }));
  yield* enrollStoreServer(ORGANIZATION).pipe(Effect.provide(services));
  // The fixture's capability is not a real one; give the Machine the vector's key, with no identity kept yet.
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.update(organizationMachine).set({ encryptedCapability: encryption.encrypt(capability(PUBLIC_KEY)) });
  }).pipe(Effect.provide(services), Effect.orDie);

  const sendRaw = (body: string | ReadableStream<Uint8Array>, headers: Record<string, string>, machine = MACHINE) => {
    // undici refuses a stream body unless the request declares half duplex.
    const init: RequestInit & { readonly duplex: "half" } = { method: "POST", body, headers, duplex: "half" };
    const request = new Request(`https://cloud.example.com/api/machines/${machine}/observe`, init);
    return handleObservePush(request, machine).pipe(Effect.provide(services));
  };
  const send = (body: string, signing: Signing = {}) => sendRaw(body, headersFor(body, signing), signing.machine);
  const identity = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return (yield* drizzle.select({ organizationId: organizationMachine.organizationId, managementIdentity: organizationMachine.managementIdentity })
      .from(organizationMachine).orderBy(asc(organizationMachine.organizationId)));
  }).pipe(Effect.orDie));
  const enrollElsewhere = (organizationId: string) => Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.insert(organization).values({ id: organizationId, name: "Other", slug: "other" });
    yield* drizzle.insert(organizationPairing).values({
      organizationId, encryptedPairingSecret: encryption.encrypt("ppair_other"), founderPublicKey: "founder-key", founderClaimMachineId: MACHINE,
    });
    yield* drizzle.insert(organizationMachine).values({
      organizationId, machineId: MACHINE, clusterKey: "e".repeat(64), encryptedCapability: encryption.encrypt(capability(PUBLIC_KEY)),
      managementIdentity: PUBLIC_KEY, isDialEntry: true,
    });
  }).pipe(Effect.provide(services), Effect.orDie);
  const rows = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return {
      crashes: (yield* drizzle.select().from(crashEvent).orderBy(asc(crashEvent.seq)))
        .map(({ seq, serviceId, serviceName, exitCode, oom }) => ({ seq, serviceId, serviceName, exitCode, oom })),
      cursor: (yield* drizzle.select().from(observeIngestCursor)).map(({ stream, seq }) => ({ stream, seq })),
      events: (yield* drizzle.select().from(operationOutbox)).map(({ name, data }) => ({ name, data })),
    };
  }).pipe(Effect.orDie));
  const sites = () => Effect.gen(function* () {
    const store = yield* cloudStore;
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: "00000000-0000-4000-8000-00000000f202", name: "shop", default_environment: PRODUCTION }));
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_environment", id: STAGING, project: "shop", name: "staging" }));
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`
      insert into config_namespace (organization_id, namespace, environment_id)
      values (${ORGANIZATION}, 'shop-prod', ${PRODUCTION}), (${ORGANIZATION}, 'shop-staging', ${STAGING})
      on conflict do nothing`);
    yield* setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "Australia/Sydney" });
    yield* setSiteAnalytics(ORGANIZATION, STAGING, { enabled: false });
  }).pipe(Effect.provide(services), Effect.orDie);
  const breakSalts = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`alter table analytics.site rename to site_gone`);
  }).pipe(Effect.orDie));
  const corruptTimezone = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`update analytics.site set timezone = 'Not/A_Zone'`);
  }).pipe(Effect.orDie));
  const stored = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return {
      pageviews: yield* drizzle.select().from(analyticsPageview).orderBy(asc(analyticsPageview.ts)),
      bots: yield* drizzle.select().from(analyticsBotDaily).orderBy(asc(analyticsBotDaily.day), asc(analyticsBotDaily.reason)),
      gaps: yield* drizzle.select().from(analyticsGapDaily).orderBy(asc(analyticsGapDaily.day)),
      cursor: (yield* drizzle.select().from(observeIngestCursor)).map(({ stream, seq }) => ({ stream, seq })),
    };
  }).pipe(Effect.orDie));
  const rollupState = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    return yield* drizzle.select({ environmentId: analyticsRollupState.environmentId, rolledUpTo: analyticsRollupState.rolledUpTo, dirty: analyticsRollupState.dirty })
      .from(analyticsRollupState);
  }).pipe(Effect.orDie));
  const rolledUp = (rolledUpTo: string, dirty: ReadonlyArray<string>) => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.update(analyticsRollupState).set({ rolledUpTo, dirty: [...dirty] });
  }).pipe(Effect.orDie));
  return { services, send, sendRaw, rows, identity, enrollElsewhere, sites, breakSalts, corruptTimezone, stored, rollupState, rolledUp };
});

it.live("a push keeps every event the Machine shipped, advances the cursor, and asks for one mail per Service", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const answer = yield* send(push([[10, WEB], [11, WEB, 0], [12, API, 137]]));
  expect(answer.status).toBe(200);
  expect(answer.headers.get("cache-control")).toBe("no-store");
  expect(answer.headers.get("content-type")).toBe("application/json");
  expect(yield* Effect.promise(() => answer.json())).toEqual({ sites: [] });
  expect(yield* rows()).toEqual({
    crashes: [
      { seq: 10n, serviceId: WEB, serviceName: "prod/web", exitCode: 1, oom: false },
      { seq: 11n, serviceId: WEB, serviceName: "prod/web", exitCode: 0, oom: false },
      { seq: 12n, serviceId: API, serviceName: "prod/api", exitCode: 137, oom: false },
    ],
    cursor: [{ stream: "crash", seq: 12n }],
    events: expect.arrayContaining([
      { name: "observe/crash.received", data: { organizationId: ORGANIZATION, serviceId: WEB } },
      { name: "observe/crash.received", data: { organizationId: ORGANIZATION, serviceId: API } },
    ]),
  });
  expect((yield* rows()).events).toHaveLength(2);
}), 60_000);

it.live("a replayed seq answers 200 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const body = push([[10, WEB], [12, WEB]]);
  expect((yield* send(body)).status).toBe(200);
  const before = yield* rows();
  expect(before.crashes.map(({ seq }) => seq)).toEqual([10n, 12n]);
  expect(before.events).toHaveLength(1);
  expect((yield* send(body)).status).toBe(200);
  expect(yield* rows()).toEqual(before);
}), 60_000);

it.live("crashes behind a cursor that went backwards are kept and mailed, and the cursor stays ahead", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[20, WEB]]))).status).toBe(200);
  expect((yield* send(push([[5, API]]))).status).toBe(200);
  const { crashes, cursor, events } = yield* rows();
  expect(crashes.map(({ seq }) => seq)).toEqual([5n, 20n]);
  expect(cursor).toEqual([{ stream: "crash", seq: 20n }]);
  expect(events.map(({ data }) => data)).toEqual([
    { organizationId: ORGANIZATION, serviceId: WEB },
    { organizationId: ORGANIZATION, serviceId: API },
  ]);
}), 60_000);

it.live("a batch that straddles the cursor keeps only the crashes it had not kept", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[10, WEB], [12, WEB]]))).status).toBe(200);
  expect((yield* send(push([[12, WEB], [13, API]]))).status).toBe(200);
  const { crashes, cursor, events } = yield* rows();
  expect(crashes.map(({ seq }) => seq)).toEqual([10n, 12n, 13n]);
  expect(cursor).toEqual([{ stream: "crash", seq: 13n }]);
  expect(events.map(({ data }) => data)).toEqual([
    { organizationId: ORGANIZATION, serviceId: WEB },
    { organizationId: ORGANIZATION, serviceId: API },
  ]);
}), 60_000);

const EMPTY = JSON.stringify({ stream: "crash", seq: "0", dropped: 0, events: [] });

it.live("an empty crash batch is a salt refresh: 200 with salts for on sites only", () => Effect.gen(function* () {
  const { send, rows, sites } = yield* setup();
  yield* sites();
  const answer = yield* send(EMPTY);
  expect(answer.status).toBe(200);
  expect(answer.headers.get("content-type")).toBe("application/json");
  const now = Date.now();
  const days = [0, 1, 2].map((offset) => siteDay("Australia/Sydney", now, offset));
  expect(yield* Effect.promise(() => answer.json())).toEqual({
    sites: [{
      namespace: "shop-prod",
      salts: days.map((day) => ({ day, ...saltWindow(day, "Australia/Sydney"), salt: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/) })),
    }],
  });
  expect(yield* rows()).toEqual({ ...nothing, cursor: [{ stream: "crash", seq: 0n }] });
}), 60_000);

it.live("a replay below the cursor carries the same salts", () => Effect.gen(function* () {
  const { send, sites } = yield* setup();
  yield* sites();
  const first = yield* send(push([[10, WEB]]));
  const replay = yield* send(EMPTY);
  expect(replay.status).toBe(200);
  expect(yield* Effect.promise(() => replay.json())).toEqual(yield* Effect.promise(() => first.json()));
}), 60_000);

it.live("a push Cloud kept but cannot issue salts for answers 204, which keeps the Machine's salts", () => Effect.gen(function* () {
  const { send, rows, sites, breakSalts } = yield* setup();
  yield* sites();
  yield* breakSalts();
  const answer = yield* send(push([[10, WEB]]));
  expect(answer.status).toBe(204);
  expect(yield* Effect.promise(() => answer.text())).toBe("");
  expect((yield* rows()).cursor).toEqual([{ stream: "crash", seq: 10n }]);
}), 60_000);

it.live("a push Cloud kept answers 204 even when issuing salts dies", () => Effect.gen(function* () {
  const { send, rows, sites, corruptTimezone } = yield* setup();
  yield* sites();
  yield* corruptTimezone();
  const answer = yield* send(push([[10, WEB]]));
  expect(answer.status).toBe(204);
  expect((yield* rows()).cursor).toEqual([{ stream: "crash", seq: 10n }]);
}), 60_000);

it.live("a stream Cloud does not know answers 404 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const answer = yield* send(push([[10, WEB]], { stream: "logs" }));
  expect(answer.status).toBe(404);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

const nothing = { crashes: [], cursor: [], events: [] };

it.live("a push signed by another key answers 401 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[10, WEB]]), { seed: OTHER_SEED })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a push whose body changed after signing answers 401 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[10, WEB]]), { signed: push([[10, API]]) })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a push signed more than five minutes away from now answers 401 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const now = Math.floor(Date.now() / 1000);
  expect((yield* send(push([[10, WEB]]), { at: now - 301 })).status).toBe(401);
  expect((yield* send(push([[10, WEB]]), { at: now + 301 })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
  expect((yield* send(push([[10, WEB]]), { at: now - 290 })).status).toBe(200);
}), 60_000);

it.live("a push with no signature, a malformed one, or a stale timestamp answers 401 before its body or the database is read", () =>
  Effect.gen(function* () {
    const { sendRaw, rows, identity } = yield* setup();
    const body = push([[10, WEB]]);
    const signed = headersFor(body);
    const stale = headersFor(body, { at: Math.floor(Date.now() / 1000) - 301 });
    const cases: ReadonlyArray<Record<string, string>> = [
      {},
      { "ployz-timestamp": signed["ployz-timestamp"] },
      { "ployz-signature": signed["ployz-signature"] },
      { ...signed, "ployz-signature": signed["ployz-signature"].slice(3) },
      { ...signed, "ployz-timestamp": `${signed["ployz-timestamp"]}.0` },
      stale,
    ];
    for (const headers of cases) {
      const stream = countedStream(Buffer.from(body), 1);
      expect((yield* sendRaw(stream.body, headers)).status).toBe(401);
      expect(stream.pulled()).toBe(0);
    }
    expect(yield* identity()).toEqual([{ organizationId: ORGANIZATION, managementIdentity: null }]);
    expect(yield* rows()).toEqual(nothing);
  }), 60_000);

const MAX_PUSH_BYTES = 256 * 1024;

/** A body that hands out `chunk` up to `limit` times, only when read, and counts the reads. */
const countedStream = (chunk: Uint8Array, limit = Number.POSITIVE_INFINITY) => {
  let pulled = 0;
  const body = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (pulled >= limit) return controller.close();
      pulled += 1;
      controller.enqueue(chunk);
    },
  }, { highWaterMark: 0 });
  return { body, pulled: () => pulled };
};

it.live("a signed push over 256 KiB answers 413 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  const body = push([[10, WEB]]) + " ".repeat(MAX_PUSH_BYTES);
  expect((yield* send(body)).status).toBe(413);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("an endless body stops being read at 256 KiB and answers 413", () => Effect.gen(function* () {
  const { sendRaw, rows } = yield* setup();
  const chunk = Buffer.alloc(64 * 1024, 0x20);
  const stream = countedStream(chunk);
  expect((yield* sendRaw(stream.body, headersFor(""))).status).toBe(413);
  expect(stream.pulled()).toBe(MAX_PUSH_BYTES / chunk.byteLength + 1);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a body whose Content-Length is over 256 KiB answers 413 without being read", () => Effect.gen(function* () {
  const { sendRaw, rows } = yield* setup();
  const stream = countedStream(Buffer.from(push([[10, WEB]])), 1);
  const answer = yield* sendRaw(stream.body, { ...headersFor(""), "content-length": String(MAX_PUSH_BYTES + 1) });
  expect(answer.status).toBe(413);
  expect(stream.pulled()).toBe(0);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a push from a Machine no Organization enrolled answers 401 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  expect((yield* send(push([[10, WEB]]), { machine: machineId("1") })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a Machine enrolled before Cloud kept its key gets the key from its capability on its first push", () => Effect.gen(function* () {
  const { send, identity } = yield* setup();
  expect(yield* identity()).toEqual([{ organizationId: ORGANIZATION, managementIdentity: null }]);
  expect((yield* send(push([[10, WEB]]))).status).toBe(200);
  expect(yield* identity()).toEqual([{ organizationId: ORGANIZATION, managementIdentity: PUBLIC_KEY }]);
}), 60_000);

it.live("a push two Organizations' rows would both verify answers 401 and writes nothing", () => Effect.gen(function* () {
  const { send, rows, enrollElsewhere } = yield* setup();
  yield* enrollElsewhere("00000000-0000-4000-8000-00000000f202");
  expect((yield* send(push([[10, WEB]]))).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a malformed push answers 422 and writes nothing", () => Effect.gen(function* () {
  const { send, rows } = yield* setup();
  for (const body of [
    "not json",
    push([[10, WEB]], { seq: 10 }),
    push([[10, WEB]], { seq: "9999999999999999999" }),
    push([[10, WEB]], { events: [{ seq: "10", marker: { kind: "exit" } }] }),
    push([[10, WEB]]).replace(`"ts":${marker(WEB, 1).ts}`, '"ts":1e100'),
    push([[10, WEB]]).replace(`"ts":${marker(WEB, 1).ts}`, '"ts":-1'),
  ]) {
    expect((yield* send(body)).status).toBe(422);
  }
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

const SYDNEY = "Australia/Sydney";
const VISITOR = "ab".repeat(32);
const HOUR_MS = 3_600_000;

type QueryKey = "utm_source" | "utm_medium" | "utm_campaign" | "utm_term" | "utm_content" | "ref";
type WirePageview = {
  readonly kind: "pageview";
  readonly ts: number;
  readonly day: string;
  readonly namespace: string;
  readonly host: string;
  readonly path: string;
  readonly query: { readonly [key in QueryKey]?: string };
  readonly referrer_host: string | null;
  readonly language: string | null;
  readonly visitor: string;
  readonly flags: ReadonlyArray<string>;
};
type WireBots = { readonly kind: "bots"; readonly namespace: string; readonly day: string; readonly reason: string; readonly count: number };

const pageview = (ts: number, fields: Partial<WirePageview> = {}): WirePageview => ({
  kind: "pageview", ts, day: "2000-01-01", namespace: "shop-prod", host: "shop.example.com", path: "/", query: {},
  referrer_host: null, language: "en", visitor: VISITOR, flags: [], ...fields,
});
const bots = (count: number, fields: Partial<WireBots> = {}): WireBots =>
  ({ kind: "bots", namespace: "shop-prod", day: "2026-10-01", reason: "user_agent", count, ...fields });
const pageviews = (seq: number, events: ReadonlyArray<object>, dropped = 0) =>
  JSON.stringify({ stream: "pageviews", seq: String(seq), dropped, events });

const plainRow = (ts: number) => ({
  environmentId: PRODUCTION, ts: new Date(ts), visitorId: -6076574518398440533n, host: "shop.example.com", pathname: "/", source: "",
  referrerHost: null, utmSource: null, utmMedium: null, utmCampaign: null, utmTerm: null, utmContent: null, ref: null, language: "en", flags: 0,
});

const nothingStored = { pageviews: [], bots: [], gaps: [], cursor: [] };

it("a visitor's id is the first 8 bytes of the Machine's hash, signed", () => {
  expect(visitorId(VISITOR)).toBe(-6076574518398440533n);
  expect(visitorId(`0123456789abcdef${"f".repeat(48)}`)).toBe(81985529216486895n);
});

it.live("a pageviews push stores its rows, sums repeated bot and gap counts, advances the cursor, and answers with salts", () =>
  Effect.gen(function* () {
    const { send, stored, sites } = yield* setup();
    yield* sites();
    const now = Date.now();
    const at = now - HOUR_MS;
    const answer = yield* send(pageviews(5, [
      pageview(at),
      pageview(at + 1000, {
        path: "/pricing",
        query: { utm_source: "news", utm_medium: "email", utm_campaign: "launch", utm_term: "deploy", utm_content: "hero", ref: "hn" },
        referrer_host: "news.ycombinator.com", language: null, flags: ["no_accept_language", "click_id"],
      }),
      pageview(at + 2000, { path: "/docs", query: { ref: "hn" }, referrer_host: "news.ycombinator.com", flags: ["no_sec_fetch"] }),
      pageview(at + 3000, { path: "/blog", referrer_host: "google.com" }),
      bots(3),
      bots(4),
      bots(1, { reason: "no_accept" }),
      { kind: "gap", namespace: "shop-prod", day: null, count: 2 },
      { kind: "gap", namespace: "shop-prod", day: null, count: 5 },
    ], 7));
    expect(answer.status).toBe(200);
    const ack: { readonly sites: ReadonlyArray<{ readonly namespace: string }> } = yield* Effect.promise(() => answer.json());
    expect(ack.sites.map(({ namespace }) => namespace)).toEqual(["shop-prod"]);
    expect(yield* stored()).toEqual({
      pageviews: [
        plainRow(at),
        {
          ...plainRow(at + 1000), pathname: "/pricing", source: "news", referrerHost: "news.ycombinator.com", utmSource: "news", utmMedium: "email",
          utmCampaign: "launch", utmTerm: "deploy", utmContent: "hero", ref: "hn", language: null, flags: 5,
        },
        { ...plainRow(at + 2000), pathname: "/docs", source: "hn", referrerHost: "news.ycombinator.com", ref: "hn", flags: 2 },
        { ...plainRow(at + 3000), pathname: "/blog", source: "google.com", referrerHost: "google.com" },
      ],
      bots: [
        { environmentId: PRODUCTION, day: "2026-10-01", reason: "no_accept", count: 1 },
        { environmentId: PRODUCTION, day: "2026-10-01", reason: "user_agent", count: 7 },
      ],
      gaps: [{ environmentId: PRODUCTION, day: siteDay(SYDNEY, now), count: 7 }],
      cursor: [{ stream: "pageviews", seq: 5n }],
    });
  }), 60_000);

it.live("a replayed batch stores no new rows and adds no bot or gap counts", () => Effect.gen(function* () {
  const { send, stored, sites } = yield* setup();
  yield* sites();
  const at = Date.now() - HOUR_MS;
  const body = pageviews(5, [pageview(at), pageview(at + 1000), bots(3), { kind: "gap", namespace: "shop-prod", day: "2026-10-01", count: 2 }]);
  expect((yield* send(body)).status).toBe(200);
  const before = yield* stored();
  expect(before.pageviews).toHaveLength(2);
  expect(before.bots.map(({ count }) => count)).toEqual([3]);
  expect(before.gaps.map(({ count }) => count)).toEqual([2]);
  expect((yield* send(body)).status).toBe(200);
  expect(yield* stored()).toEqual(before);
}), 60_000);

const outbox = (services: Context.Context<Database>) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ environmentId: analyticsOutbox.environmentId, rows: analyticsOutbox.rows }).from(analyticsOutbox);
}).pipe(Effect.provide(services), Effect.orDie);

it.live("without ClickHouse a pageviews push leaves the outbox empty", () => Effect.gen(function* () {
  const { send, stored, sites, services } = yield* setup();
  yield* sites();
  expect((yield* send(pageviews(5, [pageview(Date.now() - HOUR_MS)]))).status).toBe(200);
  expect((yield* stored()).pageviews).toHaveLength(1);
  expect(yield* outbox(services)).toEqual([]);
}), 60_000);

it.live("with ClickHouse a pageviews push queues the rows it stored, and a replay queues nothing more", () => Effect.gen(function* () {
  const { sendRaw, sites, services } = yield* setup();
  yield* sites();
  const at = Date.now() - HOUR_MS;
  const body = pageviews(5, [pageview(at)]);
  const unused = Effect.die("ingest never calls ClickHouse");
  const clickhouse = { query: () => unused, command: () => unused, insert: () => unused };
  const send = () => sendRaw(body, headersFor(body)).pipe(Effect.provideService(ClickHouse, clickhouse));
  expect((yield* send()).status).toBe(200);
  expect((yield* send()).status).toBe(200);
  expect(yield* outbox(services)).toEqual([{ environmentId: PRODUCTION, rows: [clickHouseRow(plainRow(at))] }]);
}), 60_000);

it.live("a batch whose ack was lost, back merged with newer rows under a higher seq, stores each row once", () => Effect.gen(function* () {
  const { send, stored, sites } = yield* setup();
  yield* sites();
  const at = Date.now() - HOUR_MS;
  expect((yield* send(pageviews(5, [pageview(at), bots(3)]))).status).toBe(200);
  expect((yield* send(pageviews(9, [pageview(at), pageview(at + 1000), bots(2)]))).status).toBe(200);
  expect((yield* send(pageviews(3, [pageview(at + 2000), bots(10)]))).status).toBe(200);
  expect(yield* stored()).toEqual({
    pageviews: [plainRow(at), plainRow(at + 1000), plainRow(at + 2000)],
    bots: [{ environmentId: PRODUCTION, day: "2026-10-01", reason: "user_agent", count: 5 }],
    gaps: [],
    cursor: [{ stream: "pageviews", seq: 9n }],
  });
}), 60_000);

it.live("rows for an unknown namespace, a site that is off, or a time outside retention are dropped", () => Effect.gen(function* () {
  const { send, stored, sites } = yield* setup();
  yield* sites();
  const now = Date.now();
  const outside = [
    pageview(now, { namespace: "nobody-home" }),
    pageview(now, { namespace: "shop-staging" }),
    bots(4, { namespace: "shop-staging" }),
    { kind: "gap", namespace: "nobody-home", day: null, count: 1 },
    pageview(now - 400 * 24 * HOUR_MS),
    pageview(now + 25 * HOUR_MS),
  ];
  expect((yield* send(pageviews(4, outside))).status).toBe(200);
  expect(yield* stored()).toEqual(nothingStored);
  expect((yield* send(pageviews(5, [...outside, pageview(now - HOUR_MS)]))).status).toBe(200);
  expect(yield* stored()).toEqual({ ...nothingStored, pageviews: [plainRow(now - HOUR_MS)], cursor: [{ stream: "pageviews", seq: 5n }] });
}), 60_000);

it.live("a Cloud with analytics off stores no pageviews and acks no salts", () => Effect.gen(function* () {
  const { send, stored, sites } = yield* setup({});
  yield* sites();
  const answer = yield* send(pageviews(5, [pageview(Date.now() - HOUR_MS), bots(3)]));
  expect(answer.status).toBe(200);
  expect(yield* Effect.promise(() => answer.json())).toEqual({ sites: [] });
  expect(yield* stored()).toEqual(nothingStored);
  const refresh = yield* send(EMPTY);
  expect(refresh.status).toBe(200);
  expect(yield* Effect.promise(() => refresh.json())).toEqual({ sites: [] });
}), 60_000);

it.live("new rows mark their site-local days dirty up to rolled_up_to, and yesterday for a row just after midnight", () => Effect.gen(function* () {
  const { send, sites, rollupState, rolledUp } = yield* setup();
  yield* sites();
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: null, dirty: [] }]);
  const now = Date.now();
  const day = (offset: number) => siteDay(SYDNEY, now, offset);
  const midnight = (offset: number) => saltWindow(day(offset), SYDNEY).start * 1000;
  yield* rolledUp(day(-2), [day(-10)]);
  const body = pageviews(5, [
    pageview(midnight(-3) + 10 * 60_000),
    pageview(midnight(-3) + 12 * HOUR_MS),
    pageview(midnight(-1) + 12 * HOUR_MS),
  ]);
  expect((yield* send(body)).status).toBe(200);
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: day(-2), dirty: [day(-10), day(-4), day(-3), day(-2)] }]);
  yield* rolledUp(day(-2), []);
  expect((yield* send(body)).status).toBe(200);
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: day(-2), dirty: [] }]);
}), 60_000);

it.live("a late row before midnight marks the next day dirty, since it can become the entry of a visit crossing midnight", () => Effect.gen(function* () {
  const { send, sites, rollupState, rolledUp } = yield* setup();
  yield* sites();
  const now = Date.now();
  const day = (offset: number) => siteDay(SYDNEY, now, offset);
  const midnight = (offset: number) => saltWindow(day(offset), SYDNEY).start * 1000;
  yield* rolledUp(day(-1), []);
  expect((yield* send(pageviews(5, [pageview(midnight(-2) - 2 * HOUR_MS)]))).status).toBe(200);
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: day(-1), dirty: [day(-3), day(-2)] }]);
}), 60_000);

it.live("a site that was never rolled up gets no dirty days", () => Effect.gen(function* () {
  const { send, sites, rollupState } = yield* setup();
  yield* sites();
  expect((yield* send(pageviews(5, [pageview(Date.now() - HOUR_MS)]))).status).toBe(200);
  expect(yield* rollupState()).toEqual([{ environmentId: PRODUCTION, rolledUpTo: null, dirty: [] }]);
}), 60_000);

it.live("a pageviews batch with an unknown kind or a malformed row answers 422 and stores nothing", () => Effect.gen(function* () {
  const { send, stored, sites } = yield* setup();
  yield* sites();
  const at = Date.now() - HOUR_MS;
  for (const event of [
    { kind: "click", namespace: "shop-prod" },
    pageview(at, { visitor: "not-hex" }),
    pageview(at, { flags: ["bored"] }),
    pageview(at + 0.5),
    bots(-1),
    { kind: "gap", namespace: "shop-prod", day: "yesterday", count: 1 },
  ]) {
    expect((yield* send(pageviews(5, [pageview(at), event]))).status).toBe(422);
  }
  expect((yield* send(JSON.stringify({ stream: "pageviews", seq: 5, dropped: 0, events: [] }))).status).toBe(422);
  expect(yield* stored()).toEqual(nothingStored);
}), 60_000);

const lockRace = (services: Effect.Success<ReturnType<typeof setup>>["services"]) => {
  const provide = <A, E>(effect: Effect.Effect<A, E, Database | AppConfig>) => effect.pipe(Effect.provide(services), Effect.orDie);
  const lockWaiters = () => provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.execute<{ n: number }>(sql`
      select count(*)::int as n from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'`, "objects");
    return row?.n ?? 0;
  }));
  const untilWaiting = (count: number) => Effect.gen(function* () {
    for (let tries = 0; tries < 200; tries++) {
      if ((yield* lockWaiters()) >= count) return;
      yield* Effect.sleep("50 millis");
    }
    return yield* Effect.die(new Error(`never saw ${count} sessions waiting on a lock`));
  });
  const holdRollupState = (environmentId: string) => provide(Effect.gen(function* () {
    const { database } = yield* AppConfig;
    const client = new Client({ connectionString: database.url.href });
    yield* Effect.promise(() => client.connect());
    yield* Effect.promise(() => client.query("begin"));
    yield* Effect.promise(() => client.query("select 1 from analytics.rollup_state where environment_id = $1 for update", [environmentId]));
    return Effect.promise(async () => {
      await client.query("rollback");
      await client.end();
    });
  }));
  return { provide, lockWaiters, untilWaiting, holdRollupState };
};

const SHOP = "00000000-0000-4000-8000-00000000f207";

/** A project whose environments are created highest id first, each pushing as shop-<index>. */
const shop = (services: Effect.Success<ReturnType<typeof setup>>["services"], environments: ReadonlyArray<string>, enabled: ReadonlyArray<string>) =>
  Effect.gen(function* () {
    const store = yield* cloudStore;
    const [first, ...rest] = [...environments].reverse();
    if (first === undefined) return yield* Effect.die(new Error("a shop needs an environment"));
    yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_project", id: SHOP, name: "shop", default_environment: first }));
    for (const id of rest) {
      yield* Effect.promise(() => store.write(ORGANIZATION, { command: "create_environment", id, project: "shop", name: `env-${id.slice(-4)}` }));
    }
    const { drizzle } = yield* Database;
    for (const [n, id] of environments.entries()) {
      yield* drizzle.execute(sql`insert into config_namespace (organization_id, namespace, environment_id) values (${ORGANIZATION}, ${`shop-${n}`}, ${id})`);
    }
    for (const id of enabled) yield* setSiteAnalytics(ORGANIZATION, id, { enabled: true, timezone: SYDNEY });
  }).pipe(Effect.provide(services), Effect.orDie);

const deleteShop = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`delete from config_project where id = ${SHOP}`);
});

const analyticsLeft = (services: Effect.Success<ReturnType<typeof setup>>["services"]) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ sites: number; salts: number }>(sql`
    select (select count(*)::int from analytics.site) as sites, (select count(*)::int from analytics.salt) as salts`, "objects");
  return row;
}).pipe(Effect.provide(services), Effect.orDie);

const totals = (query: StoreQuery) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const rolled = (yield* dailyTotals(query)).map((row) => ({ ...row, bucket: row.bucket.getTime() }));
  const raw = (yield* drizzle.execute<{ bucket: string; pageviews: string; visitors: string; visits: string; bounces: string; total_duration: string }>(dailyTotalsSql(query, { days: null, edges: [query.range] }), "objects"))
    .map((row) => ({
      bucket: Number(row.bucket), pageviews: Number(row.pageviews), visitors: Number(row.visitors),
      visits: Number(row.visits), bounces: Number(row.bounces), totalDuration: Number(row.total_duration),
    }));
  return { rolled, raw };
});

it.live("a pageview that waits behind a closing rollup marks the closed day dirty, and the rebuild counts it", () => Effect.gen(function* () {
  const { services, send, sites, rollupState } = yield* setup();
  const race = lockRace(services);
  yield* sites();
  const day = siteDay(SYDNEY, Date.now(), -3);
  const midnight = saltWindow(day, SYDNEY).start * 1000;
  expect((yield* send(pageviews(5, [pageview(midnight + 12 * HOUR_MS)]))).status).toBe(200);
  const release = yield* race.holdRollupState(PRODUCTION);
  const rollup = yield* Effect.forkChild(race.provide(closeDays(PRODUCTION, new Date())));
  yield* race.untilWaiting(1);
  const ingest = yield* Effect.forkChild(send(pageviews(6, [pageview(midnight + 13 * HOUR_MS, { visitor: "cd".repeat(32) })])));
  yield* race.untilWaiting(2);
  yield* release;
  expect(yield* Fiber.join(rollup)).toBeGreaterThan(0);
  expect((yield* Fiber.join(ingest)).status).toBe(200);
  expect((yield* rollupState())[0]?.dirty).toContain(day);
  yield* race.provide(rebuildDirtyDays(PRODUCTION));
  const query: StoreQuery = { environmentId: PRODUCTION, timezone: SYDNEY, range: { start: new Date(midnight), end: new Date(saltWindow(siteDay(SYDNEY, Date.now(), -2), SYDNEY).start * 1000) }, filters: [] };
  const { rolled, raw } = yield* race.provide(totals(query));
  expect(rolled).toEqual(raw);
  expect(rolled.map(({ pageviews, visitors }) => ({ pageviews, visitors }))).toEqual([{ pageviews: 2, visitors: 2 }]);
}), 60_000);

it.live("a time zone change that waits behind a closing rollup leaves every rolled day in the new zone", () => Effect.gen(function* () {
  const { services, send, sites } = yield* setup();
  const race = lockRace(services);
  yield* sites();
  const today = Date.UTC(new Date().getUTCFullYear(), new Date().getUTCMonth(), new Date().getUTCDate());
  const evening = (offset: number) => today + offset * 24 * HOUR_MS + 20 * HOUR_MS;
  expect((yield* send(pageviews(5, [-5, -4, -3, -2].map((offset) => pageview(evening(offset)))))).status).toBe(200);
  const release = yield* race.holdRollupState(PRODUCTION);
  const rollup = yield* Effect.forkChild(race.provide(closeDays(PRODUCTION, new Date())));
  yield* race.untilWaiting(1);
  const change = yield* Effect.forkChild(race.provide(setSiteAnalytics(ORGANIZATION, PRODUCTION, { enabled: true, timezone: "UTC" })));
  yield* race.untilWaiting(2);
  yield* release;
  yield* Fiber.join(rollup);
  yield* Fiber.join(change);
  yield* race.provide(closeDays(PRODUCTION, new Date()));
  const query: StoreQuery = { environmentId: PRODUCTION, timezone: "UTC", range: { start: new Date(today - 5 * 24 * HOUR_MS), end: new Date(today) }, filters: [] };
  const { rolled, raw } = yield* race.provide(totals(query));
  expect(rolled).toEqual(raw);
  expect(rolled.filter((row) => row.pageviews > 0).map((row) => row.bucket)).toEqual([-5, -4, -3, -2].map((offset) => today + offset * 24 * HOUR_MS));
}), 60_000);

it.live("an environment deleted while a bot and pageview batch waits on its rollup state leaves nothing behind and deadlocks neither", () => Effect.gen(function* () {
  const { services, send, sites, stored, rollupState } = yield* setup();
  const race = lockRace(services);
  yield* sites();
  expect((yield* send(pageviews(4, [bots(1)]))).status).toBe(200);
  const release = yield* race.holdRollupState(PRODUCTION);
  const forget = yield* Effect.forkChild(race.provide(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`delete from config_environment where id = ${PRODUCTION}`);
  })));
  yield* race.untilWaiting(1);
  const ingest = yield* Effect.forkChild(send(pageviews(5, [bots(3), pageview(Date.now() - HOUR_MS)])));
  yield* race.untilWaiting(2);
  yield* release;
  yield* Fiber.join(forget);
  expect((yield* Fiber.join(ingest)).status).toBe(200);
  expect(yield* stored()).toEqual({ ...nothingStored, cursor: [{ stream: "pageviews", seq: 5n }] });
  expect(yield* rollupState()).toEqual([]);
}), 60_000);

it.live("a project deleted while a bot batch spanning three environments waits on their rollup state deadlocks neither", () => Effect.gen(function* () {
  const { services, send, stored, rollupState } = yield* setup();
  const race = lockRace(services);
  // Postgres hashes these three ids into the order f302, f301, f303, so a delete that drops its id order locks f302
  // before waiting on f301, which the batch takes next and then waits on f302 for.
  const environments = ["00000000-0000-4000-8000-00000000f301", "00000000-0000-4000-8000-00000000f302", "00000000-0000-4000-8000-00000000f303"] as const;
  const [lowest] = environments;
  yield* shop(services, environments, environments);
  const release = yield* race.holdRollupState(lowest);
  const ingest = yield* Effect.forkChild(send(pageviews(5, environments.map((_, n) => bots(n + 1, { namespace: `shop-${n}` })))));
  yield* race.untilWaiting(1);
  const forget = yield* Effect.forkChild(race.provide(deleteShop));
  yield* race.untilWaiting(2);
  yield* release;
  expect((yield* Fiber.join(ingest)).status).toBe(200);
  yield* Fiber.join(forget);
  expect(yield* stored()).toEqual({ ...nothingStored, cursor: [{ stream: "pageviews", seq: 5n }] });
  expect(yield* rollupState()).toEqual([]);
}), 60_000);

it.live("an environment enabled while its project's delete waits on another's rollup state waits for the delete and leaves nothing behind", () => Effect.gen(function* () {
  const { services, send, stored, rollupState } = yield* setup();
  const race = lockRace(services);
  const environments = ["00000000-0000-4000-8000-00000000f301", "00000000-0000-4000-8000-00000000f302"] as const;
  const [low, high] = environments;
  yield* shop(services, environments, [high]);
  const release = yield* race.holdRollupState(high);
  const forget = yield* Effect.forkChild(race.provide(deleteShop));
  yield* race.untilWaiting(1);
  // The enable either finishes now or waits behind the delete; either way the batch below must not deadlock.
  const enable = yield* Effect.forkChild(Effect.exit(setSiteAnalytics(ORGANIZATION, low, { enabled: true, timezone: SYDNEY }).pipe(Effect.provide(services))));
  yield* Effect.raceFirst(Fiber.await(enable), race.untilWaiting(2));
  const waiting = yield* race.lockWaiters();
  const ingest = yield* Effect.forkChild(send(pageviews(5, [bots(3, { namespace: "shop-0" }), bots(2, { namespace: "shop-1" })])));
  yield* race.untilWaiting(waiting + 1);
  yield* release;
  expect((yield* Fiber.join(ingest)).status).toBe(200);
  yield* Fiber.join(forget);
  yield* Fiber.join(enable);
  expect(yield* stored()).toEqual({ ...nothingStored, cursor: [{ stream: "pageviews", seq: 5n }] });
  expect(yield* rollupState()).toEqual([]);
  expect(yield* analyticsLeft(services)).toEqual({ sites: 0, salts: 0 });
}), 60_000);
