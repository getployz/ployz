import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Database } from "#/server/database.server";
import { editUptimeCheckStatement } from "./incident-fence";
import { uptimeCheck } from "./tables";

/** What a check needs of a Service as config_applied stores it: the Store's SavedServiceIntent JSON. */
const AppliedService = Schema.fromJsonString(Schema.Struct({
  slug: Schema.String,
  config: Schema.Struct({
    healthcheck: Schema.Union([
      Schema.Struct({ type: Schema.Literal("http"), path: Schema.String }),
      Schema.Struct({ type: Schema.Literals(["none", "command"]) }),
    ]),
    managedHostnames: Schema.Array(Schema.Struct({ prefix: Schema.String })),
  }),
}));
const decodeAppliedService = Schema.decodeUnknownEffect(AppliedService);

/** The check a Service gets: its generated domain, at its HTTP healthcheck path or `/`. */
type WantedCheck = { readonly serviceId: string; readonly name: string; readonly url: string };

/**
 * The check `node` wants, or null when it has no generated domain. The hostname is expanded with the Cluster Domain
 * the applying Deployment froze, as the Store's own expansion does.
 */
const wantedCheck = Effect.fn("Uptime.wantedCheck")(function* (row: { serviceId: string; node: string; clusterDomain: string | null }) {
  const service = yield* decodeAppliedService(row.node);
  const [generated] = service.config.managedHostnames;
  if (generated === undefined || row.clusterDomain === null) return null;
  const path = service.config.healthcheck.type === "http" ? service.config.healthcheck.path : "/";
  return { serviceId: row.serviceId, name: service.slug, url: `https://${generated.prefix}.${row.clusterDomain}${path}` } satisfies WantedCheck;
});

/**
 * A URL change to a Service's check: a config edit, so it bumps the revision and closes an open incident as
 * check_changed, writing no alert. The table's interval, timeout and enabled stay as they are.
 */
const updateUptimeCheck = Effect.fn("Uptime.updateCheck")(function* (check: typeof uptimeCheck.$inferSelect, url: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(editUptimeCheckStatement({
    id: check.id, organizationId: check.organizationId, url,
    intervalSeconds: check.intervalSeconds, timeoutMs: check.timeoutMs, enabled: check.enabled,
  }));
});

/**
 * Make `environmentId`'s uptime checks match its Applied State: one per Service with a generated domain. Creates the
 * missing, updates a changed URL, deletes a check whose Service or domain is gone, and leaves the rest. Running it
 * again changes nothing. Syncs of one Environment run one at a time.
 */
export const syncUptimeChecks = Effect.fn("Uptime.sync")(function* (organizationId: string, environmentId: string) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // A Deployment's slot serializes its own syncs but not the backfill. With the lock, the later sync reads the
    // Applied State committed before it and so writes last.
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`uptime-sync:${environmentId}`}))`);
    const applied = yield* drizzle.execute<{ serviceId: string; node: string; clusterDomain: string | null }>(sql`
      select a.node_id as "serviceId", a.node, d.cluster_domain as "clusterDomain"
      from config_applied a join config_deployment d on d.id = a.deployment_id
      where a.environment_id = ${environmentId} and a.organization_id = ${organizationId} and a.node_type = 'service'
    `, "objects");
    const wanted = new Map<string, WantedCheck>();
    for (const row of applied) {
      const check = yield* wantedCheck(row);
      if (check !== null) wanted.set(check.serviceId, check);
    }
    const existing = yield* drizzle.select().from(uptimeCheck)
      .where(and(eq(uptimeCheck.organizationId, organizationId), eq(uptimeCheck.environmentId, environmentId)));
    for (const check of existing) {
      const want = wanted.get(check.serviceId);
      wanted.delete(check.serviceId);
      if (want === undefined) yield* drizzle.delete(uptimeCheck).where(eq(uptimeCheck.id, check.id));
      else if (want.url !== check.url) yield* updateUptimeCheck(check, want.url);
    }
    if (wanted.size > 0) {
      yield* drizzle.insert(uptimeCheck)
        .values([...wanted.values()].map(({ serviceId, name, url }) => ({ organizationId, environmentId, serviceId, name, url })));
    }
  }));
});

/**
 * Sync every Environment that has an applied Service. Deployments sync their own Environment, so this is for the
 * Services applied before uptime checks existed. Each sync is idempotent, so running it again changes nothing.
 */
export const backfillUptimeChecks = Effect.fn("Uptime.backfill")(function* () {
  const { drizzle } = yield* Database;
  const environments = yield* drizzle.execute<{ organizationId: string; environmentId: string }>(sql`
    select distinct organization_id as "organizationId", environment_id as "environmentId"
    from config_applied where node_type = 'service'
  `, "objects");
  for (const { organizationId, environmentId } of environments) yield* syncUptimeChecks(organizationId, environmentId);
  return environments.length;
});
