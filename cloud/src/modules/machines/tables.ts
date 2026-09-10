import type { EnrollmentAssignment } from "@ployz/sdk";
import { createdAt, type EncryptedSecretValue, type MachineId, sqlStringLiterals, updatedAt } from "#/db/tables";

import { user } from "#/modules/identity/tables";

import { MACHINE_REMOVE_ATTEMPT_STATES, type MachineRemoveAttemptState } from "#/modules/machines/machine-removal";

import { organization } from "#/modules/organization/tables";

import { type DataLossIdentity } from "#/modules/runtime/data-loss-identity";

import { sql } from "drizzle-orm";

import { boolean, check, index, jsonb, pgTable, primaryKey, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";



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

/** Backend-only connection metadata; neither membership nor live presence. */
export const organizationMachine = pgTable(
  "organization_machine",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    clusterKey: text("cluster_key").notNull(),
    encryptedTailcat: jsonb("encrypted_tailcat").notNull().$type<EncryptedSecretValue>(),
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
