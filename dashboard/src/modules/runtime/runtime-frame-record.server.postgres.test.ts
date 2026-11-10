import type { Client, RuntimeWatchView, Sealing } from "@ployz/sdk";
import { Effect, Layer } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { recordSealingPublicKey } from "#/modules/machines/sealing-key.server";
import { firstRuntimeFrame, OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { runtimeFrameRecords, runtimeSealingDigest } from "#/modules/runtime/runtime-frame-record.server";
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
const joiner = "2".repeat(32);

const held: Sealing = { kind: "held", public_key: publicKey };
const awaitsRecovery: Sealing = { kind: "awaits_recovery", public_key: publicKey };
const needsAccess: Sealing = { kind: "needs_access" };

const frame = (name: string, sealing: Sealing, others: Record<string, Sealing> = {}): RuntimeWatchView =>
  runtimeWatchFrameFixture({
    machines: Object.entries({ [server]: sealing, ...others }).map(([id, state]) => {
      const machine = runtimeWatchMachineFixture(id, id === server ? name : `joiner-${id.slice(0, 1)}`);
      return runtimeWatchMachineObservationFixture({ machine: { ...machine, runtime: { ...machine.runtime, sealing: state } } });
    }),
  });

const inngest = new Inngest({ id: "runtime-frame-record-test" });
const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

describe("runtimeFrameRecord", () => {
  let harness: PostgresTestHarness;
  /** Writes `reported` through both records, as the recorder does for a frame it never saw, and reports the ask. */
  const write = (reported: RuntimeWatchView) => harness.runEffect(runtimeFrameRecords.pipe(
    Effect.flatMap(([names, sealing]) => names.write(organizationId, clusterKey, reported).pipe(
      Effect.andThen(sealing.write(organizationId, clusterKey, reported)),
    )),
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

  it("asks once the founder holds the key, again while the joiner still lacks it, then settles", async () => {
    expect(await write(frame("web-1", awaitsRecovery, { [joiner]: needsAccess }))).toBe("settled");
    expect(send).not.toHaveBeenCalled();
    expect(await write(frame("web-1", held, { [joiner]: needsAccess }))).toBe("asked");
    expect(send).toHaveBeenCalledTimes(1);
    // The ensure job records the key, and shares it with the joiner later.
    await recordKey();
    expect(await write(frame("web-1", held, { [joiner]: needsAccess }))).toBe("asked");
    expect(send).toHaveBeenCalledTimes(2);
    expect(await write(frame("web-1", held, { [joiner]: held }))).toBe("settled");
    expect(send).toHaveBeenCalledTimes(2);
  });

  it("keeps the names it wrote and reports asked when the send fails", async () => {
    send.mockRejectedValueOnce(new Error("Inngest unreachable"));
    expect(await write(frame("web-1", held))).toBe("asked");
    expect(await machineName()).toBe("web-1");
  });

  it("asks from a frame the Organization Runtime reads for an operation, with no watch open", async () => {
    const ployz = makePloyzLayer({
      connect: async () => asTestDouble<Client>()({
        close: async () => {},
        runtime: { watch: async function* () { yield frame("web-1", held); } },
      }),
    });
    const ensureRequested = { name: "sealing-key/ensure.requested", data: { organizationId } };
    await harness.runEffect(firstRuntimeFrame(organizationId).pipe(
      // The write runs off the read, in the Runtime's own scope, which lives on in production.
      Effect.andThen(Effect.promise(() => vi.waitFor(() => expect(send).toHaveBeenCalledWith(ensureRequested)))),
      Effect.provide(OrganizationRuntimeLive.pipe(Layer.provide(ployz))),
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, inngest),
    ));
    expect(await machineName()).toBe("web-1");
  });

  describe("a Server the Cluster stopped listing", () => {
    const departed = (reported: RuntimeWatchView) => harness.runEffect(runtimeFrameRecords.pipe(
      Effect.flatMap(([, , record]) => record.write(organizationId, clusterKey, reported)),
      Effect.provideService(SecretEncryption, encryption),
      Effect.provideService(InngestClient, inngest),
    ));
    const rows = async () => (await harness.pool.query<{ machine_id: string }>(
      "select machine_id from organization_machine where organization_id = $1 order by machine_id",
      [organizationId],
    )).rows.map(({ machine_id }) => machine_id);
    const joined = (minutesAgo: number) => harness.pool.query(
      `insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, updated_at)
       values ($1, $2, $3, $4, now() - $5 * interval '1 minute')`,
      [organizationId, joiner, clusterKey, encryption.encrypt(`ployz1:cloud:${joiner}`), minutesAgo],
    );

    it("loses Cloud's row once its grace passed, as after a removal from an approved device", async () => {
      await joined(10);
      expect(await departed(frame("web-1", held))).toBe("settled");
      expect(await rows()).toEqual([server]);
    });

    it("keeps its row while its grace runs, and asks again later", async () => {
      await joined(1);
      expect(await departed(frame("web-1", held))).toBe("asked");
      expect(await rows()).toEqual([server, joiner]);
    });

    it("keeps the last Server's row: Cloud lets go of a Cluster only through its own removal", async () => {
      await harness.pool.query("update organization_machine set updated_at = now() - interval '10 minutes'");
      expect(await departed(frame("web-1", held, { [joiner]: held }))).toBe("settled");
      await harness.pool.query("delete from organization_machine where machine_id = $1", [joiner]);
      expect(await departed(runtimeWatchFrameFixture({ machines: [
        runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(joiner, "joiner") }),
      ] }))).toBe("settled");
      expect(await rows()).toEqual([server]);
    });
  });

  it("digests a Server's Sealing Key state apart from its name, so a rename alone asks nothing", () => {
    expect(runtimeSealingDigest(frame("web-1", held))).not.toBe(runtimeSealingDigest(frame("web-1", awaitsRecovery)));
    expect(runtimeSealingDigest(frame("web-1", held))).toBe(runtimeSealingDigest(frame("web-2", held)));
  });
});
