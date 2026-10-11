import { randomUUID } from "node:crypto";
import { Effect, Schema } from "effect";
import { TestClock } from "effect/testing";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import contract from "#/modules/uptime/incident-evidence.fixture.json";
import { IncidentEvidence, UPTIME_RANGES, type UptimeRange } from "#/modules/uptime/uptime-status";
import { readEnvironmentUptime, readServiceUptime } from "#/modules/uptime/uptime-status.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const acme = "00000000-0000-4000-8000-0000000e0001";
const other = "00000000-0000-4000-8000-0000000e0002";
const userId = "00000000-0000-4000-8000-0000000e0003";
const actor = { userId };
const NOW = Date.parse("2026-10-11T12:30:00.000Z");
const HOUR = 3_600_000;
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
const service = (scope: Scope, range: UptimeRange = "24h") =>
  at(readServiceUptime(actor, { organizationSlug: "acme", ...scope, range }));
const series = async (check: Check, range: UptimeRange) => (await summary(check, range)).points;

async function summary(check: Check, range: UptimeRange = "24h") {
  const uptime = await service(check, range);
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
  totalMs: number, kind: "routine" | "confirm" | "recheck" = "routine") {
  return harness.pool.query(
    `insert into uptime_result (check_id, organization_id, region, scheduled_at, kind, start_lag_ms, verdict, status,
       failed_phase, error, detail, dns_ms, connect_ms, tls_ms, ttfb_ms, total_ms, agent)
     values ($1, $2, $3, $4, $5, 0, $6, $7, $8, '', '', 0, 0, 0, 0, $9, 'agent-1')`,
    [check.id, check.organizationId, region, scheduledAt, kind, verdict, verdict === "pass" ? 200 : 0,
      verdict === "fail" ? "connect" : "none", totalMs],
  );
}

function hourly(check: Check, region: string, hour: string, probes: number, failures: number, inconclusive: number,
  p50: number | null = null) {
  return harness.pool.query(
    `insert into uptime_result_hourly values ($1, $2, $3, $4, $5, $6, $7, 0, 0, $8, $8)`,
    [check.id, check.organizationId, region, hour, probes, failures, inconclusive, p50],
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
    for (const range of UPTIME_RANGES) expect(await service(unchecked, range)).toBeNull();
  });
});

describe("uptime percentages", () => {
  it("are passing over pass plus fail probes, summed across regions, over the range", async () => {
    const check = await createCheck();
    const noise = await createCheck();
    await hourly(check, "eu-west", hourAgo(2), 10, 1, 5);
    await hourly(check, "us-east", hourAgo(3), 10, 0, 0);
    await hourly(check, "eu-west", hourAgo(3 * 24), 20, 10, 0);
    await hourly(check, "eu-west", hourAgo(30 * 24), 60, 0, 0);
    await hourly(check, "eu-west", hourAgo(91 * 24), 100, 100, 0);
    await hourly(noise, "eu-west", hourAgo(2), 100, 100, 0);

    expect((await summary(check, "24h")).uptime).toBe(19 / 20);
    expect((await summary(check, "7d")).uptime).toBe(29 / 40);
    expect((await summary(check, "90d")).uptime).toBe(89 / 100);
  });

  it("count raw routine probes after the last rolled-up hour", async () => {
    const check = await createCheck();
    await hourly(check, "eu-west", hourAgo(2), 4, 0, 0);
    await result(check, "eu-west", ago(2 * HOUR), "fail", 9000);
    await result(check, "eu-west", ago(HOUR), "fail", 9000);
    await result(check, "eu-west", ago(10 * 60_000), "pass", 120);
    await result(check, "eu-west", ago(9 * 60_000), "inconclusive", 0);
    await result(check, "eu-west", ago(8 * 60_000), "fail", 9000, "confirm");
    for (const range of UPTIME_RANGES) expect((await summary(check, range)).uptime).toBe(5 / 6);
  });

  it("leave inconclusive probes out, and are null when no probe counted", async () => {
    const check = await createCheck();
    await hourly(check, "eu-west", hourAgo(2), 0, 0, 12);
    for (const range of UPTIME_RANGES) expect((await summary(check, range)).uptime).toBeNull();

    await hourly(check, "us-east", hourAgo(2), 4, 1, 40);
    for (const range of UPTIME_RANGES) expect((await summary(check, range)).uptime).toBe(3 / 4);
  });
});

describe("uptime chart", () => {
  it("draws 24h from raw routine probes, with null for every probe that did not pass", async () => {
    const check = await createCheck();
    await result(check, "us-east", ago(2 * HOUR), "fail", 9000);
    await result(check, "eu-west", ago(2 * HOUR), "pass", 140);
    await result(check, "eu-west", ago(HOUR), "inconclusive", 0);
    await result(check, "eu-west", ago(HOUR - 1000), "fail", 9000, "confirm");
    await result(check, "eu-west", ago(25 * HOUR), "pass", 99);
    await hourly(check, "eu-west", hourAgo(2), 1, 0, 0, 555);

    expect(await series(check, "24h")).toEqual([
      { at: ago(2 * HOUR), region: "eu-west", ms: 140 },
      { at: ago(2 * HOUR), region: "us-east", ms: null },
      { at: ago(HOUR), region: "eu-west", ms: null },
    ]);
  });

  it("draws 7d from the hourly medians, then raw probes after the last rolled-up hour", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(3 * HOUR), "pass", 130);
    await result(check, "eu-west", ago(HOUR), "pass", 140);
    await hourly(check, "eu-west", hourAgo(2), 4, 0, 0, 210);
    await hourly(check, "us-east", hourAgo(2), 4, 4, 0, null);
    await hourly(check, "eu-west", hourAgo(6 * 24), 4, 0, 0, 180);
    await hourly(check, "eu-west", hourAgo(8 * 24), 4, 0, 0, 170);

    expect(await series(check, "7d")).toEqual([
      { at: hourAgo(6 * 24), region: "eu-west", ms: 180 },
      { at: hourAgo(2), region: "eu-west", ms: 210 },
      { at: hourAgo(2), region: "us-east", ms: null },
      { at: ago(HOUR), region: "eu-west", ms: 140 },
    ]);
  });

  it("draws a check's only probe before any hour rolled up, in every range", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(10 * 60_000), "pass", 120);
    for (const range of UPTIME_RANGES) {
      expect(await series(check, range)).toEqual([{ at: ago(10 * 60_000), region: "eu-west", ms: 120 }]);
    }
  });

  it("draws 90d in 6-hour buckets per region, averaging the medians by passing probes", async () => {
    const check = await createCheck();
    // hourAgo(240) is 12:00 UTC, so it and the next five hours share one bucket.
    await hourly(check, "eu-west", hourAgo(240), 4, 0, 1, 100);
    await hourly(check, "eu-west", hourAgo(239), 4, 2, 0, 400);
    await hourly(check, "eu-west", hourAgo(235), 3, 3, 0, null);
    await hourly(check, "eu-west", hourAgo(234), 4, 0, 0, 900);
    await hourly(check, "us-east", hourAgo(240), 2, 2, 0, null);
    await hourly(check, "eu-west", hourAgo(91 * 24), 4, 0, 0, 160);

    expect(await series(check, "90d")).toEqual([
      { at: hourAgo(240), region: "eu-west", ms: 200 },
      { at: hourAgo(240), region: "us-east", ms: null },
      { at: hourAgo(234), region: "eu-west", ms: 900 },
    ]);
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
    const ids = async (range: UptimeRange) => (await summary(check, range)).incidents.map((entry) => entry.id);
    expect(await ids("24h")).toEqual([recent, open]);
    expect(await ids("7d")).toEqual([recent, lastWeek, open]);
    expect(await ids("90d")).toEqual([recent, lastWeek, open, old]);
    expect((await summary(check, "24h")).incidents).toEqual([
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
    expect(await refusal(readServiceUptime(actor, { ...input, range: "24h" }))).toMatchObject({ _tag: "NotFound" });
  });

  it("shows nothing of another organization's environment under the actor's slug", async () => {
    const foreign = await createCheck(other);
    await incident(foreign, ago(HOUR));
    await result(foreign, "eu-west", ago(HOUR), "pass", 120);
    await hourly(foreign, "eu-west", hourAgo(2), 4, 0, 0, 120);
    expect(await environment(foreign.environmentId)).toEqual([]);
    for (const range of UPTIME_RANGES) expect(await service(foreign, range)).toBeNull();
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
