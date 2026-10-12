import { boolean, pgTable, uuid } from "drizzle-orm/pg-core";
import { updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";

/** Whether the Organization lets Ployz support open it. No row reads as off. */
export const organizationSupportAccess = pgTable("organization_support_access", {
  organizationId: uuid("organization_id").primaryKey()
    .references(() => organization.id, { onDelete: "cascade" }),
  allowed: boolean("allowed").default(false).notNull(),
  changedByUserId: uuid("changed_by_user_id").references(() => user.id, { onDelete: "set null" }),
  updatedAt,
});
