import { sql } from "drizzle-orm";
import { index, integer, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt } from "#/db/tables";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";
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

/** A host waiting to be enrolled, keyed by its Check-in Key. Sole writer: POST /api/checkin. Facts are the host's own. */
export const waitingServer = pgTable(
  "waiting_server",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    fleetTokenId: uuid("fleet_token_id")
      .notNull()
      .references(() => fleetToken.id, { onDelete: "cascade" }),
    checkinKey: text("checkin_key").notNull(),
    report: jsonb("report").notNull().$type<StoredReport>(),
    /** The address Cloud saw the check-in come from, not one the host claims. */
    observedIp: text("observed_ip").notNull(),
    probe: jsonb("probe").$type<unknown>(),
    firstSeenAt: timestamp("first_seen_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
    lastCheckInAt: timestamp("last_check_in_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
    /** Tombstone: the same key under the same token is told to stop. */
    rejectedAt: timestamp("rejected_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    uniqueIndex("waiting_server_key_idx").on(table.organizationId, table.checkinKey),
    index("waiting_server_fleet_token_idx").on(table.fleetTokenId),
  ],
);

/** Fixed one-second windows of check-in counts, one row per token or source address. */
export const checkinRateLimit = pgTable("checkin_rate_limit", {
  key: text("key").primaryKey(),
  windowStart: timestamp("window_start", { mode: "date", withTimezone: true }).notNull(),
  count: integer("count").notNull(),
});
