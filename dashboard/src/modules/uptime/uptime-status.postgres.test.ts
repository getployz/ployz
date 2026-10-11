import { randomUUID } from "node:crypto";
import { Effect, Schema } from "effect";
import { TestClock } from "effect/testing";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import contract from "#/modules/uptime/incident-evidence.fixture.json";
import { mergePoints, tally } from "#/modules/uptime/uptime";
import { FAILURE_CLASSES, type FailureClass, IncidentEvidence, PHASES, type UptimePoint } from "#/modules/uptime/uptime-status";
import { readEnvironmentUptime, readServiceUptime } from "#/modules/uptime/uptime-status.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const acme = "00000000-0000-4000-8000-0000000e0001";
const other = "00000000-0000-4000-8000-0000000e0002";
const userId = "00000000-0000-4000-8000-0000000e0003";
const actor = { userId };
const NOW = Date.parse("2026-10-11T12:30:00.000Z");
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const ago = (ms: number) => new Date(NOW - ms).toISOString();
/** The start of the hour `hours` before the current one. */
const hourAgo = (hours: number) => new Date(Math.floor(NOW / HOUR) * HOUR - hours * HOUR).toISOString();
const openedBy = JSON.stringify(contract.openedBy);
const closedBy = JSON.stringify(contract.observedClosedBy);

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
  await harness.pool.query(`
    insert into organization (id, name, slug) values ('${acme}', 'Acme', 'acme'), ('${other}', 'Other', 'other');
    insert into "user" (id, email, name) values ('${userId}', 'member@example.com', 'Member');
    insert into member (id, organization_id, user_id, role, created_at)
    values (gen_random_uuid(), '${acme}', '${userId}', 'member', now());
  `);
}, 60_000);

afterAll(async () => {
  await harness?.stop();
});

/** Runs `effect` with the clock at NOW. */
function at<A, E>(effect: Effect.Effect<A, E, Database>) {
  return harness.runEffect(Effect.gen(function* () {
    yield* TestClock.setTime(NOW);
    return yield* effect;
  }).pipe(Effect.provide(TestClock.layer())));
}

type Scope = { environmentId: string; serviceId: string };

const environment = (environmentId: string) => at(readEnvironmentUptime(actor, { organizationSlug: "acme", environmentId }));
/** The ranges the tab offers back from now, by their From edge. */
const RANGES = ["1d", "7d", "90d"];
const service = (scope: Scope, from = "1d", to = "now") =>
  at(readServiceUptime(actor, { organizationSlug: "acme", ...scope, from, to }));
const series = async (check: Check, from: string) => (await summary(check, from)).points;

async function summary(check: Check, from = "1d", to = "now") {
  const uptime = await service(check, from, to);
  if (uptime === null) throw new Error("no check");
  return uptime;
}

async function createCheck(organizationId = acme, environmentId = randomUUID()) {
  const { rows } = await harness.pool.query<{ id: string; service_id: string }>(
    `insert into uptime_check (organization_id, name, url, environment_id, service_id, interval_seconds, timeout_ms)
     values ($1, 'web', 'https://shop.example.com/', $2, gen_random_uuid()::text, 60, 5000)
     returning id, service_id`,
    [organizationId, environmentId],
  );
  const [row] = rows;
  if (row === undefined) throw new Error("no check");
  return { id: row.id, serviceId: row.service_id, environmentId, organizationId };
}
type Check = Awaited<ReturnType<typeof createCheck>>;

function result(check: Check, region: string, scheduledAt: string, verdict: "pass" | "fail" | "inconclusive",
  totalMs: number, kind: "routine" | "confirm" | "recheck" = "routine", status = verdict === "pass" ? 200 : 0) {
  return harness.pool.query(
    `insert into uptime_result (check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status,
       failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, agent)
     values ($1, $2, $3, $4, $5, 0, $6, $7, $8, '', $9, 1, 2, 3, $10, $10 + 6, 'agent-1')`,
    [check.id, check.organizationId, region, scheduledAt, kind, verdict, status,
      verdict === "fail" && status === 0 ? "connect" : "none", verdict === "fail" && status === 0 ? "refused by peer" : "",
      totalMs - 6],
  );
}

type Hour = Omit<UptimePoint, "medianMs" | "maxMs" | "at"> & { at: string; medianMs: number | null; maxMs: number };

/** An hour as both the rollup row and the UptimePoint it stands for, so a test can fold it with mergePoints. */
const hour = (region: string, at: string, probes: number, failed: Partial<Record<FailureClass, number>> = {},
  medianMs: number | null = null, patch: Partial<Hour> = {}): Hour => ({
  at, region, probes, failures: tally(FAILURE_CLASSES, (cls) => failed[cls] ?? 0), medianMs, maxMs: medianMs ?? 0,
  phaseMsSum: tally(PHASES, () => 0), lastError: null, ...patch,
});

function hourly(check: Check, region: string, at: string, probes: number, failures: number, inconclusive: number,
  p50: number | null = null) {
  return insertHour(check, hour(region, at, probes, { other: failures }, p50), inconclusive);
}

function insertHour(check: Check, row: Hour, inconclusive = 0) {
  const failures = FAILURE_CLASSES.map((cls) => row.failures[cls]);
  return harness.pool.query(
    `insert into uptime_result_hourly (check_id, organization_id, region, hour, probes, failures, inconclusive, pass_ms_sum,
       pass_ms_max, pass_ms_p50, dns_ms_sum, connect_ms_sum, tls_ms_sum, ttfb_ms_sum,
       ${FAILURE_CLASSES.map((cls) => `failures_${cls}`).join(", ")}, last_error, last_error_class, last_error_at)
     values ($1, $2, $3, $4, $5, $6, $7, 0, $8, $9, $10, $11, $12, $13, ${failures.map((_, index) => `$${index + 14}`).join(", ")},
       $21, $22, $23)`,
    [check.id, check.organizationId, row.region, row.at, row.probes, failures.reduce((a, b) => a + b, 0), inconclusive,
      row.maxMs, row.medianMs, ...PHASES.map((phase) => row.phaseMsSum[phase]), ...failures,
      row.lastError?.text ?? null, row.lastError?.cls ?? null, row.lastError?.at ?? null],
  );
}

async function incident(check: Check, openedAt: string, closedAt: string | null = null) {
  const id = randomUUID();
  await harness.pool.query(
    `insert into uptime_incident (id, organization_id, check_id, check_revision, opened_at, opened_by, closed_at, closed_by)
     values ($1, $2, $3, 1, $4, $5, $6, $7)`,
    [id, check.organizationId, check.id, openedAt, openedBy, closedAt, closedAt === null ? null : closedBy],
  );
  return id;
}

const close = (id: string, closedAt: string) => harness.pool.query(
  "update uptime_incident set closed_at = $2, closed_by = $3 where id = $1", [id, closedAt, closedBy]);

describe("uptime status", () => {
  it("reads pending before any probe, up after one, unreachable while an incident is open, and up once it closes", async () => {
    const check = await createCheck();
    const listed = async () => (await environment(check.environmentId)).map((entry) => entry.openIncidentAt);
    expect((await summary(check)).status).toBe("pending");

    await result(check, "eu-west", ago(10 * 60_000), "pass", 120);
    expect((await summary(check)).status).toBe("up");
    expect(await listed()).toEqual([null]);

    const id = await incident(check, ago(5 * 60_000));
    expect((await summary(check)).status).toBe("unreachable");
    expect(await listed()).toEqual([ago(5 * 60_000)]);

    await close(id, ago(60_000));
    expect((await summary(check)).status).toBe("up");
    expect(await listed()).toEqual([null]);
  });

  it("lists every check of the environment with when its open incident opened, and no other environment's", async () => {
    const environmentId = randomUUID();
    const first = await createCheck(acme, environmentId);
    const second = await createCheck(acme, environmentId);
    const quiet = await createCheck(acme, environmentId);
    const elsewhere = await createCheck();
    await incident(first, ago(2 * HOUR));
    await incident(second, ago(HOUR));
    await incident(quiet, ago(3 * HOUR), ago(2 * HOUR));
    await incident(elsewhere, ago(HOUR));

    const expected = [
      { serviceId: first.serviceId, url: "https://shop.example.com/", openIncidentAt: ago(2 * HOUR) },
      { serviceId: second.serviceId, url: "https://shop.example.com/", openIncidentAt: ago(HOUR) },
      { serviceId: quiet.serviceId, url: "https://shop.example.com/", openIncidentAt: null },
    ].sort((a, b) => a.serviceId.localeCompare(b.serviceId));
    expect(await environment(environmentId)).toEqual(expected);
  });

  it("returns the check's configuration", async () => {
    const check = await createCheck();
    expect((await summary(check)).check).toEqual({ id: check.id, url: "https://shop.example.com/", intervalSeconds: 60 });
  });

  it("reads null for a Service without a check, in every range", async () => {
    const check = await createCheck();
    await hourly(check, "eu-west", hourAgo(2), 4, 0, 0, 120);
    const unchecked = { environmentId: check.environmentId, serviceId: randomUUID() };
    for (const range of RANGES) expect(await service(unchecked, range)).toBeNull();
  });
});

describe("uptime percentages", () => {
  /** `count` routine results a second apart, ending `ms` ago. */
  const results = (check: Check, region: string, ms: number, verdict: "pass" | "fail" | "inconclusive", count: number) =>
    Promise.all(Array.from({ length: count }, (_, index) => result(check, region, ago(ms + index * 1000), verdict, 100)));

  it("are passing over pass plus fail probes, summed across regions: raw where it is kept, the hourly rollup before", async () => {
    const check = await createCheck();
    const noise = await createCheck();
    await results(check, "eu-west", 2 * HOUR, "pass", 9);
    await results(check, "eu-west", 2 * HOUR + MINUTE, "fail", 1);
    await results(check, "us-east", 3 * HOUR, "pass", 10);
    await results(check, "eu-west", 3 * DAY, "pass", 10);
    await results(check, "eu-west", 3 * DAY + MINUTE, "fail", 10);
    await hourly(check, "eu-west", hourAgo(2), 100, 100, 0);
    await hourly(check, "eu-west", hourAgo(40 * 24), 60, 0, 0);
    await hourly(check, "eu-west", hourAgo(91 * 24), 100, 100, 0);
    await results(noise, "eu-west", HOUR, "fail", 5);

    expect((await summary(check, "1d")).uptime).toBe(19 / 20);
    expect((await summary(check, "7d")).uptime).toBe(29 / 40);
    expect((await summary(check, "90d")).uptime).toBe(89 / 100);
  });

  it("count only the hours that end by Until in a range past raw retention", async () => {
    const check = await createCheck();
    await hourly(check, "eu-west", hourAgo(40 * 24 + 1), 10, 0, 0);
    await hourly(check, "eu-west", hourAgo(40 * 24), 10, 10, 0);
    const until = new Date(Date.parse(hourAgo(40 * 24)) + 30 * MINUTE).toISOString();
    const from = new Date(Date.parse(hourAgo(40 * 24 + 2))).toISOString();

    expect((await summary(check, from, until)).uptime).toBe(1);
  });

  it("count raw probes from the raw retention cutoff when Until falls before the next hour", async () => {
    const check = await createCheck();
    await hourly(check, "eu-west", hourAgo(30 * 24 + 1), 10, 0, 0);
    await hourly(check, "eu-west", hourAgo(30 * 24), 10, 10, 0);
    await result(check, "eu-west", ago(30 * DAY - 10 * MINUTE), "fail", 9000);

    expect((await summary(check, ago(30 * DAY + 2 * HOUR), ago(30 * DAY - 15 * MINUTE))).uptime).toBe(10 / 11);
  });

  it("count routine probes only", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(20 * MINUTE), "fail", 9000);
    await result(check, "eu-west", ago(10 * MINUTE), "pass", 120);
    await result(check, "eu-west", ago(8 * MINUTE), "fail", 9000, "confirm");
    await result(check, "eu-west", ago(7 * MINUTE), "fail", 9000, "recheck");
    for (const from of ["5m", ...RANGES]) expect((await summary(check, from)).uptime).toBe(1 / 2);
  });

  it("leave inconclusive probes out, and are null when no probe counted", async () => {
    const check = await createCheck();
    await results(check, "eu-west", HOUR, "inconclusive", 12);
    for (const from of RANGES) expect((await summary(check, from)).uptime).toBeNull();

    await results(check, "us-east", HOUR, "pass", 3);
    await results(check, "us-east", 2 * HOUR, "fail", 1);
    for (const from of RANGES) expect((await summary(check, from)).uptime).toBe(3 / 4);
  });
});

describe("uptime chart", () => {
  const brief = (points: readonly UptimePoint[]) =>
    points.map(({ at, region, probes, medianMs }) => ({ at, region, probes, medianMs }));

  it("draws the last day from raw routine probes, one probe a point, a failure in the class and words its probe gave", async () => {
    const check = await createCheck();
    await result(check, "us-east", ago(2 * HOUR), "fail", 9000);
    await result(check, "eu-west", ago(2 * HOUR), "pass", 140);
    await result(check, "eu-west", ago(HOUR), "inconclusive", 0);
    await result(check, "eu-west", ago(HOUR - 1000), "fail", 9000, "confirm");
    await result(check, "eu-west", ago(30 * 60_000), "fail", 400, "routine", 503);
    await result(check, "eu-west", ago(25 * HOUR), "pass", 99);
    await hourly(check, "eu-west", hourAgo(2), 1, 0, 0, 555);

    const points = await series(check, "1d");
    expect(brief(points)).toEqual([
      { at: ago(2 * HOUR), region: "eu-west", probes: 1, medianMs: 140 },
      { at: ago(2 * HOUR), region: "us-east", probes: 1, medianMs: null },
      { at: ago(HOUR), region: "eu-west", probes: 0, medianMs: null },
      { at: ago(30 * 60_000), region: "eu-west", probes: 1, medianMs: null },
    ]);
    expect(points[0]).toMatchObject({ maxMs: 140, phaseMsSum: { dns: 1, connect: 2, tls: 3, ttfb: 134 }, lastError: null });
    expect(points[1]).toMatchObject({
      maxMs: null, phaseMsSum: { dns: 0, connect: 0, tls: 0, ttfb: 0 }, failures: tally(FAILURE_CLASSES, (cls) => cls === "connect" ? 1 : 0),
      lastError: { at: ago(2 * HOUR), cls: "connect", text: "Connect · refused by peer" },
    });
    expect(points[3]).toMatchObject({ failures: { status: 1 }, lastError: { cls: "status", text: "503" } });
  });

  it("draws 7d from the hourly rows, then raw probes after the last rolled-up hour", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(3 * HOUR), "pass", 130);
    await result(check, "eu-west", ago(HOUR), "pass", 140);
    const failing = hour("us-east", hourAgo(2), 4, { tls: 3, timeout: 1 }, null, {
      lastError: { at: ago(2 * HOUR + 60_000), cls: "timeout", text: "Timeout" } });
    await insertHour(check, failing);
    await hourly(check, "eu-west", hourAgo(2), 4, 0, 0, 210);
    await hourly(check, "eu-west", hourAgo(6 * 24), 4, 0, 0, 180);
    await hourly(check, "eu-west", hourAgo(8 * 24), 4, 0, 0, 170);

    const points = await series(check, "7d");
    expect(brief(points)).toEqual([
      { at: hourAgo(6 * 24), region: "eu-west", probes: 4, medianMs: 180 },
      { at: hourAgo(2), region: "eu-west", probes: 4, medianMs: 210 },
      { at: hourAgo(2), region: "us-east", probes: 4, medianMs: null },
      { at: ago(HOUR), region: "eu-west", probes: 1, medianMs: 140 },
    ]);
    expect(points[2]).toEqual({ ...failing, maxMs: null });
  });

  it("draws a check's only probe before any hour rolled up, in every range", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(10 * 60_000), "pass", 120);
    for (const range of RANGES) {
      expect(brief(await series(check, range))).toEqual([{ at: ago(10 * 60_000), region: "eu-west", probes: 1, medianMs: 120 }]);
    }
  });

  it("draws 90d in 6-hour buckets per region, each the hours folded by mergePoints", async () => {
    const check = await createCheck();
    const error = (at: string, cls: FailureClass, text: string) => ({ lastError: { at, cls, text } });
    // hourAgo(240) is 12:00 UTC, so it and the next five hours share one bucket.
    const first = [
      hour("eu-west", hourAgo(240), 4, {}, 100, { maxMs: 180, phaseMsSum: { dns: 8, connect: 40, tls: 60, ttfb: 300 } }),
      hour("eu-west", hourAgo(239), 4, { tls: 2 }, 400, { maxMs: 610, phaseMsSum: { dns: 4, connect: 20, tls: 30, ttfb: 700 },
        ...error(ago(239 * HOUR), "tls", "TLS · invalid peer certificate: Expired") }),
      hour("eu-west", hourAgo(235), 3, { timeout: 3 }, null, error(ago(235 * HOUR), "timeout", "Timeout")),
      hour("eu-west", hourAgo(236), 2, { status: 1 }, 50, error(ago(236 * HOUR), "status", "503")),
    ];
    const next = [hour("eu-west", hourAgo(234), 4, {}, 900)];
    const down = [hour("us-east", hourAgo(240), 2, { dns: 2 }, null, error(ago(240 * HOUR), "dns", "DNS"))];
    for (const row of [...first, ...next, ...down, hour("eu-west", hourAgo(91 * 24), 4, {}, 160)]) await insertHour(check, row);

    const fold = (rows: readonly Hour[]) => {
      const merged = rows.map((row): UptimePoint => ({ ...row, maxMs: row.medianMs === null ? null : row.maxMs }))
        .reduce(mergePoints);
      return { ...merged, at: new Date(merged.at).toISOString() };
    };
    expect(await series(check, "90d")).toEqual([fold(first), fold(down), fold(next)]);
    expect(fold(first)).toMatchObject({ probes: 13, medianMs: (100 * 4 + 400 * 2 + 50) / 7, maxMs: 610 });
  });
});

describe("check strip", () => {
  it("lists every result of any kind in at least the last 30 minutes, oldest first", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(40 * 60_000), "pass", 110);
    await result(check, "eu-west", ago(20 * 60_000), "fail", 9000);
    await result(check, "us-east", ago(20 * 60_000), "fail", 9000, "confirm");
    await result(check, "eu-west", ago(5 * 60_000), "inconclusive", 0, "recheck");

    const checks = (await summary(check, "5m")).checks;
    expect(checks?.map((entry) => [entry.at, entry.region, entry.kind, entry.verdict])).toEqual([
      [ago(20 * 60_000), "eu-west", "routine", "fail"],
      [ago(20 * 60_000), "us-east", "confirm", "fail"],
      [ago(5 * 60_000), "eu-west", "recheck", "inconclusive"],
    ]);
    expect((await summary(check, "1h")).checks).toHaveLength(4);
  });

  it("keeps the newest 120 results", async () => {
    const check = await createCheck();
    for (let minute = 1; minute <= 125; minute++) await result(check, "eu-west", ago(minute * 60_000), "pass", 100);
    const checks = (await summary(check, "6h")).checks ?? [];
    expect(checks).toHaveLength(120);
    expect([checks[0]?.at, checks.at(-1)?.at]).toEqual([ago(120 * 60_000), ago(60_000)]);
  });

  it("leaves results out past 6 hours, so the strip draws spans", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(10 * 60_000), "pass", 120);
    expect((await summary(check, "1d")).checks).toBeNull();
  });
});

describe("retention", () => {
  it("hides raw results past retention, so a check probed only before it reads pending", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(31 * DAY), "pass", 120);
    expect((await summary(check)).status).toBe("pending");

    await result(check, "eu-west", ago(29 * DAY), "pass", 120);
    expect((await summary(check)).status).toBe("up");
  });
});

describe("uptime incidents", () => {
  it("lists the incidents that overlap the range newest first, with the observations that opened them", async () => {
    const check = await createCheck();
    const old = await incident(check, ago(20 * DAY), ago(10 * DAY));
    const lastWeek = await incident(check, ago(3 * DAY), ago(30 * HOUR));
    const recent = await incident(check, ago(5 * HOUR), ago(2 * HOUR));
    const open = await incident(check, ago(10 * DAY));
    await incident(check, ago(100 * DAY), ago(91 * DAY));
    await incident(check, ago(-HOUR), ago(-2 * HOUR));

    const observations = contract.openedBy.observations;
    const ids = async (from: string) => (await summary(check, from)).incidents.map((entry) => entry.id);
    expect(await ids("1d")).toEqual([recent, open]);
    expect(await ids("7d")).toEqual([recent, lastWeek, open]);
    expect(await ids("90d")).toEqual([recent, lastWeek, open, old]);
    expect((await summary(check, "1d")).incidents).toEqual([
      { id: recent, openedAt: ago(5 * HOUR), closedAt: ago(2 * HOUR), observations },
      { id: open, openedAt: ago(10 * DAY), closedAt: null, observations },
    ]);
  });

  it("treats opened_by it can't parse as a defect", async () => {
    const check = await createCheck();
    await harness.pool.query(
      `insert into uptime_incident (id, organization_id, check_id, check_revision, opened_at, opened_by)
       values ($1, $2, $3, 1, $4, $5)`,
      [randomUUID(), acme, check.id, ago(HOUR), JSON.stringify({ ...contract.openedBy, region: "eu-west" })],
    );
    await expect(summary(check)).rejects.toThrow(/opened_by the dashboard can't read/);
  });
});

describe("authorization", () => {
  const refusal = <A, E>(effect: Effect.Effect<A, E, Database>) => at(Effect.flip(effect));

  it("refuses another organization under its own slug", async () => {
    const foreign = await createCheck(other);
    const input = { organizationSlug: "other", environmentId: foreign.environmentId, serviceId: foreign.serviceId };
    expect(await refusal(readEnvironmentUptime(actor, input))).toMatchObject({ _tag: "NotFound" });
    expect(await refusal(readServiceUptime(actor, { ...input, from: "1d", to: "now" }))).toMatchObject({ _tag: "NotFound" });
  });

  it("shows nothing of another organization's environment under the actor's slug", async () => {
    const foreign = await createCheck(other);
    await incident(foreign, ago(HOUR));
    await result(foreign, "eu-west", ago(HOUR), "pass", 120);
    await hourly(foreign, "eu-west", hourAgo(2), 4, 0, 0, 120);
    expect(await environment(foreign.environmentId)).toEqual([]);
    for (const range of RANGES) expect(await service(foreign, range)).toBeNull();
  });
});

describe("opened_by contract", () => {
  const decode = Schema.decodeUnknownSync(IncidentEvidence);

  it("parses the JSON the Rust incident writer writes, key for key", () => {
    expect(decode(contract.openedBy, { onExcessProperty: "error" })).toEqual(contract.openedBy);
    const { kind, ...evidence } = contract.observedClosedBy;
    expect(kind).toBe("observed");
    expect(decode(evidence, { onExcessProperty: "error" })).toEqual(evidence);
  });

  it("refuses a key or a spelling it does not know", () => {
    const [first] = contract.openedBy.observations;
    for (const changed of [{ ...first, ip: "203.0.113.7" }, { ...first, error: "refused" }, { ...first, failed_phase: "connect" }]) {
      expect(() => decode({ ...contract.openedBy, observations: [changed] }, { onExcessProperty: "error" })).toThrow();
    }
  });
});
