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
  type PostgresTestHarness,
  startPostgresTestHarness,
} from "#/test/postgres";

type PositionRow = { id: string; resourceId: string; x: number; organizationId: string };
type Organization = { id: string; slug: string; userId: string; environmentId: string };

describe("incremental canvas position reads from the Organization change log", () => {
  let harness: PostgresTestHarness;
  let alpha: Organization;
  let beta: Organization;

  const sql = (text: string, values: unknown[] = []) => harness.pool.query(text, values);

  async function createOrganization(slug: string): Promise<Organization> {
    const organization = { id: randomUUID(), slug, userId: randomUUID(), environmentId: randomUUID() };
    await sql("insert into organization (id, name, slug) values ($1, $2, $2)", [organization.id, slug]);
    await sql("insert into \"user\" (id, email, name) values ($1, $2, $2)", [organization.userId, `${slug}@example.test`]);
    await sql("insert into member (user_id, organization_id, role) values ($1, $2, 'owner')", [organization.userId, organization.id]);
    return organization;
  }

  /** One statement, so one change log row per Organization. Returns each position's Service id. */
  async function createPositions(organization: Organization, count: number) {
    const result = await harness.pool.query<{ resource_id: string }>(`
      insert into environment_canvas_node_position (organization_id, environment_id, resource_type, resource_id, x, y)
      select $1, $2, 'service', gen_random_uuid(), 0, 0 from generate_series(1, $3) returning resource_id
    `, [organization.id, organization.environmentId, count]);
    return result.rows.map((row) => row.resource_id);
  }

  const moveTo = (resourceId: string, x: number) =>
    sql("update environment_canvas_node_position set x = $2 where resource_id = $1", [resourceId, x]);

  function read(organization: Organization, since?: string) {
    return harness.runEffect(readCollection({ userId: organization.userId }, {
      table: "environment_canvas_node_position", userId: organization.userId, organizationSlug: organization.slug, since,
    })) as Promise<CollectionRead<PositionRow>>;
  }

  async function cursorNow(organization: Organization) {
    return (await read(organization)).cursor;
  }

  beforeAll(async () => {
    harness = await startPostgresTestHarness({ ownServer: true });
    alpha = await createOrganization(`alpha-${randomUUID().slice(0, 8)}`);
    beta = await createOrganization(`beta-${randomUUID().slice(0, 8)}`);
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  it("returns an Organization's own changes since the cursor and never another's", async () => {
    const [alphaNode = ""] = await createPositions(alpha, 1);
    await createPositions(beta, 1);
    const since = await cursorNow(alpha);

    await moveTo(alphaNode, 7);

    const alphaChanges = await read(alpha, since);
    expect(alphaChanges).toMatchObject({ full: false, deleted: [], rows: [{ resourceId: alphaNode, x: 7 }] });
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

  it("returns a deleted row by its key", async () => {
    const [node] = await createPositions(alpha, 1);
    const since = await cursorNow(alpha);

    await sql("delete from environment_canvas_node_position where resource_id = $1", [node]);

    expect(await read(alpha, since)).toMatchObject({ full: false, rows: [], deleted: [`service:${node}`] });
    expect(await read(beta, since)).toMatchObject({ full: false, rows: [], deleted: [] });
  });

  it("falls back to a full read when one statement touches more than 100 rows", async () => {
    const since = await cursorNow(alpha);

    const created = await createPositions(alpha, 101);

    const changes = await read(alpha, since);
    expect(changes.full).toBe(true);
    // The window still names its tables, so the change stream refetches their collections.
    expect(await harness.runEffect(readChangeWindow({ organizationId: alpha.id, since })))
      .toMatchObject({ kind: "full", sourceTables: expect.arrayContaining(["environment_canvas_node_position"]) });
    expect(changes.rows.map((row) => row.resourceId)).toEqual(expect.arrayContaining(created));
    expect(changes.rows.every((row) => row.organizationId === alpha.id)).toBe(true);
  });

  it("holds back changes until every older transaction finishes, and skips none", async () => {
    const [first, second, third = ""] = await createPositions(alpha, 3);
    const since = await cursorNow(alpha);
    const older = await harness.pool.connect();
    const newer = await harness.pool.connect();
    const move = "update environment_canvas_node_position set x = $2 where resource_id = $1";
    try {
      // `older` takes its transaction id first but logs its change last.
      await older.query("begin");
      await older.query("select pg_current_xact_id()");
      await newer.query("begin");
      await newer.query(move, [second, 2]);
      await older.query(move, [first, 1]);
      await older.query("commit");
      // A later transaction commits while `newer` still holds an earlier change.
      await moveTo(third, 3);

      const held = await read(alpha, since);
      expect(held.rows.map((row) => row.x)).toEqual([1]);

      await newer.query("commit");
      const released = await read(alpha, held.cursor);
      expect(released.rows.map((row) => row.x).sort((a, b) => a - b)).toEqual([2, 3]);
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
      await createPositions(alpha, 1);
      await ageAll();
      await createPositions(alpha, 1);
      const [newest] = (await loggedXids()).slice(-1);

      await harness.runEffect(pruneChangeLog());

      expect(await loggedXids()).toEqual([newest]);
    });

    it("keeps the newest logged transaction when every change is older than 24 hours", async () => {
      await createPositions(alpha, 1);
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
        await createPositions(alpha, 1);
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
      const [node] = await createPositions(alpha, 1);
      await ageAll();
      await createPositions(beta, 1);
      await harness.runEffect(pruneChangeLog());
      const fence = await cursorNow(alpha);

      const expired = await read(alpha, stale);
      expect(expired.full).toBe(true);
      expect(expired.rows.map((row) => row.resourceId)).toContain(node);
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
  let harness: PostgresTestHarness;
  const organizationId = randomUUID();
  const userId = randomUUID();
  const environmentId = randomUUID();
  const slug = `every-${randomUUID().slice(0, 8)}`;

  const sql = (text: string, values: unknown[] = []) => harness.pool.query(text, values);

  function read(table: CollectionName, since?: string) {
    return harness.runEffect(readCollection({ userId }, { table, userId, organizationSlug: slug, since })) as Promise<CollectionRead<object>>;
  }
  const serialized = (rows: object[]) => rows.map((row) => JSON.stringify(row)).sort();

  beforeAll(async () => {
    harness = await startPostgresTestHarness({ ownServer: true });
    await sql("insert into organization (id, name, slug) values ($1, $2, $2)", [organizationId, slug]);
    await sql("insert into \"user\" (id, email, name) values ($1, $2, $2)", [userId, `${userId}@example.test`]);
    await sql("insert into member (user_id, organization_id, role) values ($1, $2, 'owner')", [userId, organizationId]);
    await sql("insert into environment_canvas_node_position (organization_id, environment_id, resource_type, resource_id, x, y) values ($1, $2, 'volume', gen_random_uuid(), 1, 2)",
      [organizationId, environmentId]);
    await sql("insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_public_key) values ($1, '{}', $2, 'key')",
      [organizationId, "0".repeat(32)]);
    await sql("insert into organization_cluster_domain (organization_id, endpoint, name, encrypted_token, reserved_at, lease_renewed_at) values ($1, 'https://dns.example.test/', 'acme.ployz.test', '{}', now(), now())",
      [organizationId]);
    await sql("insert into organization_server_upgrades (organization_id, automatic) values ($1, false)", [organizationId]);
    await sql("insert into organization_settings (organization_id, ask_before_destructive) values ($1, false)", [organizationId]);
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

  it("names the organization when it is renamed", async () => {
    const { cursor: since } = await harness.runEffect(readChangeWindow({ organizationId, since: undefined }));
    await sql("update organization set name = 'Renamed' where id = $1", [organizationId]);
    const window = await harness.runEffect(readChangeWindow({ organizationId, since }));
    expect(collectionsOf(window.sourceTables)).toEqual(["organization"]);
  });
});
