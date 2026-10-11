// Seeds uptime for the verify stack's `shop/production`, one Service per Monitoring tab state, without a checker.
// up.sh runs it after seed.ts. To rerun from dashboard/: set -a; . .verify/run/env; set +a; node scripts/verify/runner.mjs /scripts/verify/seed-uptime.ts
//   web       Up: 90 days of results, one closed 502 incident 12 days ago
//   api       Unreachable: an open 502 incident from 18 minutes ago
//   worker    Checking: a check with no results yet
//   postgres  Not checked: no check
// Each run replaces these checks wholesale (results and incidents cascade), so running it again converges.
import { randomUUID } from "node:crypto";
import { and, eq, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { IncidentObservation } from "#/modules/uptime/uptime-status";
import { uptimeCheck, uptimeIncident, uptimeResult, uptimeResultHourly } from "#/modules/uptime/tables";
import { Database } from "#/server/database.server";
import { AppRuntime } from "#/server/runtime.server";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const INTERVAL = 5 * MINUTE;
const REGIONS = ["eu-west", "us-east"] as const;

/** A steady, repeatable response time around `base` ms. */
const latency = (base: number, at: number, region: number) => Math.round(base + 40 * Math.sin(at / (7 * HOUR) + region) + (at / INTERVAL % 7) * 3);

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
  const slugs = ["web", "api", "worker", "postgres"] as const;

  yield* drizzle.delete(uptimeCheck).where(and(
    eq(uptimeCheck.organizationId, organizationId),
    eq(uptimeCheck.environmentId, environmentId),
    inArray(uptimeCheck.serviceId, slugs.map(serviceId)),
  ));
  const checks = yield* drizzle.insert(uptimeCheck).values((["web", "api", "worker"] as const).map((slug) => ({
    organizationId, environmentId, serviceId: serviceId(slug), name: slug, url: `https://${slug}.acme.com/health`,
  }))).returning({ id: uptimeCheck.id, name: uptimeCheck.name });
  const checkId = (slug: string) => checks.find((check) => check.name === slug)?.id ?? "";

  /** Every result from `from` to now, failing with `status` inside `down`, plus its hourly roll-up. */
  const history = (slug: string, from: number, base: number, down: { from: number; to: number; status: number }) =>
    Effect.gen(function* () {
      const id = checkId(slug);
      const raw: (typeof uptimeResult.$inferInsert)[] = [];
      const hourly = new Map<string, typeof uptimeResultHourly.$inferInsert & { ms: number[] }>();
      for (let at = from; at < now; at += INTERVAL) {
        for (const [index, region] of REGIONS.entries()) {
          const failing = at >= down.from && at < down.to;
          const ms = failing ? 1_931 : latency(base, at, index);
          const hour = Math.floor(at / HOUR) * HOUR;
          const key = `${region}:${hour}`;
          const row = hourly.get(key) ?? {
            checkId: id, organizationId, region, hour: new Date(hour), probes: 0, failures: 0, inconclusive: 0,
            passMsSum: 0, passMsMax: 0, ms: [],
          };
          row.probes += 1;
          if (failing) row.failures += 1;
          else row.ms.push(ms);
          hourly.set(key, row);
          if (at >= now - DAY) {
            raw.push({
              checkId: id, organizationId, region, scheduledAt: new Date(at), kind: "routine", startLagMs: 12,
              verdict: failing ? "fail" : "pass", status: failing ? down.status : 200, failedPhase: "none", error: "",
              detail: "", dnsMs: 4, connectMs: 18, tlsMs: 31, ttfbMs: ms - 8, totalMs: ms, remoteIp: "203.0.113.7",
              agent: `verify-${region}`,
            });
          }
        }
      }
      const rollup = [...hourly.values()].map(({ ms, ...row }) => {
        const sorted = [...ms].sort((a, b) => a - b);
        const at = (q: number) => sorted.length === 0 ? null : sorted[Math.min(sorted.length - 1, Math.floor(q * sorted.length))] ?? null;
        return { ...row, passMsSum: ms.reduce((sum, value) => sum + value, 0), passMsMax: sorted.at(-1) ?? 0, passMsP50: at(0.5), passMsP95: at(0.95) };
      });
      for (let index = 0; index < rollup.length; index += 1000) yield* drizzle.insert(uptimeResultHourly).values(rollup.slice(index, index + 1000));
      for (let index = 0; index < raw.length; index += 1000) yield* drizzle.insert(uptimeResult).values(raw.slice(index, index + 1000));
    });

  const closedFrom = now - 12 * DAY - 6 * HOUR;
  const closedTo = closedFrom + 20 * MINUTE;
  yield* history("web", now - 90 * DAY, 140, { from: closedFrom, to: closedTo, status: 502 });
  yield* drizzle.insert(uptimeIncident).values({
    id: randomUUID(), organizationId, checkId: checkId("web"), checkRevision: 1,
    openedAt: new Date(closedFrom), closedAt: new Date(closedTo),
    openedBy: { quorum: { distinctRegions: 2 }, observations: REGIONS.map((region) => observation(region, closedFrom, "fail", 502)) },
    closedBy: { kind: "observed", quorum: { oneRegion: { probes: 2, spacingMs: 30_000 } }, observations: [observation("eu-west", closedTo, "pass", 200)] },
  });

  const openFrom = now - 18 * MINUTE;
  yield* history("api", now - 7 * DAY, 90, { from: openFrom, to: now, status: 502 });
  yield* drizzle.insert(uptimeIncident).values({
    id: randomUUID(), organizationId, checkId: checkId("api"), checkRevision: 1, openedAt: new Date(openFrom),
    openedBy: { quorum: { distinctRegions: 2 }, observations: REGIONS.map((region) => observation(region, openFrom, "fail", 502)) },
  });

  return { environmentId, checks: Object.fromEntries(slugs.map((slug) => [slug, checkId(slug) || "no check"])) };
});

export async function run() {
  console.log(`VERIFY_SEED_UPTIME ${JSON.stringify(await AppRuntime.runPromise(seed))}`);
}
