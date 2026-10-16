import { sql } from "drizzle-orm";
import { integer, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";

/** A Fleet Token: one secret many hosts carry to ask to join an Organization. Cloud keeps only the secret's hash. */
export const fleetToken = pgTable(
  "fleet_token",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    name: text("name").notNull(),
    tokenHash: text("token_hash").notNull(),
    expiresAt: timestamp("expires_at", { mode: "date", withTimezone: true }).notNull(),
    /** How many hosts this token may admit without a person's approval; null means every host waits. */
    autoEnrollMax: integer("auto_enroll_max"),
    createdByUserId: uuid("created_by_user_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    lastUsedAt: timestamp("last_used_at", { mode: "date", withTimezone: true }),
    revokedAt: timestamp("revoked_at", { mode: "date", withTimezone: true }),
    createdAt,
  },
  (table) => [
    uniqueIndex("fleet_token_hash_idx").on(table.tokenHash),
    uniqueIndex("fleet_token_name_idx").on(table.organizationId, table.name).where(sql`revoked_at is null`),
  ],
);
