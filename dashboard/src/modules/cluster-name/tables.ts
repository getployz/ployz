import { sql } from "drizzle-orm";
import { check, pgTable, text, uuid } from "drizzle-orm/pg-core";
import { createdAt, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

/**
 * The Organization's Cluster Name as Cloud last set it. The Engine offers Cloud no read of it, so `appliedName` is
 * the name a Server last confirmed. No row means nobody named the Cluster: a founding writes one with the default.
 */
export const organizationClusterName = pgTable("organization_cluster_name", {
  organizationId: uuid("organization_id").primaryKey()
    .references(() => organization.id, { onDelete: "cascade" }),
  name: text("name").notNull(),
  appliedName: text("applied_name"),
  refusal: text("refusal"),
  createdAt,
  updatedAt,
}, (table) => [
  check("organization_cluster_name_name_check", sql`${table.name} ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'`),
]);
