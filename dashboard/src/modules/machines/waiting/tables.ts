import { sql } from "drizzle-orm";
import { index, integer, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt, type EncryptedSecretValue, type MachineId } from "#/db/tables";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";
import type {
  EnrollInstruction,
  EnrollmentFailure,
  EnrollmentRole,
  EnrollmentStage,
  EnrollmentState,
} from "#/modules/machines/waiting/server-enrollment";
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
    observedIp: text("observed_ip").notNull(),
    probe: jsonb("probe").$type<unknown>(),
    firstSeenAt: timestamp("first_seen_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
    lastCheckInAt: timestamp("last_check_in_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
    rejectedAt: timestamp("rejected_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    uniqueIndex("waiting_server_key_idx").on(table.organizationId, table.checkinKey),
    index("waiting_server_fleet_token_idx").on(table.fleetTokenId),
  ],
);

/** One try at enrolling one waiting host. Sole writer after insert: the server-enrollment run. */
export const serverEnrollment = pgTable(
  "server_enrollment",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    waitingServerId: uuid("waiting_server_id")
      .notNull()
      .references(() => waitingServer.id, { onDelete: "cascade" }),
    fleetTokenId: uuid("fleet_token_id").notNull(),
    role: text("role").notNull().$type<EnrollmentRole>(),
    instruction: jsonb("instruction").notNull().$type<EnrollInstruction>(),
    enrollTokenId: uuid("enroll_token_id").notNull(),
    /** Dropped when the attempt ends. */
    encryptedToken: jsonb("encrypted_token").$type<EncryptedSecretValue>(),
    state: text("state").notNull().$type<EnrollmentState>(),
    stage: text("stage").$type<EnrollmentStage>(),
    machineId: text("machine_id").$type<MachineId>(),
    failure: jsonb("failure").$type<EnrollmentFailure>(),
    inngestRunId: text("inngest_run_id"),
    /** Null when the Fleet Token enrolled the host on arrival. */
    requestedByUserId: uuid("requested_by_user_id"),
    requestedAt: timestamp("requested_at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
    endedAt: timestamp("ended_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    uniqueIndex("server_enrollment_one_open_idx").on(table.waitingServerId).where(sql`ended_at is null`),
    uniqueIndex("server_enrollment_one_founder_idx").on(table.organizationId).where(sql`ended_at is null and role = 'founder'`),
    index("server_enrollment_organization_idx").on(table.organizationId),
  ],
);

export const checkinRateLimit = pgTable("checkin_rate_limit", {
  key: text("key").primaryKey(),
  windowStart: timestamp("window_start", { mode: "date", withTimezone: true }).notNull(),
  count: integer("count").notNull(),
});
