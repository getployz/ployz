import type { RuntimeWatchView, Sealing } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { runtimeFrameDigest, runtimeFrameRecord } from "#/modules/runtime/runtime-frame-record.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { testConfigLayer } from "#/test/config-environment";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { sendsNowhere } from "#/test/store-cloud";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000c53";
const encryption = makeSecretEncryption("runtime-frame-record-test-encryption-secret");
const pairingSecret = "runtime-frame-record-test-pairing-secret";
const clusterKey = hashEnrollmentToken(pairingSecret);
const publicKey = `${"A".repeat(43)}=`;
const server = "1".repeat(32);

const held: Sealing = { kind: "held", public_key: publicKey };
const awaitsRecovery: Sealing = { kind: "awaits_recovery", public_key: publicKey };

const frame = (name: string, sealing: Sealing): RuntimeWatchView => {
  const machine = runtimeWatchMachineFixture(server, name);
  return runtimeWatchFrameFixture({
    machines: [runtimeWatchMachineObservationFixture({ machine: { ...machine, runtime: { ...machine.runtime, sealing } } })],
  });
};

const inngest = new Inngest({ id: "runtime-frame-record-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("runtimeFrameRecord", () => {
  let harness: PostgresTestHarness;
  const write = (reported: RuntimeWatchView) => harness.runEffect(runtimeFrameRecord.pipe(
    Effect.flatMap((record) => record.write(organizationId, reported)),
    Effect.provideService(SecretEncryption, encryption),
    Effect.provideService(InngestClient, inngest),
  ));
  const machineName = async () => (await harness.pool.query<{ machine_name: string | null }>(
    "select machine_name from organization_machine where organization_id = $1 and machine_id = $2",
    [organizationId, server],
  )).rows[0]?.machine_name;
  const recordKey = () => harness.runEffect(recordSealingPublicKey(organizationId, clusterKey, publicKey).pipe(
    Effect.provideService(InngestClient, sendsNowhere()),
    Effect.provide(testConfigLayer({ DATABASE_URL: harness.databaseUrl })),
  ));

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
    await harness.pool.query(
      `insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
       values ($1, $2, $3, $3)`,
      [organizationId, JSON.stringify(encryption.encrypt(pairingSecret)), server],
    );
    await harness.pool.query(
      `insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, is_dial_entry)
       values ($1, $2, $3, $4, true)`,
      [organizationId, server, clusterKey, encryption.encrypt(`ployz1:cloud:${server}`)],
    );
  });

  it("names the Server and asks to record the key a lone Server holds while Cloud has none", async () => {
    expect(await write(frame("web-1", held))).toBe("asked");
    expect(await machineName()).toBe("web-1");
    expect(send).toHaveBeenCalledWith({ name: "sealing-key/ensure.requested", data: { organizationId } });
  });

  it("settles once Cloud recorded the key and every Server holds it", async () => {
    await recordKey();
    expect(await write(frame("web-1", held))).toBe("settled");
    expect(await machineName()).toBe("web-1");
    expect(send).not.toHaveBeenCalled();
  });

  it("digests a change in a Server's Sealing Key state alone", () => {
    expect(runtimeFrameDigest(frame("web-1", held))).not.toBe(runtimeFrameDigest(frame("web-1", awaitsRecovery)));
  });
});
