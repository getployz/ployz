import { boolean, bytea, date, pgTable, primaryKey, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

export const analyticsSite = pgTable(
  "analytics_site",
  {
    organizationId: text("organization_id").notNull(),
    environmentId: text("environment_id").notNull(),
    enabled: boolean("enabled").default(false).notNull(),
    timezone: text("timezone").default("UTC").notNull(),
    updatedAt: timestamptz("updated_at").defaultNow().notNull(),
  },
  (table) => [primaryKey({ columns: [table.organizationId, table.environmentId] })],
);

export const analyticsSalt = pgTable(
  "analytics_salt",
  {
    organizationId: text("organization_id").notNull(),
    environmentId: text("environment_id").notNull(),
    day: date("day", { mode: "string" }).notNull(),
    salt: bytea("salt").notNull(),
    createdAt: timestamptz("created_at").defaultNow().notNull(),
  },
  (table) => [primaryKey({ columns: [table.organizationId, table.environmentId, table.day] })],
);

export const analyticsGoal = pgTable(
  "analytics_goal",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    environmentId: text("environment_id").notNull(),
    name: text("name").notNull(),
    pathPattern: text("path_pattern").notNull(),
    createdAt: timestamptz("created_at").defaultNow().notNull(),
  },
  (table) => [uniqueIndex("analytics_goal_environment_path_idx").on(table.environmentId, table.pathPattern)],
);
