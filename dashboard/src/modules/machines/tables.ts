import type { DrainReport, EnrollmentAssignment, MachineId, MachineName, QualifiedService } from "@ployz/sdk";
import { approvalIndex, attemptColumns, attemptConstraints, endCodeCheck, machineIdCheck } from "#/modules/inngest/attempt-table";
import { createdAt, type EncryptedSecretValue, sqlStringLiterals, updatedAt } from "#/db/tables";

import { MACHINE_ID_SQL } from "#/lib/schema";
import { user } from "#/modules/identity/tables";

import { MACHINE_REMOVE_ATTEMPT_STATES, type MachineRemoveAttemptState, type MachineRemoveResult } from "#/modules/machines/machine-removal-states";

import { CLEANUP_END_CODES, CLEANUP_STATES, type CleanupEndCode, type CleanupState } from "#/modules/machines/namespace-cleanup-states";
import { DRAIN_END_CODES, DRAIN_STATES, type DrainEndCode, type DrainState } from "#/modules/machines/server-drain-states";
import { WEB_TRAFFIC_STATES, WEB_TRAFFIC_WANTS, type WebTrafficState, type WebTrafficWant } from "#/modules/machines/server-web-traffic-states";

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
    /** The name the run's caller showed for the Server, kept after a rename or removal; null when it showed none, and on older rows. */
    serverName: text("server_name").$type<MachineName>(),
    /** Take the Server out of the Cluster without resetting it (`ployz server rm --no-reset`). */
    noReset: boolean("no_reset").default(false).notNull(),
    approvalId: uuid("approval_id"),
    confirmDataLoss: jsonb("confirm_data_loss")
      .notNull()
      .$type<DataLossIdentity[]>(),
    /** Unused: the signed teardown a departure once carried. */
    teardown: jsonb("teardown"),
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
    /** When a run first asked the Server to leave. Every retry asks again and keeps this time. */
    removeSentAt: timestamp("remove_sent_at", {
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
    uniqueIndex("machine_remove_attempt_approval_uidx")
      .on(table.approvalId)
      .where(sql`${table.approvalId} is not null`),
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
 * into the Org Store. The click writes the row under the id the tab minted, so the one-active guard answers the click
 * and every tab sees it through the change stream. Its states are `DRAIN_STATES`.
 */
export const serverDrainAttempt = pgTable(
  "server_drain_attempt",
  {
    ...attemptColumns<DrainState>(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    /** The name the run's caller showed for the Server, kept after a rename or removal; null when it showed none, and on older rows. */
    serverName: text("server_name").$type<MachineName>(),
    /**
     * The Engine's DrainReport, partial or not, with Cloud's notes on the Volumes it moves. `finished` has one, and so
     * does a `running` Drain whose Engine call returned while its Volumes move.
     */
    report: jsonb("report").$type<DrainReport | null>(),
    /** How it ended without a report; null until then, and for `finished`. */
    endCode: text("end_code").$type<DrainEndCode | null>(),
    /** The Engine's words when it refused: set exactly when `end_code` is `refused`. */
    refusalMessage: text("refusal_message"),
    /** The services the CLI previewed; it drains exactly these. Null drains every owned Namespace (the dashboard). */
    targets: jsonb("targets").$type<QualifiedService[] | null>(),
    /** The approval this drain consumed: a retry with it answers this row instead of draining again. */
    approvalId: uuid("approval_id"),
    /** The digest of the plan its gate admitted: it runs only while it would destroy exactly that, or nothing. */
    planDigest: text("plan_digest"),
    /** A person said yes to the plan's digest: it runs only while its plan is exactly that one, even one that now destroys nothing. */
    approved: boolean("approved").default(false).notNull(),
    /** A try found the servers silent, so the pending row waits for them rather than for another Drain. */
    serversSilent: boolean("servers_silent").default(false).notNull(),
  },
  (table) => [
    // One Drain per Server at a time: a second click returns the active one.
    ...attemptConstraints("server_drain_attempt", table, table.machineId, DRAIN_STATES),
    index("server_drain_attempt_latest_idx").on(table.organizationId, table.machineId, table.requestedAt),
    approvalIndex("server_drain_attempt", table.approvalId),
    check("server_drain_attempt_targets_check", sql`${table.targets} is null or jsonb_typeof(${table.targets}) = 'array'`),
    machineIdCheck("server_drain_attempt", table.machineId),
    endCodeCheck("server_drain_attempt", table, DRAIN_END_CODES),
    check(
      "server_drain_attempt_refusal_check",
      sql`(${table.endCode} is not distinct from 'refused') = (${table.refusalMessage} is not null)
        and (${table.refusalMessage} is null or length(${table.refusalMessage}) between 1 and 1024)`,
    ),
    // Each state carries exactly its evidence. A bound pending row names its run; one never queued never had one.
    check(
      "server_drain_attempt_state_shape_check",
      sql`(
        (${table.state} = 'pending'
          and ${table.startedAt} is null and ${table.endedAt} is null and ${table.report} is null and ${table.endCode} is null)
        or (${table.state} = 'running' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is null
          and (${table.report} is null or jsonb_typeof(${table.report}) = 'object') and ${table.endCode} is null)
        or (${table.state} = 'finished' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null
          and jsonb_typeof(${table.report}) = 'object' and ${table.endCode} is null)
        or (${table.state} in ('failed', 'cancelled')
          and ${table.endedAt} is not null and ${table.report} is null and ${table.endCode} is not null)
        or (${table.state} = 'unknown' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null and ${table.report} is null
          and ${table.endCode} is not null)
      )`,
    ),
  ],
);

/**
 * One web traffic change Cloud applied, or was asked to apply, to one Server: history, read server-side as the latest
 * per Server, never into the Org Store. The flip writes the row under the id the tab minted, so the one-active guard
 * answers the flip and every tab sees it through the change stream. Its states are `WEB_TRAFFIC_STATES`.
 */
export const serverWebTrafficAttempt = pgTable(
  "server_web_traffic_attempt",
  {
    ...attemptColumns<WebTrafficState>(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    want: text("want").notNull().$type<WebTrafficWant>(),
    /** Why it failed, in the user's words: set exactly when `state` is `failed`. */
    failure: text("failure"),
  },
  (table) => [
    // One change per Server at a time: a second flip returns the active one.
    ...attemptConstraints("server_web_traffic_attempt", table, table.machineId, WEB_TRAFFIC_STATES),
    index("server_web_traffic_attempt_latest_idx").on(table.organizationId, table.machineId, table.requestedAt),
    machineIdCheck("server_web_traffic_attempt", table.machineId),
    check("server_web_traffic_attempt_want_check", sql`${table.want} in (${sqlStringLiterals(WEB_TRAFFIC_WANTS)})`),
    check(
      "server_web_traffic_attempt_failure_check",
      sql`(${table.state} = 'failed') = (${table.failure} is not null)
        and (${table.failure} is null or length(${table.failure}) between 1 and 1024)`,
    ),
    // Each state carries exactly its evidence. A bound pending row names its run; one never queued never had one.
    check(
      "server_web_traffic_attempt_state_shape_check",
      sql`(
        (${table.state} = 'pending' and ${table.startedAt} is null and ${table.endedAt} is null)
        or (${table.state} = 'running' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is null)
        or (${table.state} = 'finished' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null)
        or (${table.state} = 'failed' and ${table.endedAt} is not null)
      )`,
    ),
  ],
);

/**
 * One clean of a Namespace no Environment owns, asked for from the CLI, deleting exactly the Volumes the caller
 * confirmed. Its states are `CLEANUP_STATES`.
 */
export const namespaceCleanup = pgTable(
  "namespace_cleanup",
  {
    ...attemptColumns<CleanupState>(),
    namespace: text("namespace").notNull(),
    /** The approval this clean consumed: a retry with it answers this row instead of cleaning again. */
    approvalId: uuid("approval_id"),
    /** The Volumes the caller confirmed losing. */
    confirmDataLoss: jsonb("confirm_data_loss").notNull().$type<DataLossIdentity[]>(),
    /** Unused: the signed teardown a clean once carried. */
    teardown: jsonb("teardown"),
    /** The Docker Volume names removed; only `finished` has them. */
    volumes: jsonb("volumes").$type<string[] | null>(),
    endCode: text("end_code").$type<CleanupEndCode | null>(),
    /** The Engine's words: set exactly when `end_code` is `refused` or `incomplete`. */
    endMessage: text("end_message"),
  },
  (table) => [
    ...attemptConstraints("namespace_cleanup", table, table.namespace, CLEANUP_STATES),
    approvalIndex("namespace_cleanup", table.approvalId),
    check("namespace_cleanup_namespace_check", sql`length(${table.namespace}) between 1 and 255 and ${table.namespace} !~ '[[:cntrl:]]'`),
    check("namespace_cleanup_confirm_data_loss_check", sql`jsonb_typeof(${table.confirmDataLoss}) = 'array'`),
    endCodeCheck("namespace_cleanup", table, CLEANUP_END_CODES),
    check(
      "namespace_cleanup_end_message_check",
      sql`(${table.endCode} is not distinct from 'refused' or ${table.endCode} is not distinct from 'incomplete') = (${table.endMessage} is not null)
        and (${table.endMessage} is null or length(${table.endMessage}) between 1 and 1024)`,
    ),
    check(
      "namespace_cleanup_state_shape_check",
      sql`(
        (${table.state} = 'pending'
          and ${table.startedAt} is null and ${table.endedAt} is null and ${table.volumes} is null and ${table.endCode} is null)
        or (${table.state} = 'running' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is null and ${table.volumes} is null and ${table.endCode} is null)
        or (${table.state} = 'finished' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null
          and jsonb_typeof(${table.volumes}) = 'array' and ${table.endCode} is null)
        or (${table.state} in ('failed', 'cancelled')
          and ${table.endedAt} is not null and ${table.volumes} is null and ${table.endCode} is not null)
        or (${table.state} = 'unknown' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is not null and ${table.volumes} is null
          and ${table.endCode} is not null)
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
    /** The Machine's management public key, hex; the first half of its capability. Verifies its signed pushes. */
    managementIdentity: text("management_identity"),
    /** The push URL this Machine last confirmed it holds; null until Cloud has set it since the capability was published. */
    pushTarget: text("push_target"),
    /** The Server's name as it enrolled or the Cluster last reported it; null until Cloud has seen either. */
    machineName: text("machine_name").$type<MachineName>(),
    isDialEntry: boolean("is_dial_entry").default(false).notNull(),
    /** When Cloud last accepted an OTLP batch from this Machine, kept to within 30 seconds; null until it first reported. */
    observeSeenAt: timestamp("observe_seen_at", { mode: "date", withTimezone: true }),
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
      sql`${table.machineId} ~ '${sql.raw(MACHINE_ID_SQL)}'`,
    ),
    check("organization_machine_management_identity_check", sql`${table.managementIdentity} ~ '^[0-9a-f]{64}$'`),
  ],
);

/**
 * Server Access: one Server's `cli-<id>` Management Client held for one signed-in device or Organization Token.
 * A row without a capability is a claim while its Set is in flight, so one credential Sets one Server once at a
 * time. A revoked row keeps no capability and stays until its Server confirms the Clear. `claim` names the request
 * whose Set or Clear is in flight on the slot, so no two act on it at once and a late one touches nothing; a row
 * with no claim is open to the next one.
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
    claim: text("claim"),
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
      sql`${table.revokedAt} is null or ${table.encryptedCapability} is null`),
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
    /** The one Machine identity that may use this token; set before the Pairing secret is handed out. */
    claimedMachineId: text("claimed_machine_id").$type<MachineId>(),
    /** The claimant's WireGuard public key, in the enrollment identity's base64 form. */
    claimedPublicKey: text("claimed_public_key"),
    /** The name the claimant enrolled with; null for a claim from before Cloud kept names. */
    claimedName: text("claimed_name").$type<MachineName>(),
    claimedAt: timestamp("claimed_at", { mode: "date", withTimezone: true }),
    /** When this token first waited for a join slot: its place in the Organization's line. */
    queuedAt: timestamp("queued_at", { mode: "date", withTimezone: true }),
    /** Minted with Cloudflare Tunnel chosen: a Server that founds with it starts the Cluster with ports 80 and 443 closed. */
    ingressClosed: boolean("ingress_closed").default(false).notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    uniqueIndex("machine_enrollment_token_hash_idx").on(table.tokenHash),
    check("machine_enrollment_token_claim_check", sql`(${table.claimedMachineId} is null) = (${table.claimedPublicKey} is null)
      and (${table.claimedMachineId} is null) = (${table.claimedAt} is null)`),
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

export const clusterSealingKey = pgTable(
  "cluster_sealing_key",
  {
    organizationId: uuid("organization_id").notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    clusterKey: text("cluster_key").notNull(),
    publicKey: text("public_key").notNull(),
    /** The key new values seal to once a Server of this Cluster reported one, after activation. It never goes back. */
    recipientPublicKey: text("recipient_public_key"),
    createdAt,
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.clusterKey] }),
    check("cluster_sealing_key_cluster_key_check", sql`${table.clusterKey} ~ '^[0-9a-f]{64}$'`),
    check("cluster_sealing_key_public_key_check", sql`${table.publicKey} ~ '^[A-Za-z0-9+/]{43}=$'`),
    check("cluster_sealing_key_recipient_public_key_check", sql`${table.recipientPublicKey} ~ '^[A-Za-z0-9+/]{43}=$'`),
  ],
);

/**
 * The one Organization a Sealing public key is recorded for. The first to record it keeps it; that Organization's later
 * pairings of the same Cluster record it again, and another Organization's report of it is refused.
 */
export const sealingKeyOwner = pgTable(
  "sealing_key_owner",
  {
    publicKey: text("public_key").primaryKey(),
    organizationId: uuid("organization_id").notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    createdAt,
  },
  (table) => [
    check("sealing_key_owner_public_key_check", sql`${table.publicKey} ~ '^[A-Za-z0-9+/]{43}=$'`),
  ],
);

export const clusterSealingKeyGenerator = pgTable(
  "cluster_sealing_key_generator",
  {
    organizationId: uuid("organization_id").notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    clusterKey: text("cluster_key").notNull(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    createdAt,
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.clusterKey] }),
    check("cluster_sealing_key_generator_cluster_key_check", sql`${table.clusterKey} ~ '^[0-9a-f]{64}$'`),
    check("cluster_sealing_key_generator_machine_id_check", sql`${table.machineId} ~ '${sql.raw(MACHINE_ID_SQL)}'`),
  ],
);

export const organizationSealingKey = pgTable(
  "organization_sealing_key",
  {
    organizationId: uuid("organization_id").primaryKey()
      .references(() => organization.id, { onDelete: "cascade" }),
    publicKey: text("public_key").notNull(),
    recoveryBlob: text("recovery_blob"),
    /** Unused: the backup of a second key, from before Clusters kept one Sealing Key. */
    recipientPublicKey: text("recipient_public_key"),
    recipientBlob: text("recipient_blob"),
    recipientRotation: jsonb("recipient_rotation"),
    createdAt,
    updatedAt,
  },
  (table) => [
    check("organization_sealing_key_public_key_check", sql`${table.publicKey} ~ '^[A-Za-z0-9+/]{43}=$'`),
    check("organization_sealing_key_recovery_blob_check", sql`${table.recoveryBlob} ~ '^[A-Za-z0-9+/]{103}=$'`),
    check("organization_sealing_key_recipient_public_key_check", sql`${table.recipientPublicKey} ~ '^[A-Za-z0-9+/]{43}=$'`),
    check("organization_sealing_key_recipient_blob_check", sql`${table.recipientBlob} ~ '^[A-Za-z0-9+/]{103}=$'`),
    check(
      "organization_sealing_key_recipient_whole_check",
      sql`(${table.recipientPublicKey} IS NULL) = (${table.recipientBlob} IS NULL) AND (${table.recipientBlob} IS NULL) = (${table.recipientRotation} IS NULL)`,
    ),
  ],
);
