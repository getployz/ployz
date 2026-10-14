import { readFile } from "node:fs/promises";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { mirrorStatuses } from "#/modules/volume-copies/volume-copies.refresh.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-000000000f05";
const machine = (name: string) => name.padEnd(32, "0");
const backfill = new URL("../../../drizzle/20261014082609_volume_mirror_status_backfill/migration.sql", import.meta.url);

describe("the mirror status backfill", () => {
  let harness: PostgresTestHarness;

  const sync = (volumeId: string, state: string, createdAt: string, finishedAt: string | null, epoch: number | null = 1, message: string | null = null) =>
    harness.pool.query(
      `insert into volume_run (organization_id, environment_id, volume_id, volume_name, docker_volume, kind, args, state, created_at, finished_at, epoch, message)
       values ($1, 'env-1', $2, 'data', 'data', 'sync', '{"full":false}', $3, $4, $5, $6, $7)`,
      [organizationId, volumeId, state, createdAt, finishedAt, epoch, message],
    );

  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });
  beforeEach(async () => {
    await harness.pool.query(`
      truncate table organization cascade;
      insert into organization (id, name, slug) values ('${organizationId}', 'Acme', 'acme');
      insert into volume_placement (volume_id, organization_id, environment_id, mirrors) values
        ('vol1', '${organizationId}', 'env-1', '["${machine("web-2")}", "${machine("web-3")}"]'),
        ('vol2', '${organizationId}', 'env-1', '["${machine("web-2")}"]');
    `);
  });

  it("dates each mirror from the Volume's newest done sync, and keeps what the new sync already recorded", async () => {
    await sync("vol1", "done", "2026-10-10T00:00:00Z", "2026-10-10T00:05:00Z");
    await sync("vol1", "done", "2026-10-11T00:00:00Z", "2026-10-11T00:05:00Z");
    await sync("vol1", "failed", "2026-10-12T00:00:00Z", "2026-10-12T00:05:00Z");
    await sync("vol2", "failed", "2026-10-12T00:00:00Z", "2026-10-12T00:05:00Z");
    await harness.pool.query(
      `insert into volume_mirror_status (organization_id, volume_id, machine_id, ok_at, ok_snapshot, matched_at) values ($1, 'vol1', $2, $3, 'w-1-5', $3)`,
      [organizationId, machine("web-3"), "2026-10-13T00:00:00Z"],
    );

    await harness.pool.query(await readFile(backfill, "utf8"));

    const statuses = await harness.runEffect(mirrorStatuses(organizationId, "vol1"));
    expect([...statuses].sort((a, b) => a.machine.localeCompare(b.machine))).toEqual([
      { machine: machine("web-2"), okAt: null, matchedAt: "2026-10-11T00:05:00.000Z", failedAt: null, failure: null },
      { machine: machine("web-3"), okAt: "2026-10-13T00:00:00.000Z", matchedAt: "2026-10-13T00:00:00.000Z", failedAt: null, failure: null },
    ]);
    expect(await harness.runEffect(mirrorStatuses(organizationId, "vol2"))).toEqual([]);
  });

  it("dates a mirror whose only row so far is a failed pull, and keeps the failure", async () => {
    await sync("vol2", "done", "2026-10-11T00:00:00Z", "2026-10-11T00:05:00Z");
    await harness.pool.query(
      `insert into volume_mirror_status (organization_id, volume_id, machine_id, failed_at, failed_snapshot, failure) values ($1, 'vol2', $2, $3, 'w-1-5', 'no space left')`,
      [organizationId, machine("web-2"), "2026-10-13T00:00:00Z"],
    );

    await harness.pool.query(await readFile(backfill, "utf8"));

    expect(await harness.runEffect(mirrorStatuses(organizationId, "vol2"))).toEqual([
      { machine: machine("web-2"), okAt: null, matchedAt: "2026-10-11T00:05:00.000Z", failedAt: "2026-10-13T00:00:00.000Z", failure: "no space left" },
    ]);
  });

  it("passes over a done sync that moved nothing or never recorded its finish", async () => {
    await sync("vol2", "done", "2026-10-11T00:00:00Z", "2026-10-11T00:05:00Z");
    await sync("vol2", "done", "2026-10-12T00:00:00Z", "2026-10-12T00:00:01Z", null, "data has one primary to keep");
    await sync("vol2", "done", "2026-10-13T00:00:00Z", null);

    await harness.pool.query(await readFile(backfill, "utf8"));

    expect(await harness.runEffect(mirrorStatuses(organizationId, "vol2"))).toEqual([
      { machine: machine("web-2"), okAt: null, matchedAt: "2026-10-11T00:05:00.000Z", failedAt: null, failure: null },
    ]);
  });

  it("dates a mirror from a sync that ran before runs carried an epoch", async () => {
    await sync("vol2", "done", "2026-10-11T00:00:00Z", "2026-10-11T00:05:00Z", null);

    await harness.pool.query(await readFile(backfill, "utf8"));

    expect(await harness.runEffect(mirrorStatuses(organizationId, "vol2"))).toEqual([
      { machine: machine("web-2"), okAt: null, matchedAt: "2026-10-11T00:05:00.000Z", failedAt: null, failure: null },
    ]);
  });

  it("dates a mirror the placement lists twice once", async () => {
    await harness.pool.query(`update volume_placement set mirrors = '["${machine("web-2")}", "${machine("web-2")}"]' where volume_id = 'vol2'`);
    await sync("vol2", "done", "2026-10-11T00:00:00Z", "2026-10-11T00:05:00Z");

    await harness.pool.query(await readFile(backfill, "utf8"));

    expect(await harness.runEffect(mirrorStatuses(organizationId, "vol2"))).toEqual([
      { machine: machine("web-2"), okAt: null, matchedAt: "2026-10-11T00:05:00.000Z", failedAt: null, failure: null },
    ]);
  });
});
