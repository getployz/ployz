import { sql } from "drizzle-orm";
import {
  bigint,
  boolean,
  check,
  foreignKey,
  index,
  inet,
  integer,
  jsonb,
  pgTable,
  primaryKey,
  smallint,
  text,
  timestamp,
  uniqueIndex,
  uuid,
} from "drizzle-orm/pg-core";
import { createdAt, type JsonObject, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";
import { FAILURE_CLASSES, type FailureClass } from "./uptime-status";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

export const uptimeCheck = pgTable(
  "uptime_check",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    url: text("url").notNull(),
    intervalSeconds: integer("interval_seconds").default(300).notNull(),
    timeoutMs: integer("timeout_ms").default(10000).notNull(),
    expect: text("expect").default("below_400").notNull().$type<"below_400">(),
    enabled: boolean("enabled").default(true).notNull(),
    // The config fence: incident open and close apply only at the revision the detector judged.
    revision: integer("revision").default(1).notNull(),
    source: text("source").default("service").notNull().$type<"service">(),
    // The Config Store's ids are text. The check lives while config_applied holds this Service (environment_id,
    // node_id): a trigger the Store open attaches deletes it with that row, environment deletes included.
    environmentId: text("environment_id").notNull(),
    serviceId: text("service_id").notNull(),
    // When a deploy or Check now last asked for a probe. Writing it leaves revision alone, so it never closes an incident.
    probeRequestedAt: timestamptz("probe_requested_at"),
    // The request the checker last finished judging, to the millisecond. Only the checker writes it.
    probeAnsweredAt: timestamptz("probe_answered_at"),
    createdAt,
    updatedAt,
  },
  (table) => [
    index("uptime_check_organization_idx").on(table.organizationId),
    // The target of the children's composite foreign keys: a child row can't name another organization's check.
    uniqueIndex("uptime_check_id_organization_idx").on(table.id, table.organizationId),
    uniqueIndex("uptime_check_service_idx").on(table.environmentId, table.serviceId),
    check("uptime_check_name_check", sql`length(${table.name}) between 1 and 128`),
    // A backstop only; the URL parser in the write path is the real gate.
    check("uptime_check_url_check", sql`${table.url} ~ '^https?://[^/?#@[:space:]]+([/?]|$)' and length(${table.url}) <= 2048`),
    check("uptime_check_interval_check", sql`${table.intervalSeconds} between 30 and 3600`),
    check("uptime_check_timeout_check",
      sql`${table.timeoutMs} between 1000 and 30000 and ${table.timeoutMs} < ${table.intervalSeconds} * 1000`),
    check("uptime_check_expect_check", sql`${table.expect} in ('below_400')`),
    check("uptime_check_revision_check", sql`${table.revision} >= 1`),
    check("uptime_check_source_check", sql`${table.source} in ('service')`),
  ],
);

export const uptimeIncident = pgTable(
  "uptime_incident",
  {
    // Minted by the control plane when it decides to open, so the later close names it exactly.
    id: uuid("id").primaryKey(),
    organizationId: uuid("organization_id").notNull(),
    checkId: uuid("check_id").notNull(),
    checkRevision: integer("check_revision").notNull(),
    openedAt: timestamptz("opened_at").notNull(),
    openedBy: jsonb("opened_by").notNull().$type<JsonObject>(),
    closedAt: timestamptz("closed_at"),
    closedBy: jsonb("closed_by").$type<JsonObject>(),
  },
  (table) => [
    foreignKey({
      name: "uptime_incident_check_fk",
      columns: [table.checkId, table.organizationId],
      foreignColumns: [uptimeCheck.id, uptimeCheck.organizationId],
    }).onDelete("cascade"),
    uniqueIndex("uptime_incident_one_open_idx").on(table.checkId).where(sql`${table.closedAt} is null`),
    index("uptime_incident_history_idx").on(table.checkId, table.openedAt.desc()),
    index("uptime_incident_closed_idx").on(table.closedAt).where(sql`${table.closedAt} is not null`),
    check("uptime_incident_closed_check", sql`(${table.closedAt} is null) = (${table.closedBy} is null)`),
    check("uptime_incident_order_check", sql`${table.closedAt} is null or ${table.closedAt} >= ${table.openedAt}`),
  ],
);

export const uptimeAlert = pgTable(
  "uptime_alert",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    incidentId: uuid("incident_id")
      .notNull()
      .references(() => uptimeIncident.id, { onDelete: "cascade" }),
    kind: text("kind").notNull().$type<"opened" | "resolved">(),
    payload: jsonb("payload").notNull().$type<JsonObject>(),
    state: text("state").default("pending").notNull().$type<"pending" | "sending" | "sent" | "failed" | "expired">(),
    attempts: integer("attempts").default(0).notNull(),
    createdAt,
    updatedAt,
    sentAt: timestamptz("sent_at"),
  },
  (table) => [
    // The sender's idempotency key is `${incident_id}:${kind}`.
    uniqueIndex("uptime_alert_transition_idx").on(table.incidentId, table.kind),
    index("uptime_alert_pending_idx").on(table.createdAt).where(sql`${table.state} in ('pending', 'sending')`),
    check("uptime_alert_kind_check", sql`${table.kind} in ('opened', 'resolved')`),
    check("uptime_alert_state_check", sql`${table.state} in ('pending', 'sending', 'sent', 'failed', 'expired')`),
    check("uptime_alert_sent_check", sql`(${table.state} = 'sent') = (${table.sentAt} is not null)`),
  ],
);

/**
 * What the dashboard calls each probe region, written by the control plane at boot from UPTIME_REGIONS. Probe rows keep
 * the code, so a rename relabels history. A region no longer configured keeps its row, and with it its label.
 */
export const uptimeRegion = pgTable("uptime_region", {
  region: text("region").primaryKey(),
  name: text("name"),
});

export const uptimeResult = pgTable(
  "uptime_result",
  {
    checkId: uuid("check_id").notNull(),
    organizationId: uuid("organization_id").notNull(),
    region: text("region").notNull(),
    scheduledAt: timestamptz("scheduled_at").notNull(),
    kind: text("kind").notNull().$type<"routine" | "confirm" | "recheck" | "triggered">(),
    startLagMs: integer("start_lag_ms").notNull(),
    verdict: text("verdict").notNull().$type<"pass" | "fail" | "inconclusive">(),
    // 0 when there was no HTTP response.
    status: smallint("status").notNull(),
    failedPhase: text("failed_phase").notNull().$type<"none" | "dns" | "connect" | "tls" | "request">(),
    error: text("error").notNull(),
    detail: text("detail").notNull(),
    dnsMs: integer("dns_ms").notNull(),
    connectMs: integer("connect_ms").notNull(),
    tlsMs: integer("tls_ms").notNull(),
    ttfbMs: integer("ttfb_ms").notNull(),
    totalMs: integer("total_ms").notNull(),
    remoteIp: inet("remote_ip"),
    certNotAfter: timestamptz("cert_not_after"),
    agent: text("agent").notNull(),
  },
  (table) => [
    // The probe's identity, so a re-sent batch is a no-op.
    primaryKey({ columns: [table.checkId, table.region, table.scheduledAt, table.kind] }),
    foreignKey({
      name: "uptime_result_check_fk",
      columns: [table.checkId, table.organizationId],
      foreignColumns: [uptimeCheck.id, uptimeCheck.organizationId],
    }).onDelete("cascade"),
    index("uptime_result_scheduled_brin").using("brin", table.scheduledAt),
    check("uptime_result_kind_check", sql`${table.kind} in ('routine', 'confirm', 'recheck', 'triggered')`),
    check("uptime_result_verdict_check", sql`${table.verdict} in ('pass', 'fail', 'inconclusive')`),
    check("uptime_result_phase_check", sql`${table.failedPhase} in ('none', 'dns', 'connect', 'tls', 'request')`),
  ],
);

export const uptimeResultHourly = pgTable(
  "uptime_result_hourly",
  {
    checkId: uuid("check_id").notNull(),
    organizationId: uuid("organization_id").notNull(),
    region: text("region").notNull(),
    hour: timestamptz("hour").notNull(),
    probes: integer("probes").notNull(),
    failures: integer("failures").notNull(),
    // Not observed: never a failure.
    inconclusive: integer("inconclusive").notNull(),
    passMsSum: bigint("pass_ms_sum", { mode: "number" }).notNull(),
    passMsMax: integer("pass_ms_max").notNull(),
    passMsP50: integer("pass_ms_p50"),
    dnsMsSum: bigint("dns_ms_sum", { mode: "number" }).default(0).notNull(),
    connectMsSum: bigint("connect_ms_sum", { mode: "number" }).default(0).notNull(),
    tlsMsSum: bigint("tls_ms_sum", { mode: "number" }).default(0).notNull(),
    ttfbMsSum: bigint("ttfb_ms_sum", { mode: "number" }).default(0).notNull(),
    // `failures` split by FAILURE_CLASSES. Hours rolled up before these columns existed read 0 in each.
    failuresStatus: integer("failures_status").default(0).notNull(),
    failuresTimeout: integer("failures_timeout").default(0).notNull(),
    failuresRefused: integer("failures_refused").default(0).notNull(),
    failuresDns: integer("failures_dns").default(0).notNull(),
    failuresConnect: integer("failures_connect").default(0).notNull(),
    failuresTls: integer("failures_tls").default(0).notNull(),
    failuresOther: integer("failures_other").default(0).notNull(),
    lastError: text("last_error"),
    lastErrorClass: text("last_error_class").$type<FailureClass>(),
    lastErrorAt: timestamptz("last_error_at"),
  },
  (table) => [
    primaryKey({ columns: [table.checkId, table.region, table.hour] }),
    check("uptime_result_hourly_last_error_check", sql`(${table.lastError} is null) = (${table.lastErrorClass} is null)
      and (${table.lastError} is null) = (${table.lastErrorAt} is null)`),
    check("uptime_result_hourly_last_error_class_check",
      sql`${table.lastErrorClass} in (${sql.raw(FAILURE_CLASSES.map((cls) => `'${cls}'`).join(", "))})`),
    foreignKey({
      name: "uptime_result_hourly_check_fk",
      columns: [table.checkId, table.organizationId],
      foreignColumns: [uptimeCheck.id, uptimeCheck.organizationId],
    }).onDelete("cascade"),
    index("uptime_result_hourly_brin").using("brin", table.hour),
  ],
);
