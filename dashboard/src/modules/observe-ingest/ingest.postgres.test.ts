import { createPrivateKey, sign } from "node:crypto";
import { it } from "@effect/vitest";
import { asc, sql } from "drizzle-orm";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { organizationMachine } from "#/modules/machines/tables";
import { observePushMessage, verifyObservePush } from "#/modules/observe-ingest/ingest.server";
import { crashEvent, observeIngestCursor } from "#/modules/observe-ingest/tables";
import { operationOutbox } from "#/modules/operations/tables";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { handleObservePush } from "#/routes/api/machines/-handlers";
import { saltWindow, setSiteAnalytics, siteDay } from "#/modules/web-analytics/salts.server";
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

const setup = Effect.fn(function* () {
  const inngest = new Inngest({ id: "observe-ingest-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* Layer.build(yield* storeTestCloud({ inngest }));
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
  return { send, sendRaw, rows, identity, enrollElsewhere, sites, breakSalts, corruptTimezone };
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

it.live("a pageviews push answers 404 until Cloud takes pageviews, and writes nothing", () => Effect.gen(function* () {
  const { send, rows, sites } = yield* setup();
  yield* sites();
  expect((yield* send(JSON.stringify({ stream: "pageviews", seq: "1", dropped: 0, events: [] }))).status).toBe(404);
  expect(yield* rows()).toEqual(nothing);
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
