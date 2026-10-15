import { boolean, bytea, date, pgSchema, primaryKey, text, timestamp } from "drizzle-orm/pg-core";

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
