import { readFile } from "node:fs/promises";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import {
  readSealingRecovery,
  recordFirstRecoveryBackup,
  recordSealingPublicKey,
  recoveryDigest,
  rotateRecoveryBackup,
} from "#/modules/machines/sealing-key.server";
import { InngestClient } from "#/modules/inngest/client";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { sendsNowhere } from "#/test/store-cloud";
import { makeSecretEncryption, type SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationId = "00000000-0000-4000-8000-000000000d01";
const sealingKey = (letter: string) => `${letter.repeat(43)}=`;
const clusterA = "a".repeat(64);
const clusterB = "b".repeat(64);
const backup = (fill: number) => Buffer.concat([Buffer.from([1]), Buffer.alloc(76, fill)]).toString("base64");
const pairingSecret = "sealing-key-test-pairing-secret";
const currentCluster = hashEnrollmentToken(pairingSecret);

describe("the Organization's Sealing Key", () => {
  let harness: PostgresTestHarness;

  const run = <A, E>(operation: Effect.Effect<A, E, Database | InngestClient | SecretEncryption>) =>
    harness.runEffect(operation.pipe(Effect.provideService(InngestClient, sendsNowhere())));
  const failure = <A, E>(operation: Effect.Effect<A, E, Database | InngestClient | SecretEncryption>) => run(Effect.flip(operation));
  const stored = async () => (await harness.pool.query(
    "select public_key, recovery_blob from organization_sealing_key where organization_id = $1",
    [organizationId],
  )).rows as { public_key: string; recovery_blob: string | null }[];
  const store = (publicKey: string, recoveryBlob: string | null) => harness.pool.query(
    "insert into organization_sealing_key (organization_id, public_key, recovery_blob) values ($1, $2, $3)",
    [organizationId, publicKey, recoveryBlob],
  );
  const pair = () => harness.pool.query(
    `insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
     values ($1, $2, $3, $3)`,
    [organizationId, JSON.stringify(makeSecretEncryption("test-encryption-secret").encrypt(pairingSecret)), "1".repeat(32)],
  );
  const reportCluster = (clusterKey: string, publicKey: string) => harness.pool.query(
    "insert into cluster_sealing_key (organization_id, cluster_key, public_key) values ($1, $2, $3)",
    [organizationId, clusterKey, publicKey],
  );

  beforeAll(async () => {
    harness = await startPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table organization, operation_outbox cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  describe("recorded from a Cluster's report", () => {
    it("keeps the first key an Organization reported", async () => {
      await run(recordSealingPublicKey(organizationId, clusterA, sealingKey("A")));
      await run(recordSealingPublicKey(organizationId, clusterA, sealingKey("B")));

      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: null }]);
    });

    it("takes a later Cluster's key while no backup holds the first", async () => {
      await run(recordSealingPublicKey(organizationId, clusterA, sealingKey("A")));
      await run(recordSealingPublicKey(organizationId, clusterB, sealingKey("B")));

      expect(await stored()).toEqual([{ public_key: sealingKey("B"), recovery_blob: null }]);
    });

    it("keeps a key that has a backup when a later Cluster reports another", async () => {
      await store(sealingKey("A"), backup(2));
      await run(recordSealingPublicKey(organizationId, clusterB, sealingKey("B")));

      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(2) }]);
    });

    it("keeps no Cluster key whose reseal request failed to record, so the next report sends it", async () => {
      const inngest = new Inngest({ id: "sealing-key-sends" });
      const send = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
      const report = () => harness.runEffect(
        recordSealingPublicKey(organizationId, clusterA, sealingKey("A")).pipe(Effect.provideService(InngestClient, inngest)),
      );
      await harness.pool.query(`
        create function refuse_outbox() returns trigger language plpgsql as $$ begin raise exception 'outbox refused'; end $$;
        create trigger refuse_outbox before insert on operation_outbox execute function refuse_outbox();
      `);
      try {
        await expect(report()).rejects.toThrow();
      } finally {
        await harness.pool.query("drop trigger refuse_outbox on operation_outbox; drop function refuse_outbox();");
      }
      expect((await harness.pool.query("select 1 from cluster_sealing_key")).rowCount).toBe(0);

      await report();
      expect(send).toHaveBeenCalledTimes(1);
      expect(send).toHaveBeenCalledWith(expect.objectContaining({ name: "variables/reseal.requested", data: { organizationId } }));
    });
  });

  describe("the first recovery backup", () => {
    it("is stored with the key when none is recorded", async () => {
      expect(await run(recordFirstRecoveryBackup(organizationId, sealingKey("A"), backup(2)))).toBe(recoveryDigest(backup(2)));
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(2) }]);
    });

    it("fills a recorded key that has no backup", async () => {
      await store(sealingKey("A"), null);

      expect(await run(recordFirstRecoveryBackup(organizationId, sealingKey("A"), backup(2)))).toBe(recoveryDigest(backup(2)));
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(2) }]);
    });

    it("accepts the same backup again", async () => {
      await store(sealingKey("A"), backup(2));

      expect(await run(recordFirstRecoveryBackup(organizationId, sealingKey("A"), backup(2)))).toBe(recoveryDigest(backup(2)));
    });

    it("refuses another backup or another key and writes nothing", async () => {
      await store(sealingKey("A"), backup(2));

      expect((await failure(recordFirstRecoveryBackup(organizationId, sealingKey("A"), backup(3))))._tag).toBe("Conflict");
      expect((await failure(recordFirstRecoveryBackup(organizationId, sealingKey("B"), backup(2))))._tag).toBe("Conflict");
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(2) }]);
    });
  });

  describe("reading the recovery backup", () => {
    it("returns the key, the backup and its digest", async () => {
      await store(sealingKey("A"), backup(2));

      expect(await run(readSealingRecovery(organizationId))).toEqual({
        publicKey: sealingKey("A"),
        backup: backup(2),
        digest: recoveryDigest(backup(2)),
      });
    });

    it("returns no digest before a backup is stored", async () => {
      await store(sealingKey("A"), null);

      expect(await run(readSealingRecovery(organizationId))).toEqual({ publicKey: sealingKey("A"), backup: null, digest: null });
    });

    it("is not found without a recorded key", async () => {
      expect((await failure(readSealingRecovery(organizationId)))._tag).toBe("NotFound");
    });

    it("returns the current Cluster's key in place of one no backup holds", async () => {
      await store(sealingKey("A"), null);
      await pair();
      await reportCluster(currentCluster, sealingKey("B"));

      expect(await run(readSealingRecovery(organizationId))).toEqual({ publicKey: sealingKey("B"), backup: null, digest: null });
      expect(await stored()).toEqual([{ public_key: sealingKey("B"), recovery_blob: null }]);
    });

    it("keeps a key that has a backup though the current Cluster reported another", async () => {
      await store(sealingKey("A"), backup(2));
      await pair();
      await reportCluster(currentCluster, sealingKey("B"));

      expect(await run(readSealingRecovery(organizationId))).toMatchObject({ publicKey: sealingKey("A"), backup: backup(2) });
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(2) }]);
    });

    it("keeps a key no backup holds while no current Cluster reported one", async () => {
      await store(sealingKey("A"), null);
      await reportCluster(clusterB, sealingKey("B"));
      expect(await run(readSealingRecovery(organizationId))).toMatchObject({ publicKey: sealingKey("A") });

      await pair();
      expect(await run(readSealingRecovery(organizationId))).toMatchObject({ publicKey: sealingKey("A") });
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: null }]);
    });
  });

  describe("rotating the recovery backup", () => {
    it("replaces the backup the caller last read", async () => {
      await store(sealingKey("A"), backup(2));

      expect(await run(rotateRecoveryBackup(organizationId, sealingKey("A"), recoveryDigest(backup(2)), backup(3))))
        .toEqual({ digest: recoveryDigest(backup(3)) });
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(3) }]);
    });

    it("stores a first backup for a key that has none", async () => {
      await store(sealingKey("A"), null);

      expect(await run(rotateRecoveryBackup(organizationId, sealingKey("A"), null, backup(3))))
        .toEqual({ digest: recoveryDigest(backup(3)) });
    });

    it("succeeds again when the same rotation already landed", async () => {
      await store(sealingKey("A"), backup(2));
      await run(rotateRecoveryBackup(organizationId, sealingKey("A"), recoveryDigest(backup(2)), backup(3)));

      expect(await run(rotateRecoveryBackup(organizationId, sealingKey("A"), recoveryDigest(backup(2)), backup(3))))
        .toEqual({ digest: recoveryDigest(backup(3)) });
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(3) }]);
    });

    it("refuses a stale digest or another key and writes nothing", async () => {
      await store(sealingKey("A"), backup(2));

      expect((await failure(rotateRecoveryBackup(organizationId, sealingKey("A"), recoveryDigest(backup(4)), backup(3))))._tag)
        .toBe("Conflict");
      expect((await failure(rotateRecoveryBackup(organizationId, sealingKey("B"), recoveryDigest(backup(2)), backup(3))))._tag)
        .toBe("Conflict");
      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: backup(2) }]);
    });

    it("is not found without a recorded key", async () => {
      expect((await failure(rotateRecoveryBackup(organizationId, sealingKey("A"), null, backup(3))))._tag).toBe("NotFound");
    });
  });

  describe("the migration's backfill", () => {
    const backfill = async () => {
      const migration = await readFile(
        new URL("../../../drizzle/20261022223005_organization_sealing_key/migration.sql", import.meta.url),
        "utf8",
      );
      const breakpoint = "--> statement-breakpoint";
      await harness.pool.query(migration.slice(migration.lastIndexOf(breakpoint) + breakpoint.length));
    };

    it("records the one key every Cluster reported", async () => {
      await reportCluster(clusterA, sealingKey("A"));
      await reportCluster(clusterB, sealingKey("A"));
      await backfill();

      expect(await stored()).toEqual([{ public_key: sealingKey("A"), recovery_blob: null }]);
    });

    it("skips an Organization whose Clusters reported different keys", async () => {
      await reportCluster(clusterA, sealingKey("A"));
      await reportCluster(clusterB, sealingKey("B"));
      await backfill();

      expect(await stored()).toEqual([]);
    });
  });
});
