import { sql } from "drizzle-orm";
import { bytea, check, index, integer, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { organization } from "#/modules/organization/tables";

/**
 * One chunk of the source uploaded for one Deployment: a gzipped tar, in order. It is that Deployment's alone,
 * written once before it is admitted, and deleted once it ends.
 */
export const uploadChunk = pgTable(
  "upload_chunk",
  {
    deploymentId: text("deployment_id").notNull(),
    index: integer("index").notNull(),
    organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
    data: bytea("data").notNull(),
    createdAt: timestamp("created_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [
    primaryKey({ columns: [table.deploymentId, table.index] }),
    index("upload_chunk_created_at_idx").on(table.createdAt),
    check("upload_chunk_index_check", sql`${table.index} >= 0`),
  ],
);
