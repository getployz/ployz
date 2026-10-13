import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { offsiteCopies, projectBuckets, saveBucket, saveVolumeOffsite, sealedBucket, volumeOffsiteRow } from "./offsite-bucket.server";

const organizationId = "00000000-0000-4000-8000-000000000f10";
const SECRET = "hunter2-offsite-secret";
const bucket = {
  endpoint: "https://s3.eu-central-1.amazonaws.com",
  region: "eu-central-1",
  bucket: "acme-offsite",
  prefix: "ployz",
  access_key: "AKIAEXAMPLE",
  secret_key: SECRET,
};
const LEAK = new RegExp(`${SECRET}|secret_key|encryptedSecretKey|ciphertext`, "i");

describe("offsite bucket", () => {
  let harness: PostgresTestHarness;

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
    `);
  });

  it("stores the secret key encrypted, never as typed", async () => {
    await harness.runEffect(saveBucket(organizationId, "shop", bucket));
    const { rows } = await harness.pool.query(`select * from offsite_bucket`);
    expect(rows).toHaveLength(1);
    expect(JSON.stringify(rows[0])).not.toContain(SECRET);
    expect(rows[0].encrypted_secret_key).toBeTruthy();
  });

  it("never selects the secret into anything a browser reads", async () => {
    const saved = await harness.runEffect(saveBucket(organizationId, "shop", bucket));
    await harness.runEffect(saveVolumeOffsite(organizationId, "env-1", "vol-1", saved.id));
    const reads = [
      saved,
      await harness.runEffect(projectBuckets(organizationId, "shop")),
      await harness.runEffect(offsiteCopies(organizationId, "env-1")),
      await harness.runEffect(volumeOffsiteRow(organizationId, "vol-1")),
    ];
    expect(JSON.stringify(reads)).not.toMatch(LEAK);
    expect(saved).toMatchObject({ bucket: "acme-offsite", accessKey: "AKIAEXAMPLE", secretSet: true });
  });

  it("decrypts the secret only for a request to a Machine", async () => {
    const saved = await harness.runEffect(saveBucket(organizationId, "shop", bucket));
    expect(await harness.runEffect(sealedBucket(organizationId, saved.id))).toEqual(bucket);
  });

  it("replaces the keys when the same bucket is typed again", async () => {
    await harness.runEffect(saveBucket(organizationId, "shop", bucket));
    const again = await harness.runEffect(saveBucket(organizationId, "shop", { ...bucket, secret_key: "rotated" }));
    expect(await harness.runEffect(projectBuckets(organizationId, "shop"))).toHaveLength(1);
    expect((await harness.runEffect(sealedBucket(organizationId, again.id))).secret_key).toBe("rotated");
  });
});
