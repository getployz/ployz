import type { RemovalEndpoint } from "#/modules/machines/pairing-removal";
import { createdAt, type EncryptedSecretValue, type JsonObject, type MachineId, sqlStringLiterals, updatedAt } from "#/db/tables";

import { environmentDeployment } from "#/modules/deployments/tables";

import { type CanvasNodeType } from "#/modules/environment-design/tables";

import { user } from "#/modules/identity/tables";

import { organization } from "#/modules/organization/tables";

import { environment } from "#/modules/project/tables";

import { type DataLossIdentity } from "#/modules/runtime/data-loss-identity";

import { TEARDOWN_ATTEMPT_STATUSES, TEARDOWN_SCOPES, type TeardownAttemptStatus, type TeardownOutcome, type TeardownScope, type TeardownTargets } from "#/modules/runtime/teardown";

import { VOLUME_REMOVE_ATTEMPT_STATUSES, type VolumeRemoveAttemptStatus, type VolumeRemoveOutcome, type VolumeRemoveVolume } from "#/modules/runtime/volume-removal";

import { sql } from "drizzle-orm";

import { type AnyPgColumn, check, foreignKey, index, integer, jsonb, pgTable, primaryKey, text, timestamp, unique, uniqueIndex, uuid } from "drizzle-orm/pg-core";



export const volumeRemoveAttempt = pgTable(
  "volume_remove_attempt",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    requestedByUserId: uuid("requested_by_user_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    environmentDeploymentId: uuid("environment_deployment_id").references(
      () => environmentDeployment.id,
      { onDelete: "cascade" },
    ),
    environmentResourceId: uuid("environment_resource_id"),
    retryOfAttemptId: uuid("retry_of_attempt_id").references(
      (): AnyPgColumn => volumeRemoveAttempt.id,
      { onDelete: "restrict" },
    ),
    volumes: jsonb("volumes").notNull().$type<VolumeRemoveVolume[]>(),
    status: text("status")
      .default("pending")
      .notNull()
      .$type<VolumeRemoveAttemptStatus>(),
    inngestRunId: text("inngest_run_id"),
    outcome: jsonb("outcome").$type<VolumeRemoveOutcome | null>(),
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
    index("volume_remove_attempt_organization_id_idx").on(table.organizationId),
    index("volume_remove_attempt_environment_id_idx").on(table.environmentId),
    index("volume_remove_attempt_deployment_idx").on(
      table.environmentDeploymentId,
      table.createdAt,
    ),
    index("volume_remove_attempt_resource_idx").on(
      table.environmentResourceId,
      table.createdAt,
    ),
    uniqueIndex("volume_remove_attempt_retry_of_idx")
      .on(table.retryOfAttemptId)
      .where(sql`${table.retryOfAttemptId} is not null`),
    uniqueIndex("volume_remove_attempt_one_active_resource_idx")
      .on(table.environmentResourceId)
      .where(
        sql`${table.status} in ('awaiting_deployment', 'pending', 'running')
          and ${table.environmentResourceId} is not null`,
      ),
    uniqueIndex("volume_remove_attempt_inngest_run_uidx")
      .on(table.inngestRunId)
      .where(sql`${table.inngestRunId} is not null`),
    check(
      "volume_remove_attempt_status_check",
      sql`${table.status} in (${sqlStringLiterals(VOLUME_REMOVE_ATTEMPT_STATUSES)})`,
    ),
    check(
      "volume_remove_attempt_volumes_check",
      sql`jsonb_typeof(${table.volumes}) = 'array'
        and jsonb_array_length(${table.volumes}) >= 1`,
    ),
    check(
      "volume_remove_attempt_status_shape_check",
      sql`(
        (${table.status} = 'awaiting_deployment'
          and ${table.environmentDeploymentId} is not null
          and ${table.inngestRunId} is null
          and ${table.startedAt} is null and ${table.terminalAt} is null
          and ${table.outcome} is null and ${table.failureMessage} is null)
        or (${table.status} = 'pending' and ${table.inngestRunId} is null
          and ${table.startedAt} is null and ${table.terminalAt} is null
          and ${table.outcome} is null and ${table.failureMessage} is null)
        or (${table.status} = 'running' and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.terminalAt} is null
          and ${table.outcome} is null and ${table.failureMessage} is null)
        or (${table.status} = 'unknown' and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.outcome} is null and ${table.failureMessage} is not null
          and length(${table.failureMessage}) between 1 and 2000)
        or (${table.status} = 'completed' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.outcome} is not null and ${table.failureMessage} is null)
        or (${table.status} = 'partial' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.outcome} is not null)
        or (${table.status} in ('failed', 'cancelled')
          and ${table.startedAt} is null and ${table.terminalAt} is not null
          and ${table.outcome} is null and ${table.failureMessage} is not null
          and length(${table.failureMessage}) between 1 and 2000
          and (
            (${table.inngestRunId} is not null
              and length(${table.inngestRunId}) between 1 and 255)
            or (${table.inngestRunId} is null
              and ${table.environmentDeploymentId} is not null)
          ))
      )`,
    ),
  ],
);

export const teardownAttempt = pgTable(
  "teardown_attempt",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id").notNull(),
    requestedByUserId: uuid("requested_by_user_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    projectId: uuid("project_id"),
    environmentId: uuid("environment_id"),
    scope: text("scope").notNull().$type<TeardownScope>(),
    confirmDataLoss: jsonb("confirm_data_loss")
      .notNull()
      .$type<DataLossIdentity[]>(),
    targets: jsonb("targets").notNull().$type<TeardownTargets>(),
    status: text("status")
      .default("pending")
      .notNull()
      .$type<TeardownAttemptStatus>(),
    inngestRunId: text("inngest_run_id"),
    outcome: jsonb("outcome").$type<TeardownOutcome | null>(),
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
    index("teardown_attempt_organization_id_idx").on(table.organizationId),
    index("teardown_attempt_project_id_idx").on(table.projectId),
    index("teardown_attempt_environment_id_idx").on(table.environmentId),
    uniqueIndex("teardown_attempt_one_active_environment_idx")
      .on(table.environmentId)
      .where(
        sql`${table.status} in ('pending', 'running')
          and ${table.scope} = 'environment'
          and ${table.environmentId} is not null`,
      ),
    uniqueIndex("teardown_attempt_one_active_project_idx")
      .on(table.projectId)
      .where(
        sql`${table.status} in ('pending', 'running')
          and ${table.scope} = 'project'
          and ${table.projectId} is not null`,
      ),
    uniqueIndex("teardown_attempt_one_active_organization_idx")
      .on(table.organizationId)
      .where(
        sql`${table.status} in ('pending', 'running')
          and ${table.scope} = 'organization'`,
      ),
    uniqueIndex("teardown_attempt_inngest_run_uidx")
      .on(table.inngestRunId)
      .where(sql`${table.inngestRunId} is not null`),
    check(
      "teardown_attempt_scope_check",
      sql`${table.scope} in (${sqlStringLiterals(TEARDOWN_SCOPES)})`,
    ),
    check(
      "teardown_attempt_status_check",
      sql`${table.status} in (${sqlStringLiterals(TEARDOWN_ATTEMPT_STATUSES)})`,
    ),
    check(
      "teardown_attempt_scope_ids_check",
      sql`(
        (${table.scope} = 'environment' and ${table.environmentId} is not null
          and ${table.projectId} is not null)
        or (${table.scope} = 'project' and ${table.projectId} is not null
          and ${table.environmentId} is null)
        or (${table.scope} = 'organization' and ${table.projectId} is null
          and ${table.environmentId} is null)
      )`,
    ),
    check(
      "teardown_attempt_confirm_data_loss_check",
      sql`jsonb_typeof(${table.confirmDataLoss}) = 'array'`,
    ),
    check(
      "teardown_attempt_targets_check",
      sql`jsonb_typeof(${table.targets}) = 'object'`,
    ),
    check(
      "teardown_attempt_status_shape_check",
      sql`(
        (${table.status} = 'pending' and ${table.inngestRunId} is null
          and ${table.startedAt} is null and ${table.terminalAt} is null
          and ${table.outcome} is null and ${table.failureMessage} is null)
        or (${table.status} = 'running' and ${table.inngestRunId} is not null
          and length(${table.inngestRunId}) between 1 and 255
          and ${table.startedAt} is not null and ${table.terminalAt} is null
          and ${table.failureMessage} is null)
        or (${table.status} = 'completed' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.outcome} is not null and ${table.failureMessage} is null)
        or (${table.status} = 'partial' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.outcome} is not null)
        or (${table.status} in ('failed', 'cancelled')
          and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.terminalAt} is not null
          and ${table.failureMessage} is not null
          and length(${table.failureMessage}) between 1 and 2000)
      )`,
    ),
  ],
);

export const environmentNodeConfigSnapshot = pgTable(
  "environment_node_config_snapshot",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentDeploymentId: uuid("environment_deployment_id")
      .notNull()
      .references(() => environmentDeployment.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    nodeType: text("node_type").notNull().$type<CanvasNodeType>(),
    nodeId: uuid("node_id").notNull(),
    nodeLineageId: uuid("node_lineage_id").notNull(),
    configVersion: integer("config_version").default(1).notNull(),
    config: jsonb("config").notNull().$type<JsonObject>(),
    createdAt,
    updatedAt,
  },
  (table) => [
    index("environment_node_config_snapshot_organization_id_idx").on(
      table.organizationId,
    ),
    check(
      "environment_node_config_snapshot_node_type_check",
      sql`${table.nodeType} in ('service', 'variable_group', 'volume')`,
    ),
    unique().on(table.environmentDeploymentId, table.nodeType, table.nodeId),
    index("environment_node_config_snapshot_environment_id_idx").on(
      table.environmentId,
    ),
    index("environment_node_config_snapshot_lineage_idx").on(
      table.nodeType,
      table.nodeLineageId,
    ),
    index("environment_node_config_snapshot_deployment_id_idx").on(
      table.environmentDeploymentId,
    ),
  ],
);

export const environmentNodeConfigSnapshotSecret = pgTable(
  "environment_node_config_snapshot_secret",
  {
    snapshotId: uuid("snapshot_id")
      .primaryKey()
      .references(() => environmentNodeConfigSnapshot.id, {
        onDelete: "cascade",
      }),
    encryptedRegistryUsername: jsonb(
      "encrypted_registry_username",
    ).$type<EncryptedSecretValue | null>(),
    encryptedRegistrySecret: jsonb("encrypted_registry_secret").$type<
      EncryptedSecretValue | null
    >(),
  },
  (table) => [
    check(
      "environment_node_config_snapshot_secret_nonempty_check",
      sql`num_nonnulls(${table.encryptedRegistryUsername}, ${table.encryptedRegistrySecret}) > 0`,
    ),
  ],
);

export const environmentNodeIntroduction = pgTable(
  "environment_node_introduction",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    nodeType: text("node_type").notNull().$type<CanvasNodeType>(),
    nodeId: uuid("node_id").notNull(),
    nodeLineageId: uuid("node_lineage_id").notNull(),
    configVersion: integer("config_version").default(1).notNull(),
    config: jsonb("config").notNull().$type<JsonObject>(),
    createdAt,
    updatedAt,
  },
  (table) => [
    primaryKey({
      columns: [table.environmentId, table.nodeType, table.nodeId],
    }),
    check(
      "environment_node_introduction_node_type_check",
      sql`${table.nodeType} in ('service', 'variable_group', 'volume')`
    ),
    index("environment_node_introduction_lineage_idx").on(
      table.nodeType,
      table.nodeLineageId
    ),
    index("environment_node_introduction_organization_idx").on(
      table.organizationId
    ),
  ]
);

export const environmentNodeIntroductionSecret = pgTable(
  "environment_node_introduction_secret",
  {
    environmentId: uuid("environment_id").notNull(),
    nodeType: text("node_type").notNull().$type<CanvasNodeType>(),
    nodeId: uuid("node_id").notNull(),
    authoredIntent: jsonb("authored_intent").notNull().$type<import("@ployz/sdk/config").SavedEnvironmentIntent>(),
  },
  (table) => [
    primaryKey({ columns: [table.environmentId, table.nodeType, table.nodeId] }),
    foreignKey({
      columns: [table.environmentId, table.nodeType, table.nodeId],
      foreignColumns: [
        environmentNodeIntroduction.environmentId,
        environmentNodeIntroduction.nodeType,
        environmentNodeIntroduction.nodeId,
      ],
    }).onDelete("cascade"),
  ],
);

/** One org = one rust cluster. Pairing secret is per-tenant. */
export const organizationPairing = pgTable(
  "organization_pairing",
  {
    organizationId: uuid("organization_id")
      .primaryKey()
      .references(() => organization.id, { onDelete: "cascade" }),
    encryptedPairingSecret: jsonb("encrypted_pairing_secret")
      .notNull()
      .$type<EncryptedSecretValue>(),
    removalStartedAt: timestamp("removal_started_at", { mode: "date", withTimezone: true }),
    removalEndpoints: jsonb("removal_endpoints").$type<readonly RemovalEndpoint[] | null>(),
    founderPublicKey: text("founder_public_key"),
    founderClaimMachineId: text("founder_claim_machine_id").notNull().$type<MachineId>(),
    founderMachineId: text("founder_machine_id").$type<MachineId>(),
    firstConnectDeploymentEvaluatedAt: timestamp(
      "first_connect_deployment_evaluated_at",
      { mode: "date", withTimezone: true },
    ),
    createdAt,
    updatedAt,
  },
  (table) => [
    check("organization_pairing_removal_shape_check", sql`
      (${table.removalStartedAt} is null and ${table.removalEndpoints} is null)
      or (${table.removalStartedAt} is not null and ${table.removalEndpoints} is not null and jsonb_typeof(${table.removalEndpoints}) = 'array')
    `),
    check(
      "organization_pairing_state_check",
      sql`${table.founderPublicKey} is not null or ${table.founderMachineId} is not null`,
    ),
    check(
      "organization_pairing_founder_claim_machine_id_check",
      sql`${table.founderClaimMachineId} ~ '^[0-9a-f]{32}$'`,
    ),
    check(
      "organization_pairing_founder_machine_id_check",
      sql`${table.founderMachineId} is null or ${table.founderMachineId} ~ '^[0-9a-f]{32}$'`,
    ),
  ],
);
