import { randomUUID } from "node:crypto";
import { Effect, Schema } from "effect";
import { TestClock } from "effect/testing";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import contract from "#/modules/uptime/incident-evidence.fixture.json";
import { IncidentEvidence, type UptimeRange } from "#/modules/uptime/uptime-status";
import { readOpenUptimeIncidents, readUptimeCheck } from "#/modules/uptime/uptime-status.server";
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

const detail = (checkId: string, range: UptimeRange = "24h") =>
  at(readUptimeCheck(actor, { organizationSlug: "acme", checkId, range }));

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
    const open = () => at(readOpenUptimeIncidents(actor, { organizationSlug: "acme", environmentId: check.environmentId }));
    expect((await detail(check.id)).status).toBe("pending");

    await result(check, "eu-west", ago(10 * 60_000), "pass", 120);
    expect((await detail(check.id)).status).toBe("up");
    expect(await open()).toEqual([]);

    const id = await incident(check, ago(5 * 60_000));
    expect((await detail(check.id)).status).toBe("unreachable");
    expect(await open()).toEqual([
      { checkId: check.id, serviceId: check.serviceId, incidentId: id, openedAt: ago(5 * 60_000) },
    ]);

    await close(id, ago(60_000));
    expect((await detail(check.id)).status).toBe("up");
    expect(await open()).toEqual([]);
  });

  it("lists every open check of the environment and no other environment's", async () => {
    const environmentId = randomUUID();
    const first = await createCheck(acme, environmentId);
    const second = await createCheck(acme, environmentId);
    const quiet = await createCheck(acme, environmentId);
    const elsewhere = await createCheck();
    const older = await incident(first, ago(2 * HOUR));
    const newer = await incident(second, ago(HOUR));
    await incident(quiet, ago(3 * HOUR), ago(2 * HOUR));
    await incident(elsewhere, ago(HOUR));

    const open = await at(readOpenUptimeIncidents(actor, { organizationSlug: "acme", environmentId }));
    expect(open.map((entry) => entry.incidentId)).toEqual([newer, older]);
  });

  it("returns the check's configuration", async () => {
    const check = await createCheck();
    expect((await detail(check.id)).check).toEqual({
      id: check.id, name: "web", url: "https://shop.example.com/", serviceId: check.serviceId,
      intervalSeconds: 60, timeoutMs: 5000, expect: "below_400", enabled: true,
    });
  });
});

describe("uptime percentages", () => {
  it("are passing over pass plus fail probes, summed across regions, per window", async () => {
    const check = await createCheck();
    const noise = await createCheck();
    await hourly(check, "eu-west", hourAgo(2), 10, 1, 5);
    await hourly(check, "us-east", hourAgo(3), 10, 0, 0);
    await hourly(check, "eu-west", hourAgo(3 * 24), 20, 10, 0);
    await hourly(check, "eu-west", hourAgo(30 * 24), 60, 0, 0);
    await hourly(check, "eu-west", hourAgo(91 * 24), 100, 100, 0);
    await hourly(noise, "eu-west", hourAgo(2), 100, 100, 0);

    expect((await detail(check.id)).uptime).toEqual({ "24h": 19 / 20, "7d": 29 / 40, "90d": 89 / 100 });
  });

  it("leave inconclusive probes out, and are null when no probe counted", async () => {
    const check = await createCheck();
    await hourly(check, "eu-west", hourAgo(2), 0, 0, 12);
    expect((await detail(check.id)).uptime).toEqual({ "24h": null, "7d": null, "90d": null });

    await hourly(check, "us-east", hourAgo(2), 4, 1, 40);
    expect((await detail(check.id)).uptime).toEqual({ "24h": 3 / 4, "7d": 3 / 4, "90d": 3 / 4 });
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

    expect((await detail(check.id, "24h")).series).toEqual([
      { at: ago(2 * HOUR), region: "eu-west", ms: 140 },
      { at: ago(2 * HOUR), region: "us-east", ms: null },
      { at: ago(HOUR), region: "eu-west", ms: null },
    ]);
  });

  it("draws 7d and 90d from the hourly medians", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(HOUR), "pass", 140);
    await hourly(check, "eu-west", hourAgo(2), 4, 0, 0, 210);
    await hourly(check, "us-east", hourAgo(2), 4, 4, 0, null);
    await hourly(check, "eu-west", hourAgo(6 * 24), 4, 0, 0, 180);
    await hourly(check, "eu-west", hourAgo(8 * 24), 4, 0, 0, 170);
    await hourly(check, "eu-west", hourAgo(91 * 24), 4, 0, 0, 160);

    const recent = [
      { at: hourAgo(6 * 24), region: "eu-west", ms: 180 },
      { at: hourAgo(2), region: "eu-west", ms: 210 },
      { at: hourAgo(2), region: "us-east", ms: null },
    ];
    expect((await detail(check.id, "7d")).series).toEqual(recent);
    expect((await detail(check.id, "90d")).series).toEqual([{ at: hourAgo(8 * 24), region: "eu-west", ms: 170 }, ...recent]);
  });
});

describe("retention", () => {
  it("hides raw results past retention, so a check probed only before it reads pending", async () => {
    const check = await createCheck();
    await result(check, "eu-west", ago(31 * DAY), "pass", 120);
    expect((await detail(check.id)).status).toBe("pending");

    await result(check, "eu-west", ago(29 * DAY), "pass", 120);
    expect((await detail(check.id)).status).toBe("up");
  });
});

describe("uptime incidents", () => {
  it("lists the incidents that overlap the range newest first, with the observations that opened them", async () => {
    const check = await createCheck();
    const old = await incident(check, ago(20 * DAY), ago(10 * DAY));
    const recent = await incident(check, ago(5 * HOUR), ago(2 * HOUR));
    const open = await incident(check, ago(HOUR));
    await incident(check, ago(-HOUR), ago(-2 * HOUR));

    const observations = contract.openedBy.observations;
    const recentRows = [
      { id: open, openedAt: ago(HOUR), closedAt: null, observations },
      { id: recent, openedAt: ago(5 * HOUR), closedAt: ago(2 * HOUR), observations },
    ];
    expect((await detail(check.id, "7d")).incidents).toEqual(recentRows);
    expect((await detail(check.id, "90d")).incidents).toEqual([
      ...recentRows,
      { id: old, openedAt: ago(20 * DAY), closedAt: ago(10 * DAY), observations },
    ]);
  });

  it("treats opened_by it can't parse as a defect", async () => {
    const check = await createCheck();
    await harness.pool.query(
      `insert into uptime_incident (id, organization_id, check_id, check_revision, opened_at, opened_by)
       values ($1, $2, $3, 1, $4, $5)`,
      [randomUUID(), acme, check.id, ago(HOUR), JSON.stringify({ ...contract.openedBy, region: "eu-west" })],
    );
    await expect(detail(check.id)).rejects.toThrow(/opened_by the dashboard can't read/);
  });
});

describe("authorization", () => {
  const refusal = <A, E>(effect: Effect.Effect<A, E, Database>) => at(Effect.flip(effect));

  it("refuses another organization's check, under either slug", async () => {
    const foreign = await createCheck(other);
    await incident(foreign, ago(HOUR));
    for (const organizationSlug of ["acme", "other"]) {
      expect(await refusal(readUptimeCheck(actor, { organizationSlug, checkId: foreign.id, range: "24h" })))
        .toMatchObject({ _tag: "NotFound" });
    }
  });

  it("refuses another organization, and shows nothing of another organization's environment", async () => {
    const foreign = await createCheck(other);
    await incident(foreign, ago(HOUR));
    expect(await refusal(readOpenUptimeIncidents(actor, { organizationSlug: "other", environmentId: foreign.environmentId })))
      .toMatchObject({ _tag: "NotFound" });
    expect(await at(readOpenUptimeIncidents(actor, { organizationSlug: "acme", environmentId: foreign.environmentId })))
      .toEqual([]);
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
