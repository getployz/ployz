import { randomUUID } from "node:crypto";
import { afterAll, beforeAll, expect, it } from "vitest";
import type { CollectionRead } from "./read.contract";
import { readCollection } from "./read.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = randomUUID();
const userId = randomUUID();
const projectId = randomUUID();
const [production, staging] = [randomUUID(), randomUUID()];
const saved = { [production]: randomUUID(), [staging]: randomUUID() };

let harness: PostgresTestHarness;
const sql = (text: string, values: unknown[] = []) => harness.pool.query(text, values);
const read = (since?: string) => harness.runEffect(readCollection({ userId }, { table: "environment_deployment", userId, organizationSlug: "acme", since })) as Promise<CollectionRead<{ id: string }>>;
/** Inserts an attempt created `minute` minutes into the day and returns its id. */
async function attempt(environmentId: string, minute: number, status = "applied") {
  const id = randomUUID();
  await sql(`insert into environment_deployment (id, organization_id, environment_id, trigger_origin, saved_state_snapshot_id, status, created_at)
    values ($1, $2, $3, '{}', $4, $5, timestamptz '2026-09-01' + make_interval(mins => $6))`,
  [id, organizationId, environmentId, saved[environmentId], status, minute]);
  return id;
}

beforeAll(async () => {
  // Its own server: the change log's xid horizon is cluster-wide, so other files' open transactions would hold back the delta read.
  harness = await startPostgresTestHarness({ ownServer: true });
  await sql("insert into organization (id, name, slug) values ($1, 'Acme', 'acme')", [organizationId]);
  await sql("insert into \"user\" (id, email, name) values ($1, 'owner@example.test', 'Owner')", [userId]);
  await sql("insert into member (user_id, organization_id, role) values ($1, $2, 'owner')", [userId, organizationId]);
  await sql("insert into project (id, organization_id, name, slug) values ($1, $2, 'Shop', 'shop')", [projectId, organizationId]);
  for (const environmentId of [production, staging]) {
    await sql("insert into environment (id, organization_id, project_id, name, namespace, intent) values ($1, $2, $3, $4, $4, '{}')",
      [environmentId, organizationId, projectId, environmentId === production ? "production" : "staging"]);
    await sql("insert into environment_saved_state_snapshot (id, organization_id, environment_id, actor_id, intent, volume_deletion_authorizations) values ($1, $2, $3, $4, '{}', '[]')",
      [saved[environmentId], organizationId, environmentId, userId]);
  }
}, 60_000);
afterAll(async () => { await harness?.stop(); });

it("holds active attempts plus the latest per Environment, however many attempts exist", async () => {
  for (let minute = 0; minute < 30; minute += 1) await attempt(production, minute, minute % 2 ? "applied" : "failed");
  const running = await attempt(production, 30, "planning");
  const queued = await attempt(production, 31, "queued");
  await attempt(staging, 0);
  const stagingLatest = await attempt(staging, 1);

  const full = await read();
  expect(full.rows.map((row) => row.id).sort()).toEqual([running, queued, stagingLatest].sort());

  // A delta read returns the newer attempt; the one it displaced stays in the browser until the next full read.
  const newer = await attempt(staging, 2);
  const delta = await read(full.cursor);
  expect(delta).toMatchObject({ full: false, deleted: [] });
  expect(delta.rows.map((row) => row.id)).toEqual([newer]);
});
