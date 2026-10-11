import { randomUUID } from "node:crypto";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { controlPlaneStatements, editUptimeCheckStatement } from "#/modules/uptime/incident-fence";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-0000000a0001";
const otherOrganizationId = "00000000-0000-4000-8000-0000000a0002";
const evidence = JSON.stringify({ observations: [] });

let harness: PostgresTestHarness;

function one<T>(rows: readonly T[]): T {
  const [row] = rows;
  if (row === undefined || rows.length !== 1) throw new Error(`expected one row, got ${rows.length}`);
  return row;
}

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  await harness.pool.query(
    "insert into organization (id, name, slug) values ($1, 'Acme', 'acme'), ($2, 'Other', 'other')",
    [organizationId, otherOrganizationId],
  );
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

async function createCheck(owner = organizationId) {
  const { rows } = await harness.pool.query<{ id: string }>(
    `insert into uptime_check (organization_id, name, url, environment_id, service_id)
     values ($1, 'web', 'https://example.com/', gen_random_uuid()::text, gen_random_uuid()::text) returning id`,
    [owner],
  );
  return one(rows).id;
}

async function open(checkId: string, revision: number, incidentId = randomUUID()) {
  const { rows } = await harness.pool.query<{ applied: number }>(controlPlaneStatements.open,
    [checkId, revision, incidentId, new Date(), evidence, evidence]);
  return { applied: one(rows).applied, incidentId };
}

async function observedClose(incidentId: string, revision: number) {
  const { rows } = await harness.pool.query<{ applied: number }>(controlPlaneStatements.observedClose,
    [incidentId, revision, new Date(Date.now() + 1000), JSON.stringify({ kind: "observed", observations: [] }), evidence]);
  return one(rows).applied;
}

function edit(checkId: string, enabled = true) {
  return harness.db.execute(editUptimeCheckStatement({
    id: checkId, organizationId, url: "https://example.com/health", intervalSeconds: 300, timeoutMs: 10000, enabled,
  }));
}

async function waitForLockWaiter() {
  for (let attempt = 0; attempt < 40; attempt += 1) {
    const { rows } = await harness.pool.query<{ waiting: number }>(
      "select count(*)::int as waiting from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'");
    if (one(rows).waiting > 0) return;
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("the close never waited on the uncommitted bump");
}

async function state(checkId: string) {
  const incidents = await harness.pool.query<{ id: string; closed_by: { kind: string } | null }>(
    "select id, closed_by from uptime_incident where check_id = $1 order by opened_at", [checkId]);
  const alerts = await harness.pool.query<{ incident_id: string; kind: string }>(
    "select a.incident_id, a.kind from uptime_alert a join uptime_incident i on i.id = a.incident_id where i.check_id = $1 order by a.created_at, a.kind",
    [checkId]);
  const revision = await harness.pool.query<{ revision: number }>("select revision from uptime_check where id = $1", [checkId]);
  return { incidents: incidents.rows, alerts: alerts.rows, revision: one(revision.rows).revision };
}

describe("open", () => {
  it("applies once at the matching revision with one opened alert", async () => {
    const checkId = await createCheck();
    const { applied, incidentId } = await open(checkId, 1);
    expect(applied).toBe(1);
    expect(await state(checkId)).toEqual({
      incidents: [{ id: incidentId, closed_by: null }],
      alerts: [{ incident_id: incidentId, kind: "opened" }],
      revision: 1,
    });
  });

  it("applies 0 times at a stale revision", async () => {
    const checkId = await createCheck();
    await edit(checkId);
    expect((await open(checkId, 1)).applied).toBe(0);
    expect(await state(checkId)).toEqual({ incidents: [], alerts: [], revision: 2 });
  });

  it("applies 0 times when the check is disabled", async () => {
    const checkId = await createCheck();
    await harness.pool.query("update uptime_check set enabled = false where id = $1", [checkId]);
    expect((await open(checkId, 1)).applied).toBe(0);
    expect(await state(checkId)).toMatchObject({ incidents: [], alerts: [] });
  });

  it("applies 0 times while another incident is open, and a replay of the same open is a no-op", async () => {
    const checkId = await createCheck();
    const first = await open(checkId, 1);
    expect((await open(checkId, 1)).applied).toBe(0);
    expect((await open(checkId, 1, first.incidentId)).applied).toBe(0);
    expect(await state(checkId)).toEqual({
      incidents: [{ id: first.incidentId, closed_by: null }],
      alerts: [{ incident_id: first.incidentId, kind: "opened" }],
      revision: 1,
    });
  });
});

describe("observed close", () => {
  it("applies once with one resolved alert, then 0 times on the closed incident", async () => {
    const checkId = await createCheck();
    const { incidentId } = await open(checkId, 1);
    expect(await observedClose(incidentId, 1)).toBe(1);
    expect(await observedClose(incidentId, 1)).toBe(0);
    expect(await state(checkId)).toEqual({
      incidents: [{ id: incidentId, closed_by: { kind: "observed", observations: [] } }],
      alerts: [{ incident_id: incidentId, kind: "opened" }, { incident_id: incidentId, kind: "resolved" }],
      revision: 1,
    });
  });

  it("applies 0 times when the revision changed under an open incident", async () => {
    const checkId = await createCheck();
    const { incidentId } = await open(checkId, 1);
    // Bumped without the edit statement, so the incident stays open and only the revision fence can refuse.
    await harness.pool.query("update uptime_check set revision = revision + 1 where id = $1", [checkId]);
    expect(await observedClose(incidentId, 1)).toBe(0);
    expect(await observedClose(incidentId, 2)).toBe(0);
    expect(await state(checkId)).toMatchObject({
      incidents: [{ id: incidentId, closed_by: null }],
      alerts: [{ incident_id: incidentId, kind: "opened" }],
    });
  });

  it("waits on an uncommitted revision bump, then applies 0 times", async () => {
    const checkId = await createCheck();
    const { incidentId } = await open(checkId, 1);
    const bump = await harness.pool.connect();
    try {
      await bump.query("begin");
      await bump.query("update uptime_check set revision = revision + 1 where id = $1", [checkId]);
      const closing = observedClose(incidentId, 1);
      await waitForLockWaiter();
      await bump.query("commit");
      expect(await closing).toBe(0);
    } finally {
      bump.release(true);
    }
    expect(await state(checkId)).toMatchObject({ incidents: [{ id: incidentId, closed_by: null }], revision: 2 });
  });
});

describe("config edit", () => {
  it.each([
    { enabled: true, kind: "check_changed" },
    { enabled: false, kind: "check_disabled" },
  ])("bumps the revision and closes the open incident as $kind with no alert", async ({ enabled, kind }) => {
    const checkId = await createCheck();
    const { incidentId } = await open(checkId, 1);
    const edited = await edit(checkId, enabled);
    expect(edited.rows).toEqual([{ id: checkId, enabled }]);
    expect(await observedClose(incidentId, 1)).toBe(0);
    expect(await state(checkId)).toEqual({
      incidents: [{ id: incidentId, closed_by: { kind } }],
      alerts: [{ incident_id: incidentId, kind: "opened" }],
      revision: 2,
    });
  });

  it("edits only the named organization's check", async () => {
    const checkId = await createCheck(otherOrganizationId);
    expect((await edit(checkId)).rows).toEqual([]);
    expect((await state(checkId)).revision).toBe(1);
  });
});

describe("composite foreign keys", () => {
  const foreignKeyViolation = expect.objectContaining({ code: "23503" });

  it("rejects a child row naming another organization's check", async () => {
    const checkId = await createCheck(otherOrganizationId);
    await expect(harness.pool.query(
      "insert into uptime_incident (id, organization_id, check_id, check_revision, opened_at, opened_by) values ($1, $2, $3, 1, now(), '{}')",
      [randomUUID(), organizationId, checkId],
    )).rejects.toEqual(foreignKeyViolation);
    await expect(harness.pool.query(
      `insert into uptime_result (check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status,
         failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, agent)
       values ($1, $2, 'fra', now(), 'routine', 0, 'pass', 200, 'none', '', '', 0, 0, 0, 0, 0, 'a')`,
      [checkId, organizationId],
    )).rejects.toEqual(foreignKeyViolation);
    await expect(harness.pool.query(
      `insert into uptime_result_hourly (check_id, organization_id, region, hour, probes, failures, inconclusive, pass_ms_sum, pass_ms_max)
       values ($1, $2, 'fra', date_trunc('hour', now()), 0, 0, 0, 0, 0)`,
      [checkId, organizationId],
    )).rejects.toEqual(foreignKeyViolation);
  });
});
