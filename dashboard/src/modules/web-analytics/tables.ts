import { sql } from "drizzle-orm";
import { bigint, boolean, bytea, check, date, doublePrecision, pgSchema, primaryKey, smallint, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

export const analytics = pgSchema("analytics");

export const analyticsSite = analytics.table(
  "site",
  {
    organizationId: text("organization_id").notNull(),
    environmentId: text("environment_id").notNull(),
    enabled: boolean("enabled").default(false).notNull(),
    timezone: text("timezone").default("UTC").notNull(),
    updatedAt: timestamptz("updated_at").defaultNow().notNull(),
  },
  (table) => [primaryKey({ columns: [table.organizationId, table.environmentId] })],
);

export const analyticsSalt = analytics.table(
  "salt",
  {
    organizationId: text("organization_id").notNull(),
    environmentId: text("environment_id").notNull(),
    day: date("day", { mode: "string" }).notNull(),
    salt: bytea("salt").notNull(),
    createdAt: timestamptz("created_at").defaultNow().notNull(),
  },
  (table) => [primaryKey({ columns: [table.organizationId, table.environmentId, table.day] })],
);

export const REQUEST_KINDS = ["pageview", "bot", "other"] as const;
export type RequestKind = (typeof REQUEST_KINDS)[number];

// Partitioned by day on ts in the migration; drizzle-kit cannot declare partitions.
export const analyticsRequest = analytics.table(
  "request",
  {
    environmentId: text("environment_id").notNull(),
    ts: timestamptz("ts").notNull(),
    requestId: text("request_id").notNull(),
    service: text("service").notNull(),
    host: text("host").notNull(),
    method: text("method").notNull(),
    path: text("path").notNull(),
    status: smallint("status").notNull(),
    durationMs: doublePrecision("duration_ms").notNull(),
    bytesIn: bigint("bytes_in", { mode: "number" }).notNull(),
    bytesOut: bigint("bytes_out", { mode: "number" }).notNull(),
    visitorId: bigint("visitor_id", { mode: "bigint" }),
    kind: text("kind", { enum: REQUEST_KINDS }).notNull(),
    botReason: text("bot_reason"),
    source: text("source").notNull(),
    referrerHost: text("referrer_host"),
    language: text("language"),
    utmSource: text("utm_source"),
    utmMedium: text("utm_medium"),
    utmCampaign: text("utm_campaign"),
    utmTerm: text("utm_term"),
    utmContent: text("utm_content"),
  },
  (table) => [
    primaryKey({ columns: [table.environmentId, table.ts, table.requestId] }),
    check("request_kind_check", sql`${table.kind} in ('pageview', 'bot', 'other')`),
  ],
);

export const analyticsGoal = analytics.table(
  "goal",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    environmentId: text("environment_id").notNull(),
    name: text("name").notNull(),
    pathPattern: text("path_pattern").notNull(),
    createdAt: timestamptz("created_at").defaultNow().notNull(),
  },
  (table) => [uniqueIndex("goal_environment_path_idx").on(table.environmentId, table.pathPattern)],
);
