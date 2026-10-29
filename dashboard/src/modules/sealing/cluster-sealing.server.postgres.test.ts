import type { RuntimeWatchView } from "@ployz/sdk";
import { Inngest } from "inngest";
import { ConfigProvider, Effect, Layer } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { readClusterSealing, readClusterSealingToSeal } from "#/modules/sealing/cluster-sealing.server";
import { AppConfig } from "#/server/config.server";
import { testConfigEnvironment } from "#/test/config-environment";
import { sendsNowhere } from "#/test/store-cloud";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000c51";
const encryption = makeSecretEncryption("cluster-sealing-test-encryption-secret");
const pairingSecret = "cluster-sealing-test-pairing-secret";
const clusterKey = hashEnrollmentToken(pairingSecret);
const publicKey = `${"A".repeat(43)}=`;
const otherKey = `${"B".repeat(43)}=`;

type Sealing = NonNullable<RuntimeWatchView["machines"][number]["machine"]["runtime"]["sealing"]>;
const held = (key: string): Sealing => ({ kind: "held", public_key: key });
const awaitsRecovery: Sealing = { kind: "awaits_recovery", public_key: publicKey };

const frameOf = (servers: Record<string, Sealing>): RuntimeWatchView => runtimeWatchFrameFixture({
  machines: Object.entries(servers).map(([id, sealing]) => {
    const machine = runtimeWatchMachineFixture(id, id);
    return runtimeWatchMachineObservationFixture({ machine: { ...machine, runtime: { ...machine.runtime, sealing } } });
  }),
});

describe("readClusterSealing", () => {
  let harness: PostgresTestHarness;
  /** What the Cluster answers when Cloud reads it: a frame, unreachable, or never read. */
  let cluster: RuntimeWatchView | "unreachable" | "unread";
  let opened: number;
  const runtime = {
    cancel: () => Effect.void,
    open: () => Effect.sync(() => {
      opened += 1;
      if (cluster === "unread") throw new Error("the Cluster was read");
      return cluster === "unreachable"
        ? { status: "unreachable" as const, error: null }
        : { status: "connected" as const, connected: asTestDouble<PloyzSession>()({ watchFirstFrame: () => Effect.succeed(cluster) }) };
    }),
  };
  const config = () => AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(
    ConfigProvider.fromEnv({ env: { ...testConfigEnvironment(), DATABASE_URL: harness.databaseUrl } }),
  )));
  const read = () => harness.runEffect(readClusterSealing(organizationId).pipe(Effect.provideService(SecretEncryption, encryption)));
  const readToSeal = (inngest = sendsNowhere()) => harness.runEffect(readClusterSealingToSeal(organizationId).pipe(
    Effect.provideService(SecretEncryption, encryption),
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provideService(InngestClient, inngest),
    Effect.provide(config()),
  ));
  // A Server reporting its key reads Cloud's config, which keeps its shipped default here.
  const report = () => harness.runEffect(recordSealingPublicKey(organizationId, clusterKey, publicKey).pipe(
    Effect.provideService(InngestClient, sendsNowhere()),
    Effect.provide(config()),
  ));
  const pair = () => harness.pool.query(
    `insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
     values ($1, $2, $3, $3)`,
    [organizationId, JSON.stringify(encryption.encrypt(pairingSecret)), "1".repeat(32)],
  );
  const reseals = async () => Number((await harness.pool.query(
    `select count(*) from operation_outbox where name = 'variables/reseal.requested'`,
  )).rows[0].count);

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
    // The Config Store's tables live in Cloud's database once Cloud opens the Store.
    await Effect.runPromise(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    cluster = "unread";
    opened = 0;
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  it("has no server while unpaired or before a Server reports a key", async () => {
    expect(await read()).toEqual({ state: "no_server" });
    await pair();
    expect(await read()).toEqual({ state: "no_server" });
  });

  it("is ready with the current pairing's key", async () => {
    await pair();
    await report();
    expect(await read()).toEqual({ state: "ready", publicKey });
  });

  it("to seal, records the key a lone recovered Server holds that Cloud never recorded", async () => {
    await pair();
    cluster = frameOf({ m1: held(publicKey) });
    expect(await readToSeal()).toEqual({ state: "ready", publicKey });
    expect(await read()).toEqual({ state: "ready", publicKey });
    expect(await reseals()).toBe(1);

    expect(await readToSeal()).toEqual({ state: "ready", publicKey });
    expect([opened, await reseals()]).toEqual([1, 1]);
  });

  it("to seal, answers without waiting for Inngest to take the reseal request", async () => {
    await pair();
    cluster = frameOf({ m1: held(publicKey) });
    const stalled = new Inngest({ id: "stalled" });
    vi.spyOn(stalled, "send").mockReturnValue(new Promise(() => {}));
    expect(await readToSeal(stalled)).toEqual({ state: "ready", publicKey });
    expect(await reseals()).toBe(1);
  });

  it("to seal, records the held key while another Server still awaits recovery", async () => {
    await pair();
    cluster = frameOf({ m1: held(publicKey), m2: awaitsRecovery });
    expect(await readToSeal()).toEqual({ state: "ready", publicKey });
  });

  it("to seal, has no server while two Servers hold different keys or the Cluster doesn't answer", async () => {
    await pair();
    for (const answer of [frameOf({ m1: held(publicKey), m2: held(otherKey) }), "unreachable" as const]) {
      cluster = answer;
      expect(await readToSeal()).toEqual({ state: "no_server" });
    }
    expect(await read()).toEqual({ state: "no_server" });
  });

  it("to seal, says the Cluster awaits recovery while a Server does and none holds a key", async () => {
    await pair();
    cluster = frameOf({ m1: awaitsRecovery });
    expect(await readToSeal()).toEqual({ state: "no_server", awaitsRecovery: true });
    expect(await read()).toEqual({ state: "no_server" });
  });

  it("to seal, reads no Cluster while unpaired or once the key is recorded", async () => {
    expect(await readToSeal()).toEqual({ state: "no_server" });
    await pair();
    await report();
    expect(await readToSeal()).toEqual({ state: "ready", publicKey });
  });
});
