import { randomUUID } from "node:crypto";
import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createUptimeMaintenanceFunctions } from "#/modules/uptime/maintenance.inngest";
import { pruneUptimeBatch, rollupUptime } from "#/modules/uptime/maintenance.server";
import { retentionCutoff } from "#/modules/uptime/retention";
import { failureClass } from "#/modules/uptime/uptime";
import { FAILURE_CLASSES } from "#/modules/uptime/uptime-status";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const organizationId = "00000000-0000-4000-8000-0000000d0001";

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  await harness.pool.query("insert into organization (id, name, slug) values ($1, 'Acme', 'acme')", [organizationId]);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

async function createCheck() {
  const { rows } = await harness.pool.query<{ id: string }>(
    `insert into uptime_check (organization_id, name, url, environment_id, service_id)
     values ($1, 'web', 'https://example.com/', gen_random_uuid()::text, gen_random_uuid()::text) returning id`,
    [organizationId],
  );
  const [row] = rows;
  if (row === undefined) throw new Error("no check");
  return row.id;
}

type Kind = "routine" | "confirm" | "recheck" | "triggered";
type Verdict = "pass" | "fail" | "inconclusive";

function result(checkId: string, region: string, scheduledAt: string, verdict: Verdict, totalMs: number, kind: Kind = "routine") {
  return harness.pool.query(
    `insert into uptime_result (check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status,
       failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, agent)
     values ($1, $2, $3, $4, $5, 0, $6, $7, $8, '', '', 0, 0, 0, 0, $9, 'agent-1')`,
    [checkId, organizationId, region, scheduledAt, kind, verdict, verdict === "pass" ? 200 : 0,
      verdict === "fail" ? "connect" : "none", totalMs],
  );
}

type Bucket = {
  region: string; hour: string; probes: number; failures: number; inconclusive: number;
  pass_ms_sum: number; pass_ms_max: number; pass_ms_p50: number | null;
};

async function hourly(checkId: string) {
  const { rows } = await harness.pool.query<Bucket>(
    `select region, to_char(hour at time zone 'UTC', 'YYYY-MM-DD"T"HH24:MI') as hour, probes, failures, inconclusive,
       pass_ms_sum::int as pass_ms_sum, pass_ms_max, pass_ms_p50
     from uptime_result_hourly where check_id = $1 order by hour, region`,
    [checkId],
  );
  return rows;
}

type Probe = {
  verdict: Verdict; status: number; failedPhase: "none" | "dns" | "connect" | "tls" | "request"; error: string; detail: string;
  phases: readonly [number, number, number, number]; totalMs: number;
};

/** A routine probe with every field the agent reports. */
function probe(checkId: string, region: string, scheduledAt: string, patch: Partial<Probe>) {
  const row: Probe = { verdict: "pass", status: 200, failedPhase: "none", error: "", detail: "", phases: [0, 0, 0, 0], totalMs: 0, ...patch };
  return harness.pool.query(
    `insert into uptime_result (check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status,
       failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, agent)
     values ($1, $2, $3, $4, 'routine', 0, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'agent-1')`,
    [checkId, organizationId, region, scheduledAt, row.verdict, row.status, row.failedPhase, row.error, row.detail, ...row.phases, row.totalMs],
  );
}

type Breakdown = {
  region: string; dns_ms_sum: number; connect_ms_sum: number; tls_ms_sum: number; ttfb_ms_sum: number;
  failures: number; classes: Record<string, number>; last_error: string | null; last_error_class: string | null; last_error_at: string | null;
};

async function breakdown(checkId: string) {
  const { rows } = await harness.pool.query<Breakdown>(
    `select region, dns_ms_sum::int as dns_ms_sum, connect_ms_sum::int as connect_ms_sum, tls_ms_sum::int as tls_ms_sum,
       ttfb_ms_sum::int as ttfb_ms_sum, failures,
       json_build_object(${FAILURE_CLASSES.map((cls) => `'${cls}', failures_${cls}`).join(", ")}) as classes,
       last_error, last_error_class, to_char(last_error_at at time zone 'UTC', 'HH24:MI') as last_error_at
     from uptime_result_hourly where check_id = $1 order by hour, region`,
    [checkId],
  );
  return rows;
}

const counts = (patch: Partial<Record<(typeof FAILURE_CLASSES)[number], number>>) =>
  Object.fromEntries(FAILURE_CLASSES.map((cls) => [cls, patch[cls] ?? 0]));

const rollup = (now: string) => harness.runEffect(rollupUptime(new Date(now)));

describe("uptime-rollup", () => {
  it("counts one closed hour's routine probes per region, with percentiles over passing probes", async () => {
    const a = await createCheck();
    const b = await createCheck();
    for (const [minute, ms] of [["00", 100], ["10", 200], ["20", 300], ["30", 400]] as const) {
      await result(a, "eu", `2026-10-11T09:${minute}:00Z`, "pass", ms);
    }
    await result(a, "eu", "2026-10-11T09:40:00Z", "fail", 9000);
    await result(a, "eu", "2026-10-11T09:50:00Z", "inconclusive", 0);
    await result(a, "eu", "2026-10-11T09:41:00Z", "fail", 9000, "confirm");
    await result(a, "eu", "2026-10-11T09:42:00Z", "pass", 5, "recheck");
    await result(a, "us", "2026-10-11T09:59:59.999Z", "fail", 30000);
    await result(b, "eu", "2026-10-11T09:00:00Z", "inconclusive", 0);
    await result(a, "eu", "2026-10-11T08:59:59.999Z", "pass", 700);

    await rollup("2026-10-11T10:05:00Z");

    // p50 of 100..400 is 250.
    expect(await hourly(a)).toEqual([
      { region: "eu", hour: "2026-10-11T08:00", probes: 1, failures: 0, inconclusive: 0, pass_ms_sum: 700, pass_ms_max: 700, pass_ms_p50: 700 },
      { region: "eu", hour: "2026-10-11T09:00", probes: 5, failures: 1, inconclusive: 1, pass_ms_sum: 1000, pass_ms_max: 400, pass_ms_p50: 250 },
      { region: "us", hour: "2026-10-11T09:00", probes: 1, failures: 1, inconclusive: 0, pass_ms_sum: 0, pass_ms_max: 0, pass_ms_p50: null },
    ]);
    expect(await hourly(b)).toEqual([
      { region: "eu", hour: "2026-10-11T09:00", probes: 0, failures: 0, inconclusive: 1, pass_ms_sum: 0, pass_ms_max: 0, pass_ms_p50: null },
    ]);
  });

  it("splits failures by class, sums passing probes' phases and keeps the newest error, the same on a rerun", async () => {
    const a = await createCheck();
    await probe(a, "eu", "2026-10-11T09:00:00Z", { phases: [4, 18, 31, 47], totalMs: 100 });
    await probe(a, "eu", "2026-10-11T09:05:00Z", { phases: [6, 20, 29, 145], totalMs: 200 });
    await probe(a, "eu", "2026-10-11T09:10:00Z", { verdict: "fail", status: 0, failedPhase: "tls", error: "other",
      detail: "invalid peer certificate: Expired", phases: [5, 19, 900, 0], totalMs: 924 });
    await probe(a, "eu", "2026-10-11T09:20:00Z", { verdict: "fail", status: 503, phases: [5, 19, 30, 400], totalMs: 454 });
    await probe(a, "eu", "2026-10-11T09:15:00Z", { verdict: "fail", status: 0, failedPhase: "request", error: "timed_out",
      detail: "operation timed out", totalMs: 10_000 });
    await probe(a, "eu", "2026-10-11T09:25:00Z", { verdict: "inconclusive", status: 0, error: "saturated", phases: [9, 9, 9, 9] });
    await probe(a, "us", "2026-10-11T09:00:00Z", { phases: [1, 2, 3, 4], totalMs: 10 });

    await rollup("2026-10-11T10:05:00Z");
    const first = await breakdown(a);
    await rollup("2026-10-11T10:59:00Z");

    expect(first).toEqual([
      { region: "eu", dns_ms_sum: 10, connect_ms_sum: 38, tls_ms_sum: 60, ttfb_ms_sum: 192, failures: 3,
        classes: counts({ status: 1, timeout: 1, tls: 1 }), last_error: "503", last_error_class: "status", last_error_at: "09:20" },
      { region: "us", dns_ms_sum: 1, connect_ms_sum: 2, tls_ms_sum: 3, ttfb_ms_sum: 4, failures: 0, classes: counts({}),
        last_error: null, last_error_class: null, last_error_at: null },
    ]);
    expect(await breakdown(a)).toEqual(first);
  });

  it("puts each failing probe in the class failureClass names, with its word and detail as the error", async () => {
    const a = await createCheck();
    const cases = [0, 503].flatMap((status) => ["", "connection_refused", "timed_out", "other"].flatMap((error) =>
      (["none", "dns", "connect", "tls", "request"] as const).map((failedPhase) => ({ status, error, failedPhase }))));
    for (const [index, row] of cases.entries()) {
      await probe(a, `r${index}`, "2026-10-11T09:00:00Z", { ...row, verdict: "fail", detail: index % 2 ? "why" : "" });
    }

    await rollup("2026-10-11T10:05:00Z");

    const rows = new Map((await breakdown(a)).map((row) => [row.region, row]));
    for (const [index, row] of cases.entries()) {
      const cls = failureClass({ status: row.status, error: row.error === "" ? null : row.error as "other",
        failedPhase: row.failedPhase === "none" ? null : row.failedPhase });
      const words = { status: String(row.status), timeout: "Timeout", refused: "Refused", dns: "DNS", connect: "Connect", tls: "TLS", other: "Error" };
      expect(rows.get(`r${index}`)).toMatchObject({
        failures: 1, classes: counts({ [cls]: 1 }), last_error_class: cls, last_error: index % 2 ? `${words[cls]} · why` : words[cls],
      });
    }
  });

  it("counts a triggered probe like a routine one, even at the same instant in the same region", async () => {
    const a = await createCheck();
    await result(a, "eu", "2026-10-11T09:00:00Z", "pass", 100);
    await result(a, "eu", "2026-10-11T09:00:00Z", "fail", 9000, "triggered");
    await result(a, "eu", "2026-10-11T09:30:00Z", "pass", 300, "triggered");

    await rollup("2026-10-11T10:05:00Z");

    expect(await hourly(a)).toEqual([
      { region: "eu", hour: "2026-10-11T09:00", probes: 3, failures: 1, inconclusive: 0, pass_ms_sum: 400, pass_ms_max: 300, pass_ms_p50: 200 },
    ]);
  });

  it("rolls up the hours before the run's scheduled time, however late the run starts", async () => {
    const a = await createCheck();
    await result(a, "eu", "2026-10-01T01:30:00Z", "pass", 100);
    await result(a, "eu", "2026-10-01T02:30:00Z", "pass", 200);
    const [rollupFn] = createUptimeMaintenanceFunctions(new Inngest({ id: "uptime-rollup-test" }), harness.runEffect);
    if (rollupFn === undefined) throw new Error("no rollup function");

    const scheduled = Date.parse("2026-10-01T03:05:00Z");
    await new InngestTestEngine({
      function: rollupFn,
      events: [{ name: "inngest/scheduled.timer", data: { cron: "5 * * * *" }, ts: scheduled }],
    }).execute();

    expect((await hourly(a)).map((bucket) => bucket.hour)).toEqual(["2026-10-01T01:00", "2026-10-01T02:00"]);
  });

  it("leaves the open hour alone", async () => {
    const a = await createCheck();
    await result(a, "eu", "2026-10-11T10:00:00Z", "pass", 100);
    await result(a, "eu", "2026-10-11T10:04:59Z", "fail", 100);

    await rollup("2026-10-11T10:05:00Z");

    expect(await hourly(a)).toEqual([]);
  });

  it("writes the same buckets when it runs again, and overwrites a bucket rather than adding to it", async () => {
    const a = await createCheck();
    await result(a, "eu", "2026-10-11T09:00:00Z", "pass", 100);
    await result(a, "eu", "2026-10-11T09:30:00Z", "fail", 100);
    await harness.pool.query(
      `insert into uptime_result_hourly (check_id, organization_id, region, hour, probes, failures, inconclusive, pass_ms_sum,
         pass_ms_max, pass_ms_p50, dns_ms_sum, failures_tls, last_error, last_error_class, last_error_at)
       values ($1, $2, 'eu', '2026-10-11T09:00:00Z', 99, 99, 99, 99, 99, 99, 99, 99, 'stale', 'tls', '2026-10-11T09:59:00Z')`,
      [a, organizationId],
    );

    await rollup("2026-10-11T10:05:00Z");
    const first = await hourly(a);
    await rollup("2026-10-11T10:05:00Z");
    await rollup("2026-10-11T10:59:00Z");

    expect(first).toEqual([
      { region: "eu", hour: "2026-10-11T09:00", probes: 2, failures: 1, inconclusive: 0, pass_ms_sum: 100, pass_ms_max: 100, pass_ms_p50: 100 },
    ]);
    expect(await hourly(a)).toEqual(first);
  });

  it("counts a late result on the next run, while its hour is still in the two-run window", async () => {
    const a = await createCheck();
    await result(a, "eu", "2026-10-11T09:00:00Z", "pass", 100);
    await rollup("2026-10-11T10:05:00Z");

    await result(a, "eu", "2026-10-11T09:30:00Z", "fail", 100);
    await rollup("2026-10-11T11:05:00Z");

    expect(await hourly(a)).toEqual([
      { region: "eu", hour: "2026-10-11T09:00", probes: 2, failures: 1, inconclusive: 0, pass_ms_sum: 100, pass_ms_max: 100, pass_ms_p50: 100 },
    ]);
  });

  it("never revisits an hour after its two runs, so a later result stays in raw only", async () => {
    const a = await createCheck();
    await result(a, "eu", "2026-10-11T08:00:00Z", "pass", 100);
    await rollup("2026-10-11T10:05:00Z");

    await result(a, "eu", "2026-10-11T08:30:00Z", "fail", 100);
    await rollup("2026-10-11T11:05:00Z");

    expect(await hourly(a)).toEqual([
      { region: "eu", hour: "2026-10-11T08:00", probes: 1, failures: 0, inconclusive: 0, pass_ms_sum: 100, pass_ms_max: 100, pass_ms_p50: 100 },
    ]);
  });
});

describe("uptime-prune", () => {
  const now = new Date("2026-10-11T10:05:00.000Z");
  const before = (cutoff: Date) => new Date(cutoff.getTime() - 1).toISOString();
  const prunes = { raw: retentionCutoff("raw", now), hourly: retentionCutoff("hourly", now),
    incidents: retentionCutoff("incidents", now), finishedAlerts: retentionCutoff("finishedAlerts", now) };

  async function pruneAll() {
    for (const kind of ["raw", "hourly", "incidents", "finishedAlerts"] as const) {
      while (await harness.runEffect(pruneUptimeBatch(kind, now)) > 0) { /* drain */ }
    }
  }

  it("uses Postgres' own month arithmetic for the 13-month cutoffs", async () => {
    for (const at of ["2026-10-11T10:05:00Z", "2026-03-31T12:00:00Z", "2025-03-31T00:00:00Z", "2028-02-29T23:59:59Z"]) {
      const { rows } = await harness.pool.query<{ cutoff: Date }>(
        "select ($1::timestamptz at time zone 'UTC' - interval '13 months') at time zone 'UTC' as cutoff", [at]);
      expect(retentionCutoff("hourly", new Date(at))).toEqual(rows[0]?.cutoff);
    }
  });

  it("deletes raw and hourly rows older than their cutoff and keeps the rest", async () => {
    const a = await createCheck();
    await result(a, "eu", before(prunes.raw), "pass", 1);
    await result(a, "eu", prunes.raw.toISOString(), "pass", 2);
    await result(a, "eu", now.toISOString(), "pass", 3);
    const bucket = (hour: string) => harness.pool.query(
      `insert into uptime_result_hourly (check_id, organization_id, region, hour, probes, failures, inconclusive, pass_ms_sum, pass_ms_max)
       values ($1, $2, 'eu', $3, 1, 0, 0, 1, 1)`, [a, organizationId, hour]);
    await bucket("2025-09-11T10:00:00Z");
    await bucket("2025-09-11T11:00:00Z");
    await bucket("2026-09-01T00:00:00Z");

    await pruneAll();

    const raw = await harness.pool.query<{ total_ms: number }>(
      "select total_ms from uptime_result where check_id = $1 order by total_ms", [a]);
    expect(raw.rows.map((row) => row.total_ms)).toEqual([2, 3]);
    expect((await hourly(a)).map((row) => row.hour)).toEqual(["2025-09-11T11:00", "2026-09-01T00:00"]);
  });

  it("deletes closed incidents past 13 months with their alerts, and finished alerts past 30 days", async () => {
    const a = await createCheck();
    const incident = async (openedAt: string, closedAt: string | null) => {
      const id = randomUUID();
      await harness.pool.query(
        `insert into uptime_incident (id, organization_id, check_id, check_revision, opened_at, opened_by, closed_at, closed_by)
         values ($1, $2, $3, 1, $4, '{}', $5::timestamptz, case when $5::timestamptz is null then null else '{"kind":"observed"}'::jsonb end)`,
        [id, organizationId, a, openedAt, closedAt]);
      return id;
    };
    const alert = (incidentId: string, kind: "opened" | "resolved", state: string, updatedAt: string) => harness.pool.query(
      `insert into uptime_alert (organization_id, incident_id, kind, payload, state, updated_at, sent_at)
       values ($1, $2, $3, '{}', $4, $5, case when $4 = 'sent' then $5::timestamptz end)`,
      [organizationId, incidentId, kind, state, updatedAt]);

    const expired = await incident("2024-01-01T00:00:00Z", before(prunes.incidents));
    await alert(expired, "opened", "pending", now.toISOString());
    const atCutoff = await incident("2024-01-01T00:00:00Z", prunes.incidents.toISOString());
    const stillOpen = await incident("2024-01-01T00:00:00Z", null);
    const recent = await incident("2026-10-01T00:00:00Z", "2026-10-01T01:00:00Z");
    await alert(recent, "opened", "sent", before(prunes.finishedAlerts));
    await alert(recent, "resolved", "pending", "2024-01-01T00:00:00Z");
    await alert(atCutoff, "opened", "expired", prunes.finishedAlerts.toISOString());
    await alert(atCutoff, "resolved", "failed", before(prunes.finishedAlerts));

    await pruneAll();

    const incidents = await harness.pool.query<{ id: string }>("select id from uptime_incident where check_id = $1", [a]);
    expect(new Set(incidents.rows.map((row) => row.id))).toEqual(new Set([atCutoff, stillOpen, recent]));
    const alerts = await harness.pool.query<{ incident_id: string; kind: string }>(
      "select a.incident_id, a.kind from uptime_alert a join uptime_incident i on i.id = a.incident_id where i.check_id = $1", [a]);
    expect(new Set(alerts.rows.map((row) => `${row.incident_id}:${row.kind}`)))
      .toEqual(new Set([`${recent}:resolved`, `${atCutoff}:opened`]));
  });

  it("deletes 5,000 rows per batch, and the cron batches until nothing expired is left", async () => {
    const a = await createCheck();
    await harness.pool.query(
      `insert into uptime_result (check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status,
         failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, agent)
       select $1, $2, 'eu', timestamptz '2026-01-01T00:00:00Z' + n * interval '1 second', 'routine', 0, 'pass', 200,
         'none', '', '', 0, 0, 0, 0, 1, 'agent-1'
       from generate_series(1, 10001) n`,
      [a, organizationId]);
    await result(a, "eu", new Date().toISOString(), "pass", 2);

    expect(await harness.runEffect(pruneUptimeBatch("raw", new Date()))).toBe(5000);
    const [, prune] = createUptimeMaintenanceFunctions(new Inngest({ id: "uptime-prune-test" }), harness.runEffect);
    if (prune === undefined) throw new Error("no prune function");
    await new InngestTestEngine({ function: prune }).execute();

    const left = await harness.pool.query<{ total_ms: number }>("select total_ms from uptime_result where check_id = $1", [a]);
    expect(left.rows).toEqual([{ total_ms: 2 }]);
  });
});
