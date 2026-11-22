import { sql } from "drizzle-orm";
import { bigint, boolean, customType, index, pgTable, text, uuid } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";

const xid8 = customType<{ data: string }>({ dataType: () => "xid8" });

/**
 * Organization change log, written only by the `organization_change_log` statement trigger.
 * Readers see a row once its transaction and every older one has finished:
 * `xid < pg_snapshot_xmin(pg_current_snapshot())`. That xid horizon is the read cursor.
 * No organization FK: cascading organization deletes still log their rows.
 */
export const organizationChangeLog = pgTable(
  "organization_change",
  {
    seq: bigint("seq", { mode: "number" }).primaryKey().generatedAlwaysAsIdentity(),
    xid: xid8("xid").notNull().default(sql`pg_current_xact_id()`),
    organizationId: uuid("organization_id").notNull(),
    sourceTable: text("source_table").notNull(),
    changedIds: text("changed_ids").array().notNull(),
    deletedIds: text("deleted_ids").array().notNull(),
    // More than 100 keys in one statement: the ids are dropped and readers do a full read.
    allRows: boolean("all_rows").notNull(),
    createdAt,
  },
  (table) => [
    index("organization_change_organization_id_xid_idx").on(table.organizationId, table.xid),
    // The oldest xid is the retention fence.
    index("organization_change_xid_idx").on(table.xid),
  ],
);
