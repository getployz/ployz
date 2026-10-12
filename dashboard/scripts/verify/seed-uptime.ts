// Seeds uptime for the verify stack's `shop/production`, one Service per Monitoring tab state, without a checker.
// up.sh runs it after seed.ts. To rerun from dashboard/: set -a; . .verify/run/env; set +a; node scripts/verify/runner.mjs /scripts/verify/seed-uptime.ts
//   web       Up: 90 days of results, one closed 502 incident 12 days ago, an expired certificate in sin 3 days ago,
//             and a stray TLS or timeout failure every few hours, so every range's tooltip has failures to show
//   api       Unreachable: an open 502 incident from 18 minutes ago
//   worker    Checking: a check with no results yet
//   redis     Unreachable at its healthcheck path: every result a 404 since its first check 12 minutes ago, so the tab
//             shows the Healthcheck path field
//   postgres  Not checked: no check
// ams and iad get display names and sin keeps its code, so the tooltip shows both a name and the fallback.
// Each run replaces these checks wholesale (results and incidents cascade), so running it again converges.
import { randomUUID } from "node:crypto";
import { and, eq, inArray, lt, sql } from "drizzle-orm";
import { Effect } from "effect";
import { rollupUptime } from "#/modules/uptime/maintenance.server";
import type { IncidentObservation } from "#/modules/uptime/uptime-status";
import { uptimeCheck, uptimeIncident, uptimeRegion, uptimeResult } from "#/modules/uptime/tables";
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

type Probe = Pick<typeof uptimeResult.$inferInsert, "verdict" | "status" | "failedPhase" | "error" | "detail">;
const EXPIRED: Probe = { verdict: "fail", status: 0, failedPhase: "tls", error: "other", detail: "invalid peer certificate: Expired" };
const TIMEOUT: Probe = { verdict: "fail", status: 0, failedPhase: "request", error: "timed_out", detail: "operation timed out" };

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

  /**
   * Every result from `from` to now, failing with `status` inside `down` and with `fail`'s probe wherever it gives one,
   * rolled up by the real rollup. Raw rows older than a day go once rolled up, as retention would leave them.
   */
  const history = (slug: string, from: number, base: number, down: { from: number; to: number; status: number },
    fail: (at: number, region: number) => Probe | null = () => null) =>
    Effect.gen(function* () {
      const id = checkId(slug);
      const raw: (typeof uptimeResult.$inferInsert)[] = [];
      for (let at = from; at < now; at += INTERVAL) {
        for (const [index, region] of REGIONS.entries()) {
          const [dnsMs, connectMs, tlsMs] = HANDSHAKES[index] ?? [0, 0, 0];
          const ms = latency(base, at, index) + connectMs + tlsMs;
          const failed = at >= down.from && at < down.to
            ? { verdict: "fail", status: down.status, failedPhase: "none", error: "", detail: "" } satisfies Probe
            : fail(at, index);
          const timing = failed?.failedPhase === "tls" ? { dnsMs, connectMs, tlsMs: 4, ttfbMs: 0, totalMs: dnsMs + connectMs + 4 }
            : failed?.error === "timed_out" ? { dnsMs, connectMs, tlsMs, ttfbMs: 0, totalMs: 10_000 }
            : { dnsMs, connectMs, tlsMs, ttfbMs: ms - dnsMs - connectMs - tlsMs, totalMs: failed ? 1_931 : ms };
          raw.push({
            checkId: id, organizationId, region, scheduledAt: new Date(at), kind: "routine", startLagMs: 12,
            ...(failed ?? { verdict: "pass", status: 200, failedPhase: "none", error: "", detail: "" }), ...timing,
            remoteIp: "203.0.113.7", agent: `verify-${region}`,
          });
        }
      }
      for (let index = 0; index < raw.length; index += 1000) yield* drizzle.insert(uptimeResult).values(raw.slice(index, index + 1000));
      for (let hour = Math.floor(from / HOUR) * HOUR; hour < now; hour += 2 * HOUR) yield* rollupUptime(new Date(hour + 2 * HOUR));
      yield* drizzle.delete(uptimeResult).where(and(eq(uptimeResult.checkId, id), lt(uptimeResult.scheduledAt, new Date(now - DAY))));
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
