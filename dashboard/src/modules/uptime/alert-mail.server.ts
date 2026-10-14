import "@tanstack/react-start/server-only";
import { sql } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { queueOrganizationMail } from "#/modules/notifications/recipients.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { uptimeAlertMail, type UptimeAlert } from "./alert-mail";
import { IncidentEvidence } from "./uptime-status";

/** Older than this, an alert tells of an incident too late to act on, so it never sends. */
const MAX_AGE = "1 hour";
/** A row a run claimed but never finished, because the run died, is claimable again after this. */
const LEASE = "5 minutes";
const MAX_ATTEMPTS = 5;
const CLAIM_BATCH = 100;

type Claimed = { id: string; organizationId: string; incidentId: string; kind: "opened" | "resolved"; createdAt: string };

type IncidentRow = {
  openedAt: string; closedAt: string | null; openedBy: unknown; checkName: string; checkUrl: string; serviceId: string;
  environment: string; project: string; organizationSlug: string; organizationName: string;
};

const settle = (id: string, state: "sent" | "expired" | "failed" | "pending", now: Date) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`
    update uptime_alert set state = ${state}, updated_at = ${now.toISOString()}::timestamptz,
      attempts = case when ${state} = 'pending' then attempts - 1 else attempts end,
      sent_at = case when ${state} = 'sent' then ${now.toISOString()}::timestamptz end
    where id = ${id} and state = 'sending'`);
});

const loadAlert = Effect.fn("Uptime.loadAlert")(function* (row: Claimed) {
  const { drizzle } = yield* Database;
  const [incident] = yield* drizzle.execute<IncidentRow>(sql`
    select i.opened_at as "openedAt", i.closed_at as "closedAt", i.opened_by as "openedBy", c.name as "checkName",
           c.url as "checkUrl", c.service_id as "serviceId", e.name as environment, p.name as project,
           o.slug as "organizationSlug", o.name as "organizationName"
    from uptime_incident i
      join uptime_check c on c.id = i.check_id
      join organization o on o.id = i.organization_id
      join config_environment e on e.id = c.environment_id
      join config_project p on p.id = e.project_id
    where i.id = ${row.incidentId}`, "objects");
  if (incident === undefined) return Option.none<UptimeAlert>();
  const evidence = Schema.decodeUnknownOption(IncidentEvidence)(incident.openedBy);
  const base = {
    organization: { slug: incident.organizationSlug, name: incident.organizationName },
    project: incident.project,
    environment: incident.environment,
    service: { id: incident.serviceId, name: incident.checkName },
    checkUrl: incident.checkUrl,
    incidentId: row.incidentId,
    observations: Option.match(evidence, { onNone: () => [], onSome: (value) => value.observations }),
    openedAt: new Date(incident.openedAt),
  };
  if (row.kind === "opened") return Option.some<UptimeAlert>({ ...base, kind: "opened" });
  if (incident.closedAt === null) return Option.none<UptimeAlert>();
  return Option.some<UptimeAlert>({ ...base, kind: "resolved", closedAt: new Date(incident.closedAt) });
});

/** Whether the opened alert of `incidentId` went out, so its recovery has something to follow. */
const openedState = Effect.fn("Uptime.openedState")(function* (incidentId: string) {
  const { drizzle } = yield* Database;
  const [opened] = yield* drizzle.execute<{ state: string }>(sql`
    select state from uptime_alert where incident_id = ${incidentId} and kind = 'opened'`, "objects");
  return opened?.state ?? null;
});

const sendOne = Effect.fn("Uptime.sendAlert")(function* (row: Claimed, now: Date) {
  if (row.kind === "resolved") {
    const opened = yield* openedState(row.incidentId);
    if (opened === "pending" || opened === "sending") return yield* settle(row.id, "pending", now);
    if (opened !== "sent") return yield* settle(row.id, "expired", now);
  }
  const alert = yield* loadAlert(row);
  if (Option.isNone(alert)) return yield* settle(row.id, "failed", now);
  const config = yield* AppConfig;
  yield* queueOrganizationMail({
    organizationId: row.organizationId,
    type: "uptime",
    key: `uptime:${row.incidentId}:${row.kind}`,
    content: uptimeAlertMail(alert.value, config.app.url.origin, now),
  });
  yield* settle(row.id, "sent", now);
});

/**
 * `send-uptime-alerts`: expire alerts older than an hour, claim the rest, and queue each one's email to every member who
 * wants uptime email. A claim moves a row to `sending`, so two runs never mail one alert twice.
 */
export const sendUptimeAlerts = Effect.fn("Uptime.sendAlerts")(function* (now: Date) {
  const { drizzle } = yield* Database;
  const at = now.toISOString();
  yield* drizzle.execute(sql`
    update uptime_alert set state = 'expired', updated_at = ${at}::timestamptz
    where state in ('pending', 'sending') and created_at < ${at}::timestamptz - ${MAX_AGE}::interval`);
  const claimed = yield* drizzle.execute<Claimed>(sql`
    update uptime_alert set state = 'sending', attempts = attempts + 1, updated_at = ${at}::timestamptz
    where id in (
      select id from uptime_alert
      where (state = 'pending' or (state = 'sending' and updated_at < ${at}::timestamptz - ${LEASE}::interval))
        and attempts < ${MAX_ATTEMPTS}
      order by created_at
      limit ${CLAIM_BATCH}
      for update skip locked)
    returning id, organization_id as "organizationId", incident_id as "incidentId", kind, created_at::text as "createdAt"`, "objects");
  const ordered = [...claimed].sort((a, b) =>
    a.createdAt.localeCompare(b.createdAt) || (a.kind === b.kind ? 0 : a.kind === "opened" ? -1 : 1));
  yield* Effect.forEach(ordered, (row) => sendOne(row, now).pipe(
    Effect.catchCause((cause) => Effect.logWarning("An uptime alert didn't queue; a later run retries it.", { id: row.id, cause })),
  ));
  return ordered.length;
});
