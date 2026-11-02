import type { BackfillSealingKeyRequest, Client, ConnectOptions } from "@ployz/sdk";
import { Layer, ManagedRuntime } from "effect";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { backfillClusterSealingKey } from "#/modules/machines/sealing-backfill.server";
import { OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { startPostgresTestHarness, type PostgresTestHarness } from "#/test/postgres";
import { sendsNowhere } from "#/test/store-cloud";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000008911";
const pairing = "ppair_fixture_session_cluster";
const encryption = makeSecretEncryption("fixture-session-cluster-encryption-123");
const pairingKey = `${"P".repeat(43)}=`;
const otherClusterKey = `${"X".repeat(43)}=`;
const founder = { machineId: "00000000000000000000000000008911", management: "ployz1:fixture-founder" };
const moved = { machineId: "00000000000000000000000000008912", management: "ployz1:fixture-moved" };

describe("a session's Cluster", () => {
  let harness: PostgresTestHarness;
  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness.stop(); });
  beforeEach(async () => {
    await harness.pool.query("truncate organization cascade");
    await harness.pool.query("insert into organization (id,name,slug) values ($1,'Session','session')", [organizationId]);
    await harness.pool.query(`insert into organization_pairing
      (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id, founder_machine_id)
      values ($1,$2,'founder-key',$3,$3)`, [organizationId, encryption.encrypt(pairing), founder.machineId]);
    await harness.pool.query("insert into cluster_sealing_key (organization_id, cluster_key, public_key) values ($1,$2,$3)",
      [organizationId, hashEnrollmentToken(pairing), pairingKey]);
  });

  async function saveRow(server: typeof founder, dialEntry: boolean) {
    await harness.pool.query(`insert into organization_machine
      (organization_id,machine_id,cluster_key,encrypted_capability,is_dial_entry) values ($1,$2,$3,$4,$5)`,
      [organizationId, server.machineId, hashEnrollmentToken(pairing), encryption.encrypt(server.management), dialEntry]);
  }

  /** Each Server holds its Cluster's key; the connect keeps, as the SDK does, only an entry holding the one it expects. */
  function fixture(holds: Record<string, string>) {
    const asked: string[] = [];
    const dialed: ConnectOptions[] = [];
    const ployz = makePloyzLayer({
      connect: async (options) => {
        dialed.push(options);
        const entry = options.connections.find((connection) => "management" in connection
          && (options.expect === undefined || holds[connection.management] === options.expect.sealingPublicKey));
        if (entry === undefined || !("management" in entry)) throw new Error("No entry in the expected Cluster");
        return asTestDouble<Client>()({
          close: async () => {},
          cluster: () => ({ verified: true, foreign: [] }),
          backfillSealingKey: async (request: BackfillSealingKeyRequest) => {
            asked.push(entry.management);
            return { result: "waiting", request };
          },
        });
      },
    });
    const layer = Layer.mergeAll(ployz, Layer.succeed(Database, harness.database), Layer.succeed(SecretEncryption, encryption),
      Layer.succeed(InngestClient, sendsNowhere()));
    const runtime = ManagedRuntime.make(OrganizationRuntimeLive.pipe(Layer.provideMerge(layer)));
    return { asked, dialed, runtime };
  }

  const request: BackfillSealingKeyRequest = { expected: pairingKey, generator: null };

  it("hands the pairing's Sealing Key only through an entry in its own Cluster, past a Server that moved to another", async () => {
    await saveRow(moved, true);
    await saveRow(founder, false);
    const { asked, dialed, runtime } = fixture({ [founder.management]: pairingKey, [moved.management]: otherClusterKey });
    try {
      await runtime.runPromise(backfillClusterSealingKey(organizationId, request));
      expect(dialed.map((options) => options.expect)).toEqual([{ sealingPublicKey: pairingKey }]);
      expect(asked).toEqual([founder.management]);
    } finally { await runtime.dispose(); }
  });

  it("asks nothing of a Server that moved to another Cluster when no other is left", async () => {
    await saveRow(moved, true);
    const { asked, runtime } = fixture({ [moved.management]: otherClusterKey });
    try {
      await expect(runtime.runPromise(backfillClusterSealingKey(organizationId, request))).rejects.toThrow();
      expect(asked).toEqual([]);
    } finally { await runtime.dispose(); }
  });
});
