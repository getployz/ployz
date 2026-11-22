import { createPrivateKey, sign } from "node:crypto";
import { it } from "@effect/vitest";
import { asc, sql } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { expect, vi } from "vitest";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { organizationMachine } from "#/modules/machines/tables";
import { observePushMessage } from "#/modules/observe-ingest/ingest.server";
import { verifyManagementSignature } from "#/modules/observe-ingest/machine-signers.server";
import { observeIngestCursor } from "#/modules/observe-ingest/tables";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { handleAnalyticsSalts } from "#/routes/api/machines/-handlers";
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
  expect(verifyManagementSignature(PUBLIC_KEY, message, signature)).toBe(true);
  expect(sign(null, message, privateKey(SEED)).equals(signature)).toBe(true);
  expect(verifyManagementSignature(PUBLIC_KEY, observePushMessage("a".repeat(32), 1_760_000_001n, body), signature)).toBe(false);
  expect(verifyManagementSignature(PUBLIC_KEY, observePushMessage("a".repeat(32), 1_760_000_000n, Buffer.concat([body, Buffer.from(" ")])), signature)).toBe(false);
});

const setup = Effect.fn(function* (env: Readonly<Record<string, string>> = { ANALYTICS_ENABLED: "true" }) {
  const inngest = new Inngest({ id: "observe-ingest-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* storeTestCloud({ inngest, env });
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
    const request = new Request(`https://cloud.example.com/api/machines/${machine}/analytics-salts`, init);
    return handleAnalyticsSalts(request, machine).pipe(Effect.provide(services));
  };
  const sendSalts = (body = "{}", signing: Signing = {}) => sendRaw(body, headersFor(body, signing), signing.machine);
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
    return (yield* drizzle.select().from(observeIngestCursor)).map(({ stream, seq }) => ({ stream, seq }));
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
    yield* drizzle.execute(sql`alter table analytics_site rename to site_gone`);
  }).pipe(Effect.orDie));
  return { services, sendRaw, sendSalts, rows, identity, enrollElsewhere, sites, breakSalts };
});

it.live("a signed salts request answers the salts of the sites that are on, the same on each ask", () => Effect.gen(function* () {
  const { sendSalts, sites } = yield* setup();
  yield* sites();
  const answer = yield* sendSalts();
  expect(answer.status).toBe(200);
  expect(answer.headers.get("content-type")).toBe("application/json");
  const days = [0, 1, 2].map((offset) => siteDay("Australia/Sydney", Date.now(), offset));
  const body = yield* Effect.promise(() => answer.json());
  expect(body).toEqual({
    sites: [{
      namespace: "shop-prod",
      salts: days.map((day) => ({ day, ...saltWindow(day, "Australia/Sydney"), salt: expect.stringMatching(/^[A-Za-z0-9_-]{43}$/) })),
    }],
  });
  const again = yield* sendSalts();
  expect(yield* Effect.promise(() => again.json())).toEqual(body);
}), 60_000);

it.live("a salts request signed by another key answers 401 and issues nothing", () => Effect.gen(function* () {
  const { sendSalts, sites } = yield* setup();
  yield* sites();
  const answer = yield* sendSalts("{}", { seed: OTHER_SEED });
  expect(answer.status).toBe(401);
  expect(yield* Effect.promise(() => answer.text())).not.toContain("salt");
}), 60_000);

it.live("a salts request Cloud cannot issue answers 503, so the Machine keeps the salts it has", () => Effect.gen(function* () {
  const { sendSalts, sites, breakSalts } = yield* setup();
  yield* sites();
  yield* breakSalts();
  expect((yield* sendSalts()).status).toBe(503);
}), 60_000);

const nothing: ReadonlyArray<unknown> = [];
const SALTS_BODY = "{}";

it.live("a salts request whose body changed after signing answers 401 and writes nothing", () => Effect.gen(function* () {
  const { sendSalts, rows } = yield* setup();
  expect((yield* sendSalts(SALTS_BODY, { signed: '{"x":1}' })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a salts request signed more than five minutes away from now answers 401 and writes nothing", () => Effect.gen(function* () {
  const { sendSalts, rows } = yield* setup();
  const now = Math.floor(Date.now() / 1000);
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(now * 1000);
  expect((yield* sendSalts(SALTS_BODY, { at: now - 301 })).status).toBe(401);
  expect((yield* sendSalts(SALTS_BODY, { at: now + 301 })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
  expect((yield* sendSalts(SALTS_BODY, { at: now - 300 })).status).toBe(200);
  expect((yield* sendSalts(SALTS_BODY, { at: now + 300 })).status).toBe(200);
}).pipe(Effect.ensuring(Effect.sync(() => vi.useRealTimers()))), 60_000);

it.live("a salts request with no signature, a malformed one, or a stale timestamp answers 401 before its body or the database is read", () =>
  Effect.gen(function* () {
    const { sendRaw, rows, identity } = yield* setup();
    const signed = headersFor(SALTS_BODY);
    const stale = headersFor(SALTS_BODY, { at: Math.floor(Date.now() / 1000) - 301 });
    const cases: ReadonlyArray<Record<string, string>> = [
      {},
      { "ployz-timestamp": signed["ployz-timestamp"] },
      { "ployz-signature": signed["ployz-signature"] },
      { ...signed, "ployz-signature": signed["ployz-signature"].slice(3) },
      { ...signed, "ployz-timestamp": `${signed["ployz-timestamp"]}.0` },
      stale,
    ];
    for (const headers of cases) {
      const stream = countedStream(Buffer.from(SALTS_BODY), 1);
      expect((yield* sendRaw(stream.body, headers)).status).toBe(401);
      expect(stream.pulled()).toBe(0);
    }
    expect(yield* identity()).toEqual([{ organizationId: ORGANIZATION, managementIdentity: null }]);
    expect(yield* rows()).toEqual(nothing);
  }), 60_000);

const MAX_SALTS_BYTES = 1024;

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

it.live("a signed salts request over 1 KiB answers 422 and writes nothing", () => Effect.gen(function* () {
  const { sendSalts, rows } = yield* setup();
  expect((yield* sendSalts(SALTS_BODY + " ".repeat(MAX_SALTS_BYTES))).status).toBe(422);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("an endless body stops being read at 1 KiB and answers 422", () => Effect.gen(function* () {
  const { sendRaw, rows } = yield* setup();
  const chunk = Buffer.alloc(512, 0x20);
  const stream = countedStream(chunk);
  expect((yield* sendRaw(stream.body, headersFor(""))).status).toBe(422);
  expect(stream.pulled()).toBe(MAX_SALTS_BYTES / chunk.byteLength + 1);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a body whose Content-Length is over 1 KiB answers 422 without being read", () => Effect.gen(function* () {
  const { sendRaw, rows } = yield* setup();
  const stream = countedStream(Buffer.from(SALTS_BODY), 1);
  const answer = yield* sendRaw(stream.body, { ...headersFor(""), "content-length": String(MAX_SALTS_BYTES + 1) });
  expect(answer.status).toBe(422);
  expect(stream.pulled()).toBe(0);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a salts request from a Machine no Organization enrolled answers 401 and writes nothing", () => Effect.gen(function* () {
  const { sendSalts, rows } = yield* setup();
  expect((yield* sendSalts(SALTS_BODY, { machine: machineId("1") })).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a Machine enrolled before Cloud kept its key gets the key from its capability on its first salts request", () => Effect.gen(function* () {
  const { sendSalts, identity } = yield* setup();
  expect(yield* identity()).toEqual([{ organizationId: ORGANIZATION, managementIdentity: null }]);
  expect((yield* sendSalts()).status).toBe(200);
  expect(yield* identity()).toEqual([{ organizationId: ORGANIZATION, managementIdentity: PUBLIC_KEY }]);
}), 60_000);

it.live("a salts request two Organizations' rows would both verify answers 401 and writes nothing", () => Effect.gen(function* () {
  const { sendSalts, rows, enrollElsewhere } = yield* setup();
  yield* enrollElsewhere("00000000-0000-4000-8000-00000000f202");
  expect((yield* sendSalts()).status).toBe(401);
  expect(yield* rows()).toEqual(nothing);
}), 60_000);

it.live("a signed salts request counts as contact from the Machine, on every ask", () => Effect.gen(function* () {
  const { sendSalts, rows } = yield* setup();
  expect((yield* sendSalts()).status).toBe(200);
  expect((yield* sendSalts()).status).toBe(200);
  expect(yield* rows()).toEqual([{ stream: "salts", seq: 0n }]);
}), 60_000);
