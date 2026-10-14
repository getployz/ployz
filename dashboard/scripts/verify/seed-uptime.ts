// Seeds uptime for the verify stack's `shop/production`, one Service per Monitoring tab state, without a checker.
// up.sh runs it in the background after seed.ts and writes .verify/run/uptime (ok or failed) when it ends.
// To rerun from dashboard/: set -a; . .verify/run/env; set +a; node scripts/verify/runner.mjs /scripts/verify/seed-uptime.ts
//   web       Up: 90 days of results, one closed 502 incident 12 days ago, an expired certificate in sin 3 days ago,
//             and a stray TLS or timeout failure every few hours, so every range's tooltip has failures to show
//   api       Unreachable: an open 502 incident from 18 minutes ago
//   worker    Checking: a check with no results yet
//   redis     Unreachable at its healthcheck path: every result a 404 since its first check 12 minutes ago, so the tab
//             shows the Healthcheck path field
//   postgres  Not checked: no check
// ams and iad get display names and sin keeps its code, so the tooltip shows both a name and the fallback.
// Each run replaces these checks wholesale (results and incidents cascade), so running it again converges.
// History older than a day goes straight into uptime_result_hourly, summed here the way the rollup sums raw probes, so
// the seed takes seconds instead of a minute of raw inserts and rollups. Its rows match what rollupUptime writes from
// the same probes, median rounding included.
import { randomUUID } from "node:crypto";
import { and, eq, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import { failureClass } from "#/modules/uptime/uptime";
import { FAILURE_CLASSES, FAILURE_WORDS, type FailureClass, type IncidentObservation } from "#/modules/uptime/uptime-status";
import { uptimeCheck, uptimeIncident, uptimeRegion, uptimeResult, uptimeResultHourly } from "#/modules/uptime/tables";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const INTERVAL = 5 * MINUTE;
const REGIONS = ["ams", "iad", "sin"] as const;
const REGION_NAMES = { ams: "Amsterdam", iad: "Virginia", sin: null } satisfies Record<(typeof REGIONS)[number], string | null>;
/** Each region's DNS, connect and TLS time: further from the origin, slower handshakes. */
const HANDSHAKES = [[4, 18, 31], [6, 40, 62], [9, 118, 170]] as const;

/** A steady, repeatable response time around `base` ms. */
const latency = (base: number, at: number, region: number) => Math.round(base + 40 * Math.sin(at / (7 * HOUR) + region) + (at / INTERVAL % 7) * 3);

/** A failing probe's answer, in the shape `failureClass` reads. */
type Failure = Pick<IncidentObservation, "status" | "failedPhase" | "error"> & { readonly detail: string };
const EXPIRED: Failure = { status: 0, failedPhase: "tls", error: "other", detail: "invalid peer certificate: Expired" };
const TIMEOUT: Failure = { status: 0, failedPhase: "request", error: "timed_out", detail: "operation timed out" };

/** One region's closed hour of probes, before it becomes an uptime_result_hourly row. */
type Hour = {
  readonly at: number;
  readonly passing: number[];
  readonly phases: Record<"dns" | "connect" | "tls" | "ttfb", number>;
  readonly failures: Record<FailureClass, number>;
  last: { readonly text: string; readonly cls: FailureClass; readonly at: number } | null;
};
const emptyHour = (at: number): Hour => ({
  at, passing: [], phases: { dns: 0, connect: 0, tls: 0, ttfb: 0 },
  failures: { status: 0, timeout: 0, refused: 0, dns: 0, connect: 0, tls: 0, other: 0 } satisfies Record<FailureClass, number>,
  last: null,
});

/** Postgres casts float to int half-to-even, so the rollup's `percentile_cont(0.5)::int` turns 162.5 into 162. */
const halfToEven = (value: number) => (value % 1 === 0.5 && Math.floor(value) % 2 === 0 ? Math.floor(value) : Math.round(value));

/** The rollup's sums over `hour`: totals over passing probes, the median rounded, each failure in its class. */
function hourlyRow(checkId: string, organizationId: string, region: string, hour: Hour): typeof uptimeResultHourly.$inferInsert {
  const passing = [...hour.passing].sort((a, b) => a - b);
  const middle = passing.length / 2;
  const failures = FAILURE_CLASSES.reduce((sum, cls) => sum + hour.failures[cls], 0);
  return {
    checkId, organizationId, region, hour: new Date(hour.at), probes: passing.length + failures, failures, inconclusive: 0,
    passMsSum: passing.reduce((sum, ms) => sum + ms, 0), passMsMax: passing.at(-1) ?? 0,
    passMsP50: passing.length === 0 ? null : passing.length % 2 === 1
      ? passing[Math.floor(middle)] ?? 0 : halfToEven(((passing[middle - 1] ?? 0) + (passing[middle] ?? 0)) / 2),
    dnsMsSum: hour.phases.dns, connectMsSum: hour.phases.connect, tlsMsSum: hour.phases.tls, ttfbMsSum: hour.phases.ttfb,
    failuresStatus: hour.failures.status, failuresTimeout: hour.failures.timeout, failuresRefused: hour.failures.refused,
    failuresDns: hour.failures.dns, failuresConnect: hour.failures.connect, failuresTls: hour.failures.tls,
    failuresOther: hour.failures.other,
    lastError: hour.last?.text ?? null, lastErrorClass: hour.last?.cls ?? null,
    lastErrorAt: hour.last === null ? null : new Date(hour.last.at),
  };
}
const BATCH = 1000;

const observation = (region: string, at: number, verdict: "pass" | "fail", status: number): IncidentObservation =>
  ({ region, kind: verdict === "fail" ? "confirm" : "recheck", verdict, status, failedPhase: null, error: null, remoteIp: "203.0.113.7", at: new Date(at).toISOString() });

const seed = Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const now = Math.floor(Date.now() / INTERVAL) * INTERVAL;

  const [environment] = yield* drizzle.execute<{ id: string; organizationId: string }>(sql`
    select e.id, e.organization_id as "organizationId"
    from config_environment e
    join config_project p on p.id = e.project_id
    join organization o on o.id::text = e.organization_id
    where o.slug = 'ada' and p.name = 'shop' and e.name = 'production'
  `, "objects");
  if (!environment) return yield* Effect.die("No ada/shop/production: run scripts/verify/up.sh first.");
  const { id: environmentId, organizationId } = environment;
  const services = new Map((yield* drizzle.execute<{ id: string; slug: string }>(sql`
    select node_id as id, node::json->>'slug' as slug
    from config_node_introduction where environment_id = ${environmentId} and node_type = 'service'
  `, "objects")).map((row) => [row.slug, row.id]));
  const serviceId = (slug: string) => {
    const id = services.get(slug);
    if (id === undefined) throw new Error(`No Service ${slug} in production.`);
    return id;
  };
  const slugs = ["web", "api", "worker", "redis", "postgres"] as const;

  yield* drizzle.insert(uptimeRegion).values(REGIONS.map((region) => ({ region, name: REGION_NAMES[region] })))
    .onConflictDoUpdate({ target: uptimeRegion.region, set: { name: sql`excluded.name` } });
  yield* drizzle.delete(uptimeCheck).where(and(
    eq(uptimeCheck.organizationId, organizationId),
    eq(uptimeCheck.environmentId, environmentId),
    inArray(uptimeCheck.serviceId, slugs.map(serviceId)),
  ));
  const checks = yield* drizzle.insert(uptimeCheck).values((["web", "api", "worker", "redis"] as const).map((slug) => ({
    organizationId, environmentId, serviceId: serviceId(slug), name: slug, url: `https://${slug}.acme.com/health`,
  }))).returning({ id: uptimeCheck.id, name: uptimeCheck.name });
  const checkId = (slug: string) => checks.find((check) => check.name === slug)?.id ?? "";

  const currentHour = Math.floor(now / HOUR) * HOUR;
  /**
   * Every result from `from` to now, failing with `status` inside `down` and with `fail`'s answer wherever it gives one.
   * Raw rows cover the last day, as a day-old prune would leave them, and every closed hour is summed into
   * uptime_result_hourly, as the rollup would.
   */
  const history = (slug: string, from: number, base: number, down: { from: number; to: number; status: number },
    fail: (at: number, region: number) => Failure | null = () => null) =>
    Effect.gen(function* () {
      const id = checkId(slug);
      const raw: (typeof uptimeResult.$inferInsert)[] = [];
      const hours = REGIONS.map(() => new Map<number, Hour>());
      for (let at = from; at < now; at += INTERVAL) {
        for (const [index, region] of REGIONS.entries()) {
          const [dnsMs, connectMs, tlsMs] = HANDSHAKES[index] ?? [0, 0, 0];
          const ms = latency(base, at, index) + connectMs + tlsMs;
          const failed: Failure | null = at >= down.from && at < down.to
            ? { status: down.status, failedPhase: null, error: null, detail: "" }
            : fail(at, index);
          const timing = failed?.failedPhase === "tls" ? { dnsMs, connectMs, tlsMs: 4, ttfbMs: 0, totalMs: dnsMs + connectMs + 4 }
            : failed?.error === "timed_out" ? { dnsMs, connectMs, tlsMs, ttfbMs: 0, totalMs: 10_000 }
            : { dnsMs, connectMs, tlsMs, ttfbMs: ms - dnsMs - connectMs - tlsMs, totalMs: failed ? 1_931 : ms };
          if (at >= now - DAY) {
            raw.push({
              checkId: id, organizationId, region, scheduledAt: new Date(at), kind: "routine", startLagMs: 12,
              verdict: failed ? "fail" : "pass", status: failed?.status ?? 200, failedPhase: failed?.failedPhase ?? "none",
              error: failed?.error ?? "", detail: failed?.detail ?? "", ...timing,
              remoteIp: "203.0.113.7", agent: `verify-${region}`,
            });
          }
          const hourAt = Math.floor(at / HOUR) * HOUR;
          const regionHours = hours[index];
          if (hourAt >= currentHour || regionHours === undefined) continue;
          const hour = regionHours.get(hourAt) ?? emptyHour(hourAt);
          regionHours.set(hourAt, hour);
          if (failed) {
            const cls = failureClass(failed);
            hour.failures[cls] += 1;
            const text = `${cls === "status" ? String(failed.status) : FAILURE_WORDS[cls]}${failed.detail ? ` · ${failed.detail}` : ""}`;
            hour.last = { text, cls, at };
          } else {
            hour.passing.push(timing.totalMs);
            hour.phases.dns += timing.dnsMs;
            hour.phases.connect += timing.connectMs;
            hour.phases.tls += timing.tlsMs;
            hour.phases.ttfb += timing.ttfbMs;
          }
        }
      }
      const hourly = REGIONS.flatMap((region, index) =>
        [...(hours[index]?.values() ?? [])].map((hour) => hourlyRow(id, organizationId, region, hour)));
      for (let index = 0; index < raw.length; index += BATCH) yield* drizzle.insert(uptimeResult).values(raw.slice(index, index + BATCH));
      for (let index = 0; index < hourly.length; index += BATCH) yield* drizzle.insert(uptimeResultHourly).values(hourly.slice(index, index + BATCH));
    });

  const closedFrom = now - 12 * DAY - 6 * HOUR;
  const closedTo = closedFrom + 20 * MINUTE;
  const expiredFrom = now - 3 * DAY - 5 * HOUR;
  yield* history("web", now - 90 * DAY, 140, { from: closedFrom, to: closedTo, status: 502 }, (at, region) => {
    const probe = at / INTERVAL;
    if (region === 2 && at >= expiredFrom && at < expiredFrom + 4 * HOUR) return EXPIRED;
    if (region === 2 && probe % 37 === 0) return EXPIRED;
    return region === 1 && probe % 53 === 0 ? TIMEOUT : null;
  });
  yield* drizzle.insert(uptimeIncident).values({
    id: randomUUID(), organizationId, checkId: checkId("web"), checkRevision: 1,
    openedAt: new Date(closedFrom), closedAt: new Date(closedTo),
    openedBy: { quorum: { distinctRegions: 2 }, observations: REGIONS.map((region) => observation(region, closedFrom, "fail", 502)) },
    closedBy: { kind: "observed", quorum: { oneRegion: { probes: 2, spacingMs: 30_000 } }, observations: [observation("ams", closedTo, "pass", 200)] },
  });

  const openFrom = now - 18 * MINUTE;
  yield* history("api", now - 7 * DAY, 90, { from: openFrom, to: now, status: 502 });
  yield* drizzle.insert(uptimeIncident).values({
    id: randomUUID(), organizationId, checkId: checkId("api"), checkRevision: 1, openedAt: new Date(openFrom),
    openedBy: { quorum: { distinctRegions: 2 }, observations: REGIONS.map((region) => observation(region, openFrom, "fail", 502)) },
  });

  const wrongFrom = now - 12 * MINUTE;
  yield* history("redis", wrongFrom, 60, { from: wrongFrom, to: now, status: 404 });
  yield* drizzle.insert(uptimeIncident).values({
    id: randomUUID(), organizationId, checkId: checkId("redis"), checkRevision: 1, openedAt: new Date(wrongFrom),
    openedBy: { quorum: { distinctRegions: 2 }, observations: REGIONS.map((region) => observation(region, wrongFrom, "fail", 404)) },
  });

  return { environmentId, checks: Object.fromEntries(slugs.map((slug) => [slug, checkId(slug) || "no check"])) };
});

export async function run() {
  console.log(`VERIFY_SEED_UPTIME ${JSON.stringify(await AppRuntime.runPromise(seed))}`);
}
