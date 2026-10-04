import type { DrainReport, EnrollmentAssignment } from "@ployz/sdk";
import { createdAt, type EncryptedSecretValue, type MachineId, sqlStringLiterals, updatedAt } from "#/db/tables";

import { user } from "#/modules/identity/tables";

import { MACHINE_REMOVE_ATTEMPT_STATES, type MachineRemoveAttemptState, type MachineRemoveResult } from "#/modules/machines/machine-removal";

import { DRAIN_FAILURE_CODES, DRAIN_STATES, type DrainFailureCode, type DrainState } from "#/modules/machines/server-drain";

import { organization } from "#/modules/organization/tables";

import { type DataLossIdentity } from "#/modules/runtime/data-loss-identity";

import { sql } from "drizzle-orm";

import { boolean, check, foreignKey, index, jsonb, pgTable, primaryKey, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";



export const machineRemoveAttempt = pgTable(
  "machine_remove_attempt",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    requestedByUserId: uuid("requested_by_user_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    /** Take the Server out of the Cluster without resetting it (`ployz server rm --no-reset`). */
    noReset: boolean("no_reset").default(false).notNull(),
    confirmDataLoss: jsonb("confirm_data_loss")
      .notNull()
      .$type<DataLossIdentity[]>(),
    state: text("state")
      .default("pending")
      .notNull()
      .$type<MachineRemoveAttemptState>(),
    inngestRunId: text("inngest_run_id"),
    missingIdentities: jsonb("missing_identities").$type<
      DataLossIdentity[] | null
    >(),
    failureCode: text("failure_code"),
    failureMessage: text("failure_message"),
    /** A succeeded removal's result: the reset warning, and what became of Cloud's hold. */
    result: jsonb("result").$type<MachineRemoveResult | null>(),
    startedAt: timestamp("started_at", {
      mode: "date",
      withTimezone: true,
    }),
    terminalAt: timestamp("terminal_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    uniqueIndex("machine_remove_attempt_inngest_run_uidx")
      .on(table.inngestRunId)
      .where(sql`${table.inngestRunId} is not null`),
    uniqueIndex("machine_remove_attempt_one_active_org_machine_idx")
      .on(table.organizationId, table.machineId)
      .where(sql`${table.state} in ('pending', 'running')`),
    check(
      "machine_remove_attempt_machine_id_check",
      sql`length(${table.machineId}) between 1 and 64 and ${table.machineId} !~ '[[:cntrl:]]'`,
    ),
    check(
      "machine_remove_attempt_result_check",
      sql`${table.result} is null or ${table.state} = 'succeeded'`,
    ),
    check(
      "machine_remove_attempt_confirm_data_loss_check",
      sql`jsonb_typeof(${table.confirmDataLoss}) = 'array'`,
    ),
    check(
      "machine_remove_attempt_state_check",
      sql`${table.state} in (${sqlStringLiterals(MACHINE_REMOVE_ATTEMPT_STATES)})`,
    ),
    check(
      "machine_remove_attempt_state_shape_check",
      sql`(
        (${table.state} = 'pending' and ${table.inngestRunId} is null
          and ${table.startedAt} is null and ${table.terminalAt} is null
          and ${table.failureCode} is null and ${table.failureMessage} is null
          and ${table.missingIdentities} is null)
        or (${table.state} = 'running' and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.startedAt} is not null and ${table.terminalAt} is null
          and ${table.failureCode} is null and ${table.failureMessage} is null
          and ${table.missingIdentities} is null)
        or (${table.state} = 'succeeded' and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.failureCode} is null and ${table.failureMessage} is null
          and ${table.missingIdentities} is null)
        or (${table.state} in ('failed','cancelled')
          and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.failureCode} is not null and ${table.failureMessage} is not null
          and ${table.failureCode} ~ '^[a-z][a-z0-9_]{0,63}$'
          and length(${table.failureMessage}) between 1 and 1024
          and ${table.missingIdentities} is null)
        or (${table.state} = 'missing_identities'
          and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.failureCode} is null and ${table.failureMessage} is null
          and jsonb_typeof(${table.missingIdentities}) = 'array'
          and jsonb_array_length(${table.missingIdentities}) > 0)
      )`,
    ),
  ],
);

/**
 * One Drain Cloud ran, or was asked to run, on one Server: history, read server-side as the latest per Server, never
 * into the Org Store. The click writes the row (`pending`) under the id the tab minted, so the one-active guard answers
 * the click and every tab sees it through the change stream; the run binds itself to it, claims it (`running`) right
 * before it asks the Engine, and ends it with the Engine's report (`finished`) or without one (`failed`, `cancelled`,
 * `unknown`).
 */
export const serverDrainAttempt = pgTable(
  "server_drain_attempt",
  {
    /** The request id the confirming tab minted. */
    id: uuid("id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    /** Who clicked Drain; kept when the user is deleted. */
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    state: text("state").default("pending").notNull().$type<DrainState>(),
    /** The run that owns the row, bound while pending; only it may claim or end the row. */
    inngestRunId: text("inngest_run_id"),
    /** The Engine's DrainReport, verbatim, partial or not. Only `finished` has one. */
    report: jsonb("report").$type<DrainReport | null>(),
    failureCode: text("failure_code").$type<DrainFailureCode | null>(),
    failureMessage: text("failure_message"),
    requestedAt: timestamp("requested_at", { mode: "date", withTimezone: true }).defaultNow().notNull(),
    startedAt: timestamp("started_at", { mode: "date", withTimezone: true }),
    endedAt: timestamp("ended_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    // One Drain per Server at a time: a second click returns the active one.
    uniqueIndex("server_drain_attempt_one_active_idx")
      .on(table.organizationId, table.machineId)
      .where(sql`${table.state} in ('pending', 'running')`),
    uniqueIndex("server_drain_attempt_run_uidx")
      .on(table.inngestRunId)
      .where(sql`${table.inngestRunId} is not null`),
    index("server_drain_attempt_latest_idx").on(table.organizationId, table.machineId, table.requestedAt),
    check("server_drain_attempt_machine_id_check", sql`${table.machineId} ~ '^[0-9a-f]{32}$'`),
    check("server_drain_attempt_state_check", sql`${table.state} in (${sqlStringLiterals(DRAIN_STATES)})`),
    check(
      "server_drain_attempt_failure_code_check",
      sql`${table.failureCode} is null or ${table.failureCode} in (${sqlStringLiterals(DRAIN_FAILURE_CODES)})`,
    ),
    check("server_drain_attempt_run_check", sql`${table.inngestRunId} is null or length(${table.inngestRunId}) between 1 and 255`),
    // Each state carries exactly its evidence. A bound pending row names its run; a dispatch failure never had one.
    check(
      "server_drain_attempt_state_shape_check",
      sql`(
        (${table.state} = 'pending'
          and ${table.startedAt} is null and ${table.endedAt} is null
          and ${table.report} is null and ${table.failureCode} is null and ${table.failureMessage} is null)
        or (${table.state} = 'running' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is null
          and ${table.report} is null and ${table.failureCode} is null and ${table.failureMessage} is null)
        or (${table.state} = 'finished' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null
          and jsonb_typeof(${table.report}) = 'object' and ${table.failureCode} is null and ${table.failureMessage} is null)
        or (${table.state} in ('failed', 'cancelled')
          and ${table.endedAt} is not null and ${table.report} is null
          and ${table.failureCode} is not null and ${table.failureMessage} is not null
          and length(${table.failureMessage}) between 1 and 1024)
        or (${table.state} = 'unknown' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null and ${table.report} is null
          and ${table.failureCode} is not null and ${table.failureMessage} is not null
          and length(${table.failureMessage}) between 1 and 1024)
      )`,
    ),
  ],
);

/** Backend-only connection metadata; neither membership nor live presence. */
export const organizationMachine = pgTable(
  "organization_machine",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    clusterKey: text("cluster_key").notNull(),
    encryptedCapability: jsonb("encrypted_capability").notNull().$type<EncryptedSecretValue>(),
    isDialEntry: boolean("is_dial_entry").default(false).notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    primaryKey({
      columns: [table.organizationId, table.machineId],
    }),
    index("organization_machine_machine_idx").on(table.machineId),
    uniqueIndex("organization_machine_one_dial_entry_idx")
      .on(table.organizationId)
      .where(sql`${table.isDialEntry}`),
    check("organization_machine_cluster_key_check", sql`${table.clusterKey} ~ '^[0-9a-f]{64}$'`),
    check(
      "organization_machine_id_format_check",
      sql`${table.machineId} ~ '^[0-9a-f]{32}$'`,
    ),
  ],
);

/**
 * Server Access: one Server's `cli-<id>` Management Client held for one signed-in device or Organization Token.
 * A revoked row keeps no capability and stays until its Server confirms the Clear.
 */
export const serverAccess = pgTable(
  "server_access",
  {
    organizationId: uuid("organization_id").notNull(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    // No foreign keys to the credential or its user: a pending revocation outlives both.
    credentialId: uuid("credential_id").notNull(),
    credentialKind: text("credential_kind").notNull().$type<"session" | "token">(),
    userId: uuid("user_id").notNull(),
    encryptedCapability: jsonb("encrypted_capability").$type<EncryptedSecretValue>(),
    revokedAt: timestamp("revoked_at", { mode: "date", withTimezone: true }),
    createdAt,
    updatedAt,
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.credentialId, table.machineId] }),
    // Pairing removal clears every `cli-` slot on its Servers, so these rows go with the Server.
    foreignKey({
      name: "server_access_organization_machine_fkey",
      columns: [table.organizationId, table.machineId],
      foreignColumns: [organizationMachine.organizationId, organizationMachine.machineId],
    }).onDelete("cascade"),
    index("server_access_credential_idx").on(table.credentialId),
    check("server_access_credential_kind_check", sql`${table.credentialKind} in ('session', 'token')`),
    check("server_access_revoked_shape_check",
      sql`(${table.revokedAt} is null) = (${table.encryptedCapability} is not null)`),
  ],
);

export const machineEnrollmentToken = pgTable(
  "machine_enrollment_token",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    tokenHash: text("token_hash").notNull(),
    createdByUserId: uuid("created_by_user_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    expiresAt: timestamp("expires_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    /** The first Server that completed enrollment with this token. */
    joinedMachineId: text("joined_machine_id").$type<MachineId>(),
    createdAt,
    updatedAt,
  },
  (table) => [
    uniqueIndex("machine_enrollment_token_hash_idx").on(table.tokenHash),
    index("machine_enrollment_token_organization_idx").on(
      table.organizationId,
    ),
    check(
      "machine_enrollment_token_hash_check",
      sql`${table.tokenHash} ~ '^[0-9a-f]{64}$'`,
    ),
  ],
);

/** Cloud's allocation history, not runtime Cluster membership. */
export const enrollmentAllocation = pgTable(
  "enrollment_allocation",
  {
    organizationId: uuid("organization_id").notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    // The pairing credential identifies the current Cloud Cluster generation.
    clusterKey: text("cluster_key").notNull(),
    assignments: jsonb("assignments").notNull().$type<EnrollmentAssignment[]>(),
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.clusterKey] }),
    check("enrollment_allocation_cluster_key_check", sql`${table.clusterKey} ~ '^[0-9a-f]{64}$'`),
    check("enrollment_allocation_assignments_check", sql`jsonb_typeof(${table.assignments}) = 'array'`),
  ],
);
