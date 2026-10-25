import { ConfigProvider, Effect, Layer } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { readClusterSealing } from "#/modules/sealing/cluster-sealing.server";
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

describe("readClusterSealing", () => {
  let harness: PostgresTestHarness;
  const read = () => harness.runEffect(readClusterSealing(organizationId).pipe(Effect.provideService(SecretEncryption, encryption)));
  // A Server reporting its key reads Cloud's config, which keeps its shipped default here.
  const report = () => harness.runEffect(recordSealingPublicKey(organizationId, clusterKey, publicKey).pipe(
    Effect.provideService(InngestClient, sendsNowhere()),
    Effect.provide(AppConfig.layer.pipe(Layer.provide(ConfigProvider.layer(ConfigProvider.fromEnv({ env: { ...testConfigEnvironment(), DATABASE_URL: harness.databaseUrl } }))))),
  ));
  const pair = () => harness.pool.query(
    `insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
     values ($1, $2, $3, $3)`,
    [organizationId, JSON.stringify(encryption.encrypt(pairingSecret)), "1".repeat(32)],
  );

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
    // The Config Store's tables live in Cloud's database once Cloud opens the Store.
    await Effect.runPromise(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
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
});
