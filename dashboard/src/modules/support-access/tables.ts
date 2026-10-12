import { sql } from "drizzle-orm";
import { boolean, check, pgTable, uuid } from "drizzle-orm/pg-core";
import { updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";

/**
 * Whether the Organization lets Ployz support open it, and what support may read once inside. No row reads as off.
 * Support reads nothing extra while it can't get in.
 */
export const organizationSupportAccess = pgTable("organization_support_access", {
  organizationId: uuid("organization_id").primaryKey()
    .references(() => organization.id, { onDelete: "cascade" }),
  allowed: boolean("allowed").default(false).notNull(),
  variables: boolean("variables").default(false).notNull(),
  configs: boolean("configs").default(false).notNull(),
  logs: boolean("logs").default(false).notNull(),
  changedByUserId: uuid("changed_by_user_id").references(() => user.id, { onDelete: "set null" }),
  updatedAt,
}, (table) => [
  check(
    "organization_support_access_shows_only_when_allowed",
    sql`${table.allowed} or not (${table.variables} or ${table.configs} or ${table.logs})`,
  ),
]);
