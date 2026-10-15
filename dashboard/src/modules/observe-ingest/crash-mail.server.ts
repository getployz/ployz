import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { Effect } from "effect";
import { crashMail, type CrashPlace } from "#/modules/mail/templates/servers/crash";
import { queueOrganizationMail } from "#/modules/notifications/recipients.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/** A Service that keeps crashing mails at most once in this window. */
const MAIL_GAP_MS = 3_600_000;
/** Crash events older than this are deleted. */
const RETAIN = "30 days";
export const PRUNE_BATCH_ROWS = 5_000;

export type CrashClaim =
  | { readonly kind: "none" }
  | { readonly kind: "wait"; readonly until: string }
  | { readonly kind: "mailed"; readonly count: number };

type Unmailed = {
  id: string; machineId: string; serviceName: string; exitCode: number | null; oom: boolean; occurredMs: number;
};

type Context = {
  organizationName: string; organizationSlug: string; project: string | null; environment: string | null; slug: string | null;
};

/** The runtime name after the `<namespace>/` the marker qualifies it with. */
const runtimeName = (serviceName: string) => serviceName.slice(serviceName.indexOf("/") + 1);
const namespaceOf = (serviceName: string) => serviceName.slice(0, Math.max(serviceName.indexOf("/"), 0));

/** The Organization, and the Project and Environment that own the namespace with the Service's applied slug, if any. */
const loadContext = Effect.fn("CrashMail.context")(function* (organizationId: string, serviceId: string, serviceName: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<Context>(sql`
    select o.name as "organizationName", o.slug as "organizationSlug", placed.project, placed.environment, placed.slug
    from organization o
      left join lateral (
        select p.name as project, e.name as environment, (a.node::jsonb)->>'slug' as slug
        from config_namespace n
          join config_environment e on e.id = n.environment_id
          join config_project p on p.id = e.project_id
          left join config_applied a on a.environment_id = e.id and a.node_id = ${serviceId} and a.node_type = 'service'
        where n.organization_id = ${organizationId} and n.namespace = ${namespaceOf(serviceName)}
      ) placed on true
    where o.id = ${organizationId}`, "objects");
  return row ?? null;
});

/**
 * Claim one Service's unmailed crashes and queue their email. A Service mailed within the last hour waits: the claim
 * returns when it may mail again and leaves the crashes for then. The mail is queued before the claim commits, keyed by
 * the first crash it counts, so a retry after a failed commit re-queues the same mail and send-mail drops the copy.
 */
export const claimCrashMail = Effect.fn("CrashMail.claim")(function* (
  organizationId: string,
  serviceId: string,
  now: Date,
  serverNames: ReadonlyMap<string, string>,
) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const unmailed = yield* drizzle.execute<Unmailed>(sql`
      select id, machine_id as "machineId", service_name as "serviceName", exit_code::int as "exitCode", oom,
             (extract(epoch from occurred_at) * 1000)::float8 as "occurredMs"
      from crash_event
      where organization_id = ${organizationId} and service_id = ${serviceId} and mailed_at is null
      order by received_at, id
      for update skip locked`, "objects");
    const [first] = unmailed;
    const last = unmailed.reduce<Unmailed | undefined>(
      (latest, row) => latest === undefined || row.occurredMs >= latest.occurredMs ? row : latest,
      undefined,
    );
    if (first === undefined || last === undefined) return { kind: "none" } satisfies CrashClaim;

    const [mailed] = yield* drizzle.execute<{ ms: number | null }>(sql`
      select (extract(epoch from max(mailed_at)) * 1000)::float8 as ms from crash_event
      where organization_id = ${organizationId} and service_id = ${serviceId}`, "objects");
    const lastMailed = mailed?.ms ?? null;
    if (lastMailed !== null && now.getTime() - lastMailed < MAIL_GAP_MS) {
      return { kind: "wait", until: new Date(lastMailed + MAIL_GAP_MS).toISOString() } satisfies CrashClaim;
    }

    const context = yield* loadContext(organizationId, serviceId, last.serviceName);
    if (context !== null) {
      const place: CrashPlace | null = context.project === null || context.environment === null
        ? null
        : { project: context.project, environment: context.environment, serviceId };
      const config = yield* AppConfig;
      yield* queueOrganizationMail({
        organizationId,
        type: "crash",
        key: `crash:${first.id}`,
        content: crashMail({
          organization: { name: context.organizationName, slug: context.organizationSlug },
          service: context.slug ?? runtimeName(last.serviceName),
          place,
          server: { machineId: last.machineId, name: serverNames.get(last.machineId) ?? last.machineId.slice(0, 12) },
          count: unmailed.length,
          last: { exitCode: last.exitCode, oom: last.oom, at: new Date(last.occurredMs) },
          origin: config.app.url.origin,
        }),
      });
    }
    const ids = unmailed.map(({ id }) => id);
    yield* drizzle.execute(sql`
      update crash_event set mailed_at = ${now.toISOString()}::timestamptz
      where id in (${sql.join(ids.map((id) => sql`${id}::uuid`), sql`, `)})`);
    return { kind: "mailed", count: unmailed.length } satisfies CrashClaim;
  }));
});

/** One batch of crash events older than 30 days; how many it deleted. */
export const pruneCrashEvents = Effect.fn("CrashMail.prune")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const deleted = yield* drizzle.execute<{ id: string }>(sql`
    delete from crash_event where id in (
      select id from crash_event where occurred_at < ${now.toISOString()}::timestamptz - ${RETAIN}::interval
      limit ${PRUNE_BATCH_ROWS})
    returning id`, "objects");
  return deleted.length;
});
