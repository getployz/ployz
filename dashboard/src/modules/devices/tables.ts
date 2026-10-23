import { sql } from "drizzle-orm";
import { check, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

/**
 * A pending Device request: the public key a `ployz` device asked to be admitted with, under the code its holder reads
 * out. Cloud only relays it; a Machine admits the Device. It reads as gone once expired.
 */
export const deviceRequest = pgTable("device_request", {
  organizationId: uuid("organization_id").notNull()
    .references(() => organization.id, { onDelete: "cascade" }),
  code: text("code").notNull(),
  key: text("key").notNull(),
  expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }).notNull(),
  createdAt,
}, (table) => [
  primaryKey({ columns: [table.organizationId, table.code] }),
  check("device_request_code_shape", sql`${table.code} ~ '^[a-z2-7]{10}$'`),
  check("device_request_key_shape", sql`${table.key} ~ '^[0-9a-f]{64}$'`),
]);
