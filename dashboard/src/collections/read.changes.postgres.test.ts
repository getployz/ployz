import { randomUUID } from "node:crypto";
import { QueryClient } from "@tanstack/react-query";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { collectionsOf, changeNameSources } from "./change-sources";
import { pruneChangeLog, readChangeWindow } from "#/modules/organization/change-log.server";
import { readCollection } from "./read.server";
import type { CollectionName, CollectionRead } from "./read.contract";
import { orgStoreTableNames } from "#/test/org-store-tables";
import { orgStoreTables } from "./collections";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";

type ServiceRow = { id: string; name: string; organizationId: string };
type Organization = { id: string; slug: string; userId: string; projectId: string; environmentId: string };

describe("incremental Service reads from the Organization change log", () => {
  let harness: GithubPostgresTestHarness;
  let alpha: Organization;
  let beta: Organization;

  const sql = (text: string, values: unknown[] = []) => harness.pool.query(text, values);

  async function createOrganization(slug: string): Promise<Organization> {
    const organization = { id: randomUUID(), slug, userId: randomUUID(), projectId: randomUUID(), environmentId: randomUUID() };
    await sql("insert into organization (id, name, slug) values ($1, $2, $2)", [organization.id, slug]);
    await sql("insert into \"user\" (id, email, name) values ($1, $2, $2)", [organization.userId, `${slug}@example.test`]);
    await sql("insert into member (user_id, organization_id, role) values ($1, $2, 'owner')", [organization.userId, organization.id]);
    await sql("insert into project (id, organization_id, name, slug) values ($1, $2, 'api', 'api')", [organization.projectId, organization.id]);
    await sql(
      "insert into environment (id, organization_id, project_id, name, namespace, intent) values ($1, $2, $3, 'production', 'production', '{}')",
      [organization.environmentId, organization.id, organization.projectId],
    );
    return organization;
  }

  /** One statement, so one change log row per Organization. */
  async function createServices(organization: Organization, names: string[]) {
    const result = await harness.pool.query<{ id: string }>(`
      with lineage as (
        insert into service_lineage (organization_id, project_id, canonical_name, canonical_slug)
        select $3, $1, name, name from unnest($2::text[]) as name returning id, canonical_name
      )
      insert into service (organization_id, project_id, environment_id, lineage_id, name)
      select $3, $1, $4, id, canonical_name from lineage returning id
    `, [organization.projectId, names, organization.id, organization.environmentId]);
    return result.rows.map((row) => row.id);
  }

  function read(organization: Organization, since?: string) {
    return harness.runEffect(readCollection({ userId: organization.userId }, {
      table: "service", userId: organization.userId, organizationSlug: organization.slug, since,
    })) as Promise<CollectionRead<ServiceRow>>;
  }

  async function cursorNow(organization: Organization) {
    return (await read(organization)).cursor;
  }

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
    alpha = await createOrganization(`alpha-${randomUUID().slice(0, 8)}`);
    beta = await createOrganization(`beta-${randomUUID().slice(0, 8)}`);
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  it("returns an Organization's own changes since the cursor and never another's", async () => {
    const [alphaService] = await createServices(alpha, [`web-${randomUUID().slice(0, 8)}`]);
    await createServices(beta, [`web-${randomUUID().slice(0, 8)}`]);
    const since = await cursorNow(alpha);

    await sql("update service set name = 'renamed' where id = $1", [alphaService]);

    const alphaChanges = await read(alpha, since);
    expect(alphaChanges).toMatchObject({ full: false, deleted: [], rows: [{ id: alphaService, name: "renamed" }] });
    const betaChanges = await read(beta, since);
    expect(betaChanges).toMatchObject({ full: false, deleted: [], rows: [] });
    // Nothing changed, yet the cursor still moves forward.
    expect(BigInt(betaChanges.cursor)).toBeGreaterThan(BigInt(since));
    // The change stream's reader sees no tables for the other Organization.
    expect(await harness.runEffect(readChangeWindow({ organizationId: beta.id, since })))
      .toMatchObject({ kind: "delta", sourceTables: [], changed: [], deleted: [] });
    const quiet = await read(alpha, alphaChanges.cursor);
    expect(quiet).toMatchObject({ full: false, deleted: [], rows: [] });
  });

  it("returns a deleted Service as a deleted id", async () => {
    const [service] = await createServices(alpha, [`worker-${randomUUID().slice(0, 8)}`]);
    const since = await cursorNow(alpha);

    await sql("delete from service where id = $1", [service]);

    expect(await read(alpha, since)).toMatchObject({ full: false, rows: [], deleted: [service] });
    expect(await read(beta, since)).toMatchObject({ full: false, rows: [], deleted: [] });
  });

  it("falls back to a full read when one statement touches more than 100 Services", async () => {
    const since = await cursorNow(alpha);

    const created = await createServices(alpha, Array.from({ length: 101 }, (_, index) => `bulk-${index}-${randomUUID().slice(0, 8)}`));

    const changes = await read(alpha, since);
    expect(changes.full).toBe(true);
    // The window still names its tables, so the change stream refetches their collections.
    expect(await harness.runEffect(readChangeWindow({ organizationId: alpha.id, since })))
      .toMatchObject({ kind: "full", sourceTables: expect.arrayContaining(["service"]) });
    expect(changes.rows.map((row) => row.id)).toEqual(expect.arrayContaining(created));
    expect(changes.rows.every((row) => row.organizationId === alpha.id)).toBe(true);
  });

  it("holds back changes until every older transaction finishes, and skips none", async () => {
    const [first, second, third] = await createServices(alpha, ["a", "b", "c"].map((name) => `${name}-${randomUUID().slice(0, 8)}`));
    const since = await cursorNow(alpha);
    const older = await harness.pool.connect();
    const newer = await harness.pool.connect();
    try {
      // `older` takes its transaction id first but logs its Service change last.
      await older.query("begin");
      await older.query("select pg_current_xact_id()");
      await newer.query("begin");
      await newer.query("update service set name = 'second' where id = $1", [second]);
      await older.query("update service set name = 'first' where id = $1", [first]);
      await older.query("commit");
      // A later transaction commits while `newer` still holds an earlier change.
      await sql("update service set name = 'third' where id = $1", [third]);

      const held = await read(alpha, since);
      expect(held.rows.map((row) => row.name)).toEqual(["first"]);

      await newer.query("commit");
      const released = await read(alpha, held.cursor);
      expect(released.rows.map((row) => row.name).sort()).toEqual(["second", "third"]);
    } finally {
      await newer.query("rollback").catch(() => undefined);
      older.release();
      newer.release();
    }
  });

  // Retention is global, so these run last and share one log.
  describe("after retention", () => {
    const ageAll = () => sql("update organization_change set created_at = now() - interval '25 hours'");
    const loggedXids = async () => (await sql("select xid::text from organization_change group by xid order by xid")).rows.map((row) => row.xid as string);

    it("prunes changes older than 24 hours and keeps newer ones", async () => {
      await createServices(alpha, [`old-${randomUUID().slice(0, 8)}`]);
      await ageAll();
      await createServices(alpha, [`new-${randomUUID().slice(0, 8)}`]);
      const [newest] = (await loggedXids()).slice(-1);

      await harness.runEffect(pruneChangeLog());

      expect(await loggedXids()).toEqual([newest]);
    });

    it("keeps the newest logged transaction when every change is older than 24 hours", async () => {
      await createServices(alpha, [`quiet-${randomUUID().slice(0, 8)}`]);
      await ageAll();
      const [newest] = (await loggedXids()).slice(-1);

      await harness.runEffect(pruneChangeLog());

      // A quiet log keeps its fence, so recent cursors stay incremental instead of reading in full.
      expect(await loggedXids()).toEqual([newest]);
    });

    it("never prunes at or past a running transaction, so the oldest change stays a fence", async () => {
      const running = await harness.pool.connect();
      try {
        await running.query("begin");
        await running.query("select pg_current_xact_id()");
        await createServices(alpha, [`later-${randomUUID().slice(0, 8)}`]);
        await ageAll();
        const before = await loggedXids();

        await harness.runEffect(pruneChangeLog());

        // The later change survives: pruning it would leave a gap above the running transaction's change.
        expect(await loggedXids()).toEqual(before.slice(-1));
      } finally {
        await running.query("rollback");
        running.release();
      }
    });

    it("reads in full, flagged as such, from a since below the oldest change", async () => {
      const stale = await cursorNow(alpha);
      const [service] = await createServices(alpha, [`fresh-${randomUUID().slice(0, 8)}`]);
      await ageAll();
      await createServices(beta, [`fresh-${randomUUID().slice(0, 8)}`]);
      await harness.runEffect(pruneChangeLog());
      const fence = await cursorNow(alpha);

      const expired = await read(alpha, stale);
      expect(expired.full).toBe(true);
      expect(expired.rows.map((row) => row.id)).toContain(service);
      // At or above the oldest change nothing was pruned, so the read stays incremental.
      expect(await read(alpha, fence)).toMatchObject({ full: false, rows: [], deleted: [] });
    });

    it("reads an empty delta from a since against an empty log, and in full without one", async () => {
      const since = await cursorNow(alpha);
      await sql("delete from organization_change");

      // An empty log has no fence: nothing was pruned after `since`, so nothing is missing.
      expect(await read(alpha, since)).toMatchObject({ full: false, rows: [], deleted: [] });
      expect(await harness.runEffect(readChangeWindow({ organizationId: alpha.id, since })))
        .toEqual({ kind: "delta", cursor: expect.any(String), sourceTables: [], changed: [], deleted: [] });
      expect(await harness.runEffect(readChangeWindow({ organizationId: alpha.id, since: undefined }))).toMatchObject({ kind: "full" });
    });
  });
});

describe("every Org Store collection reads its changes from the Organization change log", () => {
  let harness: GithubPostgresTestHarness;
  const organizationId = randomUUID();
  const userId = randomUUID();
  const otherUserId = randomUUID();
  const projectId = randomUUID();
  const environmentId = randomUUID();
  const deploymentId = randomUUID();
  const slug = `every-${randomUUID().slice(0, 8)}`;

  const sql = (text: string, values: unknown[] = []) => harness.pool.query(text, values);

  function read(table: CollectionName, since?: string) {
    return harness.runEffect(readCollection({ userId }, { table, userId, organizationSlug: slug, since })) as Promise<CollectionRead<object>>;
  }
  const serialized = (rows: object[]) => rows.map((row) => JSON.stringify(row)).sort();

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
    const lineageId = randomUUID();
    const snapshotId = randomUUID();
    await sql("insert into organization (id, name, slug) values ($1, $2, $2)", [organizationId, slug]);
    for (const id of [userId, otherUserId]) {
      await sql("insert into \"user\" (id, email, name) values ($1, $2, $2)", [id, `${id}@example.test`]);
      await sql("insert into member (user_id, organization_id, role) values ($1, $2, 'owner')", [id, organizationId]);
    }
    await sql("insert into project (id, organization_id, name, slug) values ($1, $2, 'api', 'api')", [projectId, organizationId]);
    await sql(
      "insert into environment (id, organization_id, project_id, name, namespace, intent) values ($1, $2, $3, 'production', 'production', '{}')",
      [environmentId, organizationId, projectId],
    );
    for (const id of [userId, otherUserId]) {
      await sql("insert into user_project_preference (organization_id, user_id, project_id, environment_id) values ($1, $2, $3, $4)",
        [organizationId, id, projectId, environmentId]);
    }
    await sql(`
      with lineage as (
        insert into service_lineage (organization_id, project_id, canonical_name, canonical_slug) values ($1, $2, 'web', 'web') returning id
      )
      insert into service (organization_id, project_id, environment_id, lineage_id, name) select $1, $2, $3, id, 'web' from lineage
    `, [organizationId, projectId, environmentId]);
    await sql("insert into resource_lineage (id, organization_id, project_id, canonical_name, canonical_slug) values ($1, $2, $3, 'data', 'data')",
      [lineageId, organizationId, projectId]);
    await sql("insert into environment_resource (organization_id, project_id, environment_id, lineage_id, implementation_type) values ($1, $2, $3, $4, 'volume')",
      [organizationId, projectId, environmentId, lineageId]);
    await sql("insert into environment_canvas_node_position (organization_id, environment_id, resource_type, resource_id, x, y) values ($1, $2, 'volume', $3, 1, 2)",
      [organizationId, environmentId, lineageId]);
    await sql("insert into environment_saved_state_snapshot (id, organization_id, environment_id, actor_id, intent, volume_deletion_authorizations) values ($1, $2, $3, $4, '{}', '[]')",
      [snapshotId, organizationId, environmentId, userId]);
    await sql("insert into environment_deployment (id, organization_id, environment_id, trigger_origin, saved_state_snapshot_id) values ($1, $2, $3, '{}', $4)",
      [deploymentId, organizationId, environmentId, snapshotId]);
    await sql("insert into environment_deployment_event (organization_id, deployment_id, progress) values ($1, $2, '{\"stage\": \"queued\"}')",
      [organizationId, deploymentId]);
    await sql("insert into environment_node_config_snapshot (organization_id, environment_deployment_id, environment_id, node_type, node_id, node_lineage_id, config) values ($1, $2, $3, 'volume', $4, $4, '{}')",
      [organizationId, deploymentId, environmentId, lineageId]);
    await sql("insert into environment_node_introduction (organization_id, environment_id, node_type, node_id, node_lineage_id, config) values ($1, $2, 'volume', $3, $3, '{}')",
      [organizationId, environmentId, lineageId]);
    await sql("insert into volume_remove_attempt (organization_id, requested_by_user_id, environment_id, volumes) values ($1, $2, $3, '[{}]')",
      [organizationId, userId, environmentId]);
    await sql("insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_public_key) values ($1, '{}', $2, 'key')",
      [organizationId, "0".repeat(32)]);
    await sql("insert into organization_cluster_domain (organization_id, endpoint, name, encrypted_token, reserved_at, lease_renewed_at) values ($1, 'https://dns.example.test/', 'acme.ployz.test', '{}', now(), now())",
      [organizationId]);
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  it.each(orgStoreTableNames)("%s re-reads every row a change to each of its source tables names", async (table) => {
    const full = await read(table);
    expect(full.rows.length).toBeGreaterThan(0);
    const collection = orgStoreTables[table](slug, { queryClient: new QueryClient(), sessionId: "session", userId });
    // SAFETY: read(table) returns rows of the collection that orgStoreTables names `table`.
    const clientKeys = full.rows.map((row) => String(collection.config.getKey(row as never)));
    for (const source of changeNameSources[table]) {
      // A no-op update logs every key of the table, so the incremental read must find every row by that key.
      await sql(`update ${source} set organization_id = organization_id where organization_id = $1`, [organizationId]);
      // The client keys its rows exactly as the log names them, so deletes and merges hit the right row.
      const window = await harness.runEffect(readChangeWindow({ organizationId, since: full.cursor, sourceTables: [source] }));
      expect(window.kind === "delta" ? window.changed : [], source).toEqual(expect.arrayContaining(clientKeys));
      const changes = await read(table, full.cursor);
      expect(changes, source).toMatchObject({ full: false, deleted: [] });
      expect(serialized(changes.rows), source).toEqual(serialized(full.rows));
    }
  });

  it("moves a deployment's progress when an event is logged", async () => {
    const since = (await read("environment_deployment")).cursor;
    await sql("insert into environment_deployment_event (organization_id, deployment_id, progress) values ($1, $2, '{\"stage\": \"building\"}')",
      [organizationId, deploymentId]);
    expect(await read("environment_deployment", since)).toMatchObject({
      full: false, rows: [{ id: deploymentId, runtimeProgress: { stage: "building" } }],
    });
  });

  it("keeps this member's project preference when another member's for the same project is deleted", async () => {
    const since = (await read("project_preference")).cursor;
    await sql("delete from user_project_preference where user_id = $1", [otherUserId]);
    // The client drops the deleted key, then upserts the rows, so this member's preference survives.
    expect(await read("project_preference", since)).toMatchObject({
      full: false, deleted: [projectId], rows: [{ id: projectId, environmentId }],
    });
  });

  it("names the organization when it is renamed", async () => {
    const { cursor: since } = await harness.runEffect(readChangeWindow({ organizationId, since: undefined }));
    await sql("update organization set name = 'Renamed' where id = $1", [organizationId]);
    const window = await harness.runEffect(readChangeWindow({ organizationId, since }));
    expect(collectionsOf(window.sourceTables)).toEqual(["organization"]);
  });
});
