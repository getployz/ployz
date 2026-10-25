import type { RuntimeWatchView, Sealing } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { requestSealingKeyEnsure } from "#/modules/machines/sealing-ensure.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { testConfigLayer } from "#/test/config-environment";
import { sendsNowhere } from "#/test/store-cloud";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000c52";
const encryption = makeSecretEncryption("sealing-ensure-test-encryption-secret");
const pairingSecret = "sealing-ensure-test-pairing-secret";
const clusterKey = hashEnrollmentToken(pairingSecret);
const publicKey = `${"A".repeat(43)}=`;

const SEALING = {
  held: { kind: "held", public_key: publicKey },
  needs_access: { kind: "needs_access" },
  awaits_recovery: { kind: "awaits_recovery", public_key: publicKey },
  unknown: null,
} as const satisfies Record<string, Sealing | null>;

const frame = (servers: Record<string, keyof typeof SEALING>): RuntimeWatchView =>
  runtimeWatchFrameFixture({
    machines: Object.entries(servers).map(([id, sealing]) => {
      const machine = runtimeWatchMachineFixture(id, id);
      return runtimeWatchMachineObservationFixture({
        machine: { ...machine, runtime: { ...machine.runtime, sealing: SEALING[sealing] } },
      });
    }),
  });

const inngest = new Inngest({ id: "sealing-ensure-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("requestSealingKeyEnsure", () => {
  let harness: PostgresTestHarness;
  const ensure = (servers: Record<string, keyof typeof SEALING>) => harness.runEffect(
    requestSealingKeyEnsure(organizationId, clusterKey, frame(servers)).pipe(
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, inngest),
    ),
  );
  const pair = () => harness.pool.query(
    `insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
     values ($1, $2, $3, $3)`,
    [organizationId, JSON.stringify(encryption.encrypt(pairingSecret)), "1".repeat(32)],
  );
  const recordKey = (generation = clusterKey) => harness.runEffect(recordSealingPublicKey(organizationId, generation, publicKey).pipe(
    Effect.provideService(InngestClient, sendsNowhere()),
    Effect.provide(testConfigLayer({ DATABASE_URL: harness.databaseUrl })),
  ));
  const ensureRequested = { name: "sealing-key/ensure.requested", data: { organizationId } };

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
    await Effect.runPromise(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    send.mockClear();
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
    await pair();
  });

  it("asks to record the key when the lone Server recovered it and Cloud has none for the Cluster", async () => {
    await ensure({ m1: "held" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(ensureRequested);
  });

  it("asks nothing once Cloud recorded the key and every Server holds it", async () => {
    await recordKey();
    await ensure({ m1: "held", m2: "held" });
    expect(send).not.toHaveBeenCalled();
  });

  it("asks to share the key when one Server holds it and another still lacks it", async () => {
    await recordKey();
    await ensure({ m1: "held", m2: "needs_access" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(ensureRequested);
  });

  it("asks to record the key when Cloud holds one only for an earlier pairing of the same Servers", async () => {
    await recordKey(hashEnrollmentToken("an-earlier-pairing-secret"));
    await ensure({ m1: "held", m2: "held" });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith(ensureRequested);
  });

  it("asks nothing while no Server holds the key", async () => {
    await ensure({ m1: "awaits_recovery", m2: "needs_access", m3: "unknown" });
    expect(send).not.toHaveBeenCalled();
  });
});
