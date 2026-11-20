import { createPrivateKey, sign } from "node:crypto";
import { it } from "@effect/vitest";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterEach, expect, vi } from "vitest";
import { organizationMachine } from "#/modules/machines/tables";
import { organization } from "#/modules/organization/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { handleMachineOtlp } from "#/routes/api/machines/-handlers";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { machineId } from "#/test/ids";
import { enrollStoreServer, seedStoreOrganization, storeTestCloud } from "#/test/store-cloud";

const ORGANIZATION = "00000000-0000-4000-8000-00000000f301";
const OTHER_ORGANIZATION = "00000000-0000-4000-8000-00000000f302";
const MACHINE = machineId("0");
const SEED = "07".repeat(32);
const PUBLIC_KEY = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const OTHER_SEED = "08".repeat(32);
const COLLECTOR = "http://observe-collector:4318";

const capability = (publicKey: string) => `ployz1:${Buffer.concat([Buffer.from(publicKey, "hex"), Buffer.alloc(32, 9)]).toString("base64url")}`;
const privateKey = (seed: string) =>
  createPrivateKey({ key: Buffer.from(`302e020100300506032b657004220420${seed}`, "hex"), format: "der", type: "pkcs8" });

type Minting = { readonly machine?: string; readonly seed?: string; readonly iat?: number; readonly exp?: number };

const mint = ({ machine = MACHINE, seed = SEED, iat = Math.floor(Date.now() / 1000), exp = iat + 3600 }: Minting = {}) => {
  const payload = Buffer.from(JSON.stringify({ machine, iat, exp })).toString("base64url");
  const signature = sign(null, Buffer.concat([Buffer.from("ployz-otlp-v1\0"), Buffer.from(payload)]), privateKey(seed));
  return `Bearer ployz-otlp-v1.${payload}.${signature.toString("base64url")}`;
};

it("the known-answer token is what M1 mints: these exact bytes verify under the vector's key", () => {
  const authorization = "Bearer ployz-otlp-v1.eyJtYWNoaW5lIjoiYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWEiLCJpYXQiOjE3NjAwMDAwMDAsImV4cCI6MTc2MDAwMzYwMH0"
    + ".tvurJ9YIYWfDYv2nWDQXUC3ReUY_Gudf5AP6xl-I5Ss4LSxig5XCSahPtYYTdMrLhU0TxS301gunh2hRzbFqAQ";
  expect(mint({ machine: "a".repeat(32), iat: 1_760_000_000, exp: 1_760_003_600 })).toBe(authorization);
});

const body = JSON.stringify({ resourceLogs: [] });

const setup = Effect.fn(function* (env: Readonly<Record<string, string>> = { OBSERVE_COLLECTOR_URL: COLLECTOR }) {
  const inngest = new Inngest({ id: "observe-otlp-test" });
  vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
  const services = yield* storeTestCloud({ inngest, env });
  yield* seedStoreOrganization(ORGANIZATION, { name: "Acme", slug: "acme" }).pipe(Effect.provide(services));
  yield* enrollStoreServer(ORGANIZATION).pipe(Effect.provide(services));
  // The fixture's capability is not a real one; give the Machine the vector's key.
  yield* Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.update(organizationMachine).set({ encryptedCapability: encryption.encrypt(capability(PUBLIC_KEY)), managementIdentity: PUBLIC_KEY });
  }).pipe(Effect.provide(services), Effect.orDie);

  const collector = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("{}", { status: 200, headers: { "content-type": "application/json" } }));
  const post = (authorization: string | undefined, headers: Record<string, string> = {}, signal: "logs" | "metrics" | "traces" = "logs") => {
    const sent = new Headers({ "content-type": "application/json", ...headers });
    if (authorization !== undefined) sent.set("authorization", authorization);
    const request = new Request(`https://cloud.example.com/api/machines/otlp/v1/${signal}`, { method: "POST", body, headers: sent });
    return handleMachineOtlp(request, signal).pipe(Effect.provide(services));
  };
  const enrollElsewhere = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const encryption = yield* SecretEncryption;
    yield* drizzle.insert(organization).values({ id: OTHER_ORGANIZATION, name: "Other", slug: "other" });
    yield* drizzle.insert(organizationPairing).values({
      organizationId: OTHER_ORGANIZATION, encryptedPairingSecret: encryption.encrypt("ppair_other"), founderPublicKey: "founder-key", founderClaimMachineId: MACHINE,
    });
    yield* drizzle.insert(organizationMachine).values({
      organizationId: OTHER_ORGANIZATION, machineId: MACHINE, clusterKey: "e".repeat(64), encryptedCapability: encryption.encrypt(capability(PUBLIC_KEY)),
      managementIdentity: PUBLIC_KEY, isDialEntry: true,
    });
  }).pipe(Effect.provide(services), Effect.orDie);
  const removeMachine = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.delete(organizationMachine).where(eq(organizationMachine.machineId, MACHINE));
  }).pipe(Effect.provide(services), Effect.orDie);
  const seenAt = Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select({ seenAt: organizationMachine.observeSeenAt }).from(organizationMachine).where(eq(organizationMachine.machineId, MACHINE));
    return row?.seenAt ?? null;
  }).pipe(Effect.provide(services), Effect.orDie);
  return { post, collector, enrollElsewhere, removeMachine, seenAt };
});

afterEach(() => vi.restoreAllMocks());

it.live("a verified Machine's batch reaches the Collector stamped with its Organization, whatever the sender claims", () => Effect.gen(function* () {
  const { post, collector } = yield* setup();
  const answer = yield* post(mint(), { "x-ployz-org": "someone-else", "x-ployz-retention-days": "9999", "x-ployz-machine": "spoof" }, "traces");
  expect(answer.status).toBe(200);
  expect(collector).toHaveBeenCalledOnce();
  const [url, init] = collector.mock.calls[0] ?? [];
  expect(url).toBe(`${COLLECTOR}/v1/traces`);
  expect(Object.fromEntries(new Headers(init?.headers))).toEqual({
    "content-type": "application/json",
    "x-ployz-org": ORGANIZATION,
    "x-ployz-retention-days": "30",
    "x-ployz-machine": MACHINE,
  });
}), 60_000);

it.live("records when a Machine's batch was last accepted, and not for one Cloud refused", () => Effect.gen(function* () {
  const { post, collector, seenAt } = yield* setup();
  // Each answer is read once, so each batch gets its own.
  collector.mockImplementation(async () => new Response("{}", { status: 200, headers: { "content-type": "application/json" } }));
  expect(yield* seenAt).toBeNull();
  expect((yield* post("Bearer nonsense")).status).toBe(401);
  collector.mockResolvedValueOnce(new Response("bad", { status: 400 }));
  expect((yield* post(mint())).status).toBe(400);
  expect(yield* seenAt).toBeNull();
  const before = Date.now();
  expect((yield* post(mint())).status).toBe(200);
  const first = yield* seenAt;
  expect(first?.getTime()).toBeGreaterThanOrEqual(before - 1_000);
  // Within thirty seconds a second batch writes nothing, so a busy Machine costs no row update per batch.
  expect((yield* post(mint())).status).toBe(200);
  expect((yield* seenAt)?.getTime()).toBe(first?.getTime());
}), 60_000);

it.live("a token another Machine's key signed, an expired one, one from the future, and a malformed one get 401 and reach no Collector", () => Effect.gen(function* () {
  const { post, collector } = yield* setup();
  const now = Math.floor(Date.now() / 1000);
  const refused = [
    mint({ seed: OTHER_SEED }),
    mint({ iat: now - 7200, exp: now - 3600 }),
    mint({ iat: now + 3600, exp: now + 7200 }),
    mint({ iat: now, exp: now + 90_000 }),
    mint().slice(0, -2),
    "Bearer nonsense",
    undefined,
  ];
  for (const authorization of refused) expect((yield* post(authorization)).status).toBe(401);
  expect(collector).not.toHaveBeenCalled();
}), 60_000);

it.live("a Machine the Organization removed, and one held by two Organizations, get 401", () => Effect.gen(function* () {
  const { post, collector, enrollElsewhere, removeMachine } = yield* setup();
  yield* enrollElsewhere;
  expect((yield* post(mint())).status).toBe(401);
  yield* removeMachine;
  expect((yield* post(mint())).status).toBe(401);
  expect(collector).not.toHaveBeenCalled();
}), 60_000);

it.live("a Cloud with no Collector, one that is down, and one that refuses the body answer 503, 503 and 400", () => Effect.gen(function* () {
  const none = yield* setup({});
  expect((yield* none.post(mint())).status).toBe(503);
  const { post, collector } = yield* setup();
  collector.mockRejectedValueOnce(new TypeError("fetch failed"));
  expect((yield* post(mint())).status).toBe(503);
  collector.mockResolvedValueOnce(new Response("bad", { status: 400 }));
  expect((yield* post(mint())).status).toBe(400);
}), 60_000);
