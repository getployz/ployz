import { sql } from "drizzle-orm";
import { bigint, boolean, bytea, date, pgSchema, primaryKey, smallint, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";

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

// Partitioned by month on ts in the migration; drizzle-kit cannot declare partitions.
export const analyticsPageview = analytics.table(
  "pageview",
  {
    environmentId: text("environment_id").notNull(),
    ts: timestamptz("ts").notNull(),
    visitorId: bigint("visitor_id", { mode: "bigint" }).notNull(),
    host: text("host").notNull(),
    pathname: text("pathname").notNull(),
    source: text("source").notNull(),
    referrerHost: text("referrer_host"),
    utmSource: text("utm_source"),
    utmMedium: text("utm_medium"),
    utmCampaign: text("utm_campaign"),
    utmTerm: text("utm_term"),
    utmContent: text("utm_content"),
    ref: text("ref"),
    language: text("language"),
    flags: smallint("flags").default(0).notNull(),
  },
  (table) => [
    primaryKey({
      columns: [table.environmentId, table.ts, table.visitorId, table.host, table.pathname],
    }),
  ],
);

export const analyticsDaily = analytics.table(
  "daily",
  {
    environmentId: text("environment_id").notNull(),
    day: date("day", { mode: "string" }).notNull(),
    dim: text("dim").notNull(),
    value: text("value").notNull(),
    pageviews: bigint("pageviews", { mode: "number" }).notNull(),
    visitors: bigint("visitors", { mode: "number" }).notNull(),
    visits: bigint("visits", { mode: "number" }).notNull(),
    bounces: bigint("bounces", { mode: "number" }).notNull(),
    totalDuration: bigint("total_duration", { mode: "number" }).notNull(),
  },
  (table) => [primaryKey({ columns: [table.environmentId, table.day, table.dim, table.value] })],
);

export const analyticsSessionHourly = analytics.table(
  "session_hourly",
  {
    environmentId: text("environment_id").notNull(),
    hour: timestamptz("hour").notNull(),
    visits: bigint("visits", { mode: "number" }).notNull(),
    bounces: bigint("bounces", { mode: "number" }).notNull(),
    totalDuration: bigint("total_duration", { mode: "number" }).notNull(),
  },
  (table) => [primaryKey({ columns: [table.environmentId, table.hour] })],
);

export const analyticsBotDaily = analytics.table(
  "bot_daily",
  {
    environmentId: text("environment_id").notNull(),
    day: date("day", { mode: "string" }).notNull(),
    reason: text("reason").notNull(),
    count: bigint("count", { mode: "number" }).notNull(),
  },
  (table) => [primaryKey({ columns: [table.environmentId, table.day, table.reason] })],
);

export const analyticsGapDaily = analytics.table(
  "gap_daily",
  {
    environmentId: text("environment_id").notNull(),
    day: date("day", { mode: "string" }).notNull(),
    count: bigint("count", { mode: "number" }).notNull(),
  },
  (table) => [primaryKey({ columns: [table.environmentId, table.day] })],
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

export const analyticsRollupState = analytics.table("rollup_state", {
  environmentId: text("environment_id").primaryKey(),
  rolledUpTo: date("rolled_up_to", { mode: "string" }),
  dirty: date("dirty", { mode: "string" }).array().default(sql`'{}'::date[]`).notNull(),
  updatedAt: timestamptz("updated_at").defaultNow().notNull(),
});
