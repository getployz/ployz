import { inspect } from "node:util";
import { it } from "@effect/vitest";
import { CipherSuite } from "@hpke/core";
import { Chacha20Poly1305 } from "@hpke/chacha20poly1305";
import { DhkemX25519HkdfSha256, HkdfSha256 } from "@hpke/dhkem-x25519";
import { InngestTestEngine } from "@inngest/test";
import type { ConfigCommand, SecretCiphertext } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { type Context, Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, vi } from "vitest";
import { callStore } from "#/modules/config-store/config-store.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { clusterSealingKey } from "#/modules/machines/tables";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { Database } from "#/server/database.server";
import { makeInngestEffectRunner } from "#/server/run.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { enrollStoreServer, seedStoreOrganization, type StoreCloud, storeTestCloud } from "#/test/store-cloud";
import { encryptSecretValue } from "#/utils/encrypted-secret.server";
import { createResealVariables, requestResealAtStart } from "./reseal.inngest";

const ORGANIZATION = "00000000-0000-4000-8000-0000000e5001";
const SHOP = { project: "00000000-0000-4000-8000-0000000e5010", environment: "00000000-0000-4000-8000-0000000e5011", service: "00000000-0000-4000-8000-0000000e5012" };
const BLOG = { project: "00000000-0000-4000-8000-0000000e5020", environment: "00000000-0000-4000-8000-0000000e5021", service: "00000000-0000-4000-8000-0000000e5022" };
const PAIRING_SECRET = "ppair_fixture_store";
const STORE_SECRET = testConfigEnvironment().APP_ENCRYPTION_SECRET;
const PLAINTEXT = { DATABASE_URL: "postgres://shop:hunter2@db/shop", STRIPE_KEY: "sk_test_resealed" };

const suite = new CipherSuite({ kem: new DhkemX25519HkdfSha256(), kdf: new HkdfSha256(), aead: new Chacha20Poly1305() });
const encoder = new TextEncoder();

interface StoredVariable { readonly key: string; readonly valueFingerprint: string; readonly value: { readonly kind: string; readonly encryptedValue: SecretCiphertext } }

/** A throwaway Cluster key pair: the public half as Cloud records it. */
const clusterKeyPair = Effect.promise(async () => {
  const pair = await suite.kem.generateKeyPair();
  return { privateKey: pair.privateKey, publicKey: Buffer.from(await suite.kem.serializePublicKey(pair.publicKey)).toString("base64") };
});

/** Organization Shop with Projects shop and blog, each one Environment whose `web` holds the two Legacy secrets in Working State. */
const legacyOrganization = Effect.gen(function* () {
  yield* seedStoreOrganization(ORGANIZATION);
  const store = yield* cloudStore;
  const write = (command: ConfigCommand) => Effect.promise(() => store.write(ORGANIZATION, command));
  for (const [name, ids] of [["shop", SHOP], ["blog", BLOG]] as const) {
    yield* write({ command: "create_project", id: ids.project, name, default_environment: ids.environment });
    yield* write({ command: "create_service", id: ids.service, environment: { project: name, environment: null }, name: "web", image: "nginx:1" });
    yield* plantLegacy(ids.environment);
  }
});

const plantLegacy = Effect.fn(function* (environment: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ working: string }>(sql`select working from config_environment where id = ${environment}`, "objects");
  const working = JSON.parse(row?.working ?? "") as { services: Array<{ variables: unknown[] }> };
  working.services[0]?.variables.push(...Object.entries(PLAINTEXT).map(([key, value]) => ({
    id: crypto.randomUUID(), key, description: null, exported: false, valueFingerprint: "s1:old",
    value: { kind: "secret", encryptedValue: encryptSecretValue(STORE_SECRET, value) },
  })));
  yield* drizzle.execute(sql`update config_environment set working = ${JSON.stringify(working)} where id = ${environment}`);
});

const secretsIn = Effect.fn(function* (environment: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ working: string }>(sql`select working from config_environment where id = ${environment}`, "objects");
  const working = JSON.parse(row?.working ?? "") as { services: Array<{ id: string; variables: StoredVariable[] }> };
  return working.services.flatMap((service) => service.variables.map((variable) => ({ service: service.id, ...variable })));
});

/** ployz-core's `SealedValue::associated_data` for `key` of `service` in `environment`. */
const associatedData = (environment: string, service: string, key: string) => {
  const uuid = (id: string) => Buffer.from(id.replaceAll("-", ""), "hex");
  const fields = [encoder.encode("ployz-sealed-v1"), encoder.encode(ORGANIZATION), uuid(environment), encoder.encode("service"), uuid(service), encoder.encode(key)];
  return Buffer.concat(fields.flatMap((field) => {
    const length = Buffer.alloc(4);
    length.writeUInt32BE(field.length);
    return [length, field];
  }));
};

/** Every secret in `environment`, opened with the Cluster's private key as ployzd opens it. */
const openedIn = Effect.fn(function* (environment: string, privateKey: CryptoKey) {
  const opened: Record<string, string> = {};
  for (const variable of yield* secretsIn(environment)) {
    const sealed = variable.value.encryptedValue;
    if (!("sealedFor" in sealed)) throw new Error(`${variable.key} is still Legacy`);
    expect(sealed.sealedFor).toBe(environment);
    const bytes = Buffer.from(sealed.value.sealed, "base64");
    const plaintext = yield* Effect.promise(() => suite.open(
      { recipientKey: privateKey, enc: bytes.subarray(0, 32), info: encoder.encode("ployz-sealed-v1") },
      bytes.subarray(32),
      associatedData(environment, variable.service, variable.key),
    ));
    expect(variable.valueFingerprint).toMatch(/^c1:[0-9a-f]{64}$/);
    opened[variable.key] = new TextDecoder().decode(plaintext);
  }
  return opened;
});

/** Effect's default logger writes every level through `console.log`, naming the level before the message. */
const logged = () => {
  const spy = vi.spyOn(console, "log");
  return {
    lines: (level: "INFO" | "WARN") => spy.mock.calls.map((args) => inspect(args, { depth: null })).filter((line) => line.includes(` ${level} `)),
    [Symbol.dispose]: () => spy.mockRestore(),
  };
};

const resealJob = (
  services: Context.Context<StoreCloud>,
  steps: ConstructorParameters<typeof InngestTestEngine>[0]["steps"] = [],
  data: { organizationId?: string } = { organizationId: ORGANIZATION },
) => {
  const runner: Parameters<typeof createResealVariables>[1] =
    makeInngestEffectRunner((program) => Effect.runPromise(program.pipe(Effect.provide(services))));
  return Effect.promise(() => new InngestTestEngine({
    function: createResealVariables(new Inngest({ id: "reseal-test" }), runner),
    events: [{ name: "variables/reseal.requested", data }],
    steps,
  }).execute());
};

const ready = Effect.fn(function* (publicKey: string) {
  yield* enrollStoreServer(ORGANIZATION);
  yield* recordSealingPublicKey(ORGANIZATION, hashEnrollmentToken(PAIRING_SECRET), publicKey);
});

describe("variables/reseal.requested", () => {
  it.live("resumes after a crash at the next Environment and seals every Legacy value exactly once", () => Effect.gen(function* () {
    const services = yield* storeTestCloud();
    yield* Effect.gen(function* () {
      const { privateKey, publicKey } = yield* clusterKeyPair;
      yield* legacyOrganization;
      yield* ready(publicKey);

      const crashed = yield* resealJob(services, [{ id: `reseal-${BLOG.environment}`, handler: () => { throw new Error("the worker died"); } }]);
      expect(crashed.error).toBeDefined();
      expect(yield* openedIn(SHOP.environment, privateKey)).toEqual(PLAINTEXT);
      const shopAfterCrash = yield* secretsIn(SHOP.environment);
      expect((yield* secretsIn(BLOG.environment)).map((variable) => "version" in variable.value.encryptedValue)).toEqual([true, true]);

      using logs = logged();
      const resumed = yield* resealJob(services);
      expect(resumed.error).toBeUndefined();
      expect(yield* secretsIn(SHOP.environment)).toEqual(shopAfterCrash);
      expect(yield* openedIn(BLOG.environment, privateKey)).toEqual(PLAINTEXT);
      expect(logs.lines("INFO").filter((line) => line.includes("0 legacy values and 0 history values"))).toHaveLength(1);
    }).pipe(Effect.provide(services));
  }), 120_000);

  it.live("reseals nothing on a second run", () => Effect.gen(function* () {
    const services = yield* storeTestCloud();
    yield* Effect.gen(function* () {
      const { privateKey, publicKey } = yield* clusterKeyPair;
      yield* legacyOrganization;
      yield* ready(publicKey);

      expect((yield* resealJob(services)).error).toBeUndefined();
      const sealed = [...yield* secretsIn(SHOP.environment), ...yield* secretsIn(BLOG.environment)];

      using logs = logged();
      const again = yield* resealJob(services);
      expect(again.error).toBeUndefined();
      expect(again.result).toEqual({ resealed: 0, current: 0, history: 0 });
      expect([...yield* secretsIn(SHOP.environment), ...yield* secretsIn(BLOG.environment)]).toEqual(sealed);
      expect(yield* openedIn(SHOP.environment, privateKey)).toEqual(PLAINTEXT);
      expect(logs.lines("INFO").filter((line) => line.includes("0 legacy values and 0 history values"))).toHaveLength(1);
    }).pipe(Effect.provide(services));
  }), 120_000);

  it.live("waits for a Server, then reseals once the Cluster reports its key", () => Effect.gen(function* () {
    const inngest = new Inngest({ id: "reseal-test-sends" });
    const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
    const services = yield* storeTestCloud({ inngest });
    yield* Effect.gen(function* () {
      const { privateKey, publicKey } = yield* clusterKeyPair;
      yield* legacyOrganization;
      yield* enrollStoreServer(ORGANIZATION);

      using logs = logged();
      const waiting = yield* resealJob(services);
      expect(waiting.error).toBeUndefined();
      expect(logs.lines("INFO").filter((line) => line.includes("awaiting a Server"))).toHaveLength(1);
      expect((yield* secretsIn(SHOP.environment)).every((variable) => "version" in variable.value.encryptedValue)).toBe(true);

      yield* recordSealingPublicKey(ORGANIZATION, hashEnrollmentToken(PAIRING_SECRET), publicKey);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "variables/reseal.requested", data: { organizationId: ORGANIZATION } }));
      yield* recordSealingPublicKey(ORGANIZATION, hashEnrollmentToken(PAIRING_SECRET), publicKey);
      expect(send).toHaveBeenCalledTimes(1);

      expect((yield* resealJob(services)).error).toBeUndefined();
      expect(yield* openedIn(SHOP.environment, privateKey)).toEqual(PLAINTEXT);
      expect(yield* openedIn(BLOG.environment, privateKey)).toEqual(PLAINTEXT);
    }).pipe(Effect.provide(services));
  }), 120_000);

  it.live("an install whose Cluster reported its key before an upgrade reseals once a worker starts, and a Deploy waits for it", () => Effect.gen(function* () {
    const inngest = new Inngest({ id: "reseal-test-upgrade" });
    const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
    const services = yield* storeTestCloud({ inngest });
    yield* Effect.gen(function* () {
      const { privateKey, publicKey } = yield* clusterKeyPair;
      yield* legacyOrganization;
      yield* enrollStoreServer(ORGANIZATION);
      // As v0.3.4 recorded it: the key, and no reseal request.
      const { drizzle } = yield* Database;
      yield* drizzle.insert(clusterSealingKey).values({ organizationId: ORGANIZATION, clusterKey: hashEnrollmentToken(PAIRING_SECRET), publicKey });
      yield* recordSealingPublicKey(ORGANIZATION, hashEnrollmentToken(PAIRING_SECRET), publicKey);
      expect(send).not.toHaveBeenCalled();

      const refused = yield* callStore(ORGANIZATION, null, { operation: "write", command: {
        command: "admit", admit: "deploy", id: crypto.randomUUID(), environment: { project: "shop", environment: null },
        services: [], version: null, accept_volume_loss: [],
      } });
      if (refused.ok) return expect.unreachable("a Deploy that reads a Legacy value waits for the reseal");
      expect(refused.refusal.code).toBe("invalid_argument");
      expect(refused.refusal.message).toContain("reseal");
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "variables/reseal.requested", data: { organizationId: ORGANIZATION } }));

      yield* requestResealAtStart(new Date());
      expect(send).toHaveBeenLastCalledWith(expect.objectContaining({ name: "variables/reseal.requested", data: {} }));
      const fanOut = yield* resealJob(services, [], {});
      expect(fanOut.ctx.step.sendEvent).toHaveBeenCalledWith("reseal-organizations", [expect.objectContaining({ data: { organizationId: ORGANIZATION } })]);
      expect((yield* resealJob(services)).error).toBeUndefined();
      expect(yield* openedIn(SHOP.environment, privateKey)).toEqual(PLAINTEXT);
      expect(yield* openedIn(BLOG.environment, privateKey)).toEqual(PLAINTEXT);
    }).pipe(Effect.provide(services));
  }), 120_000);
});
