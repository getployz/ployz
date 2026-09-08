import type { PruneRefusal } from "@ployz/sdk";
import { createdAt, type EncryptedSecretValue, type JsonValue, updatedAt } from "#/db/tables";

import { type DeploymentTriggerOrigin } from "#/modules/deployments/deployment";

import { type EnvironmentSnapshotVariableProducer } from "#/modules/environment-design/tables";

import { user } from "#/modules/identity/tables";

import { organization } from "#/modules/organization/tables";

import { environment } from "#/modules/project/tables";

import { type ServiceMode } from "#/modules/services/deploy-compile-types";

import { sql } from "drizzle-orm";

import { type AnyPgColumn, check, foreignKey, index, jsonb, pgTable, text, timestamp, unique, uniqueIndex, uuid } from "drizzle-orm/pg-core";



export const ENVIRONMENT_DEPLOYMENT_STATUSES = [
  "queued",
  "planning",
  "deploying",
  "applied",
  "failed",
  "cancelled",
] as const;

export type EnvironmentDeploymentStatus =
  (typeof ENVIRONMENT_DEPLOYMENT_STATUSES)[number];

export type RedactedDeployManifest = {
  version: 1 | 2;
  namespaceId: string;
  volumes?: Array<{
    name: string;
  }>;
  services: Array<{
    serviceId: string;
    image: string;
    mode: ServiceMode;
    environmentKeys: string[];
    hasRegistryCredential: boolean;
  }>;
};

export type EnvironmentDeploymentServiceActionPolicy = {
  kind: "all_affected_required";
};

export type EnvironmentDeploymentPreview = {
  storage?: JsonValue[];
  prune_refusal?: PruneRefusal | null;
  project_name: string;
  operations: JsonValue[];
  warnings: JsonValue[];
  would_remove: JsonValue[];
  volumes_to_create?: JsonValue[];
  preserved_volumes: JsonValue[];
};

export const environmentDeployment = pgTable(
  "environment_deployment",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    triggerOrigin: jsonb("trigger_origin")
      .notNull()
      .$type<DeploymentTriggerOrigin>(),
    savedStateSnapshotId: uuid("saved_state_snapshot_id").notNull(),
    serviceActionPolicy: jsonb("service_action_policy").$type<
      EnvironmentDeploymentServiceActionPolicy
    >(),
    status: text("status")
      .default("queued")
      .notNull()
      .$type<EnvironmentDeploymentStatus>(),
    inngestRunId: text("inngest_run_id"),
    coreDeployId: text("core_deploy_id"),
    retryOfDeploymentId: uuid("retry_of_deployment_id").references(
      (): AnyPgColumn => environmentDeployment.id,
      { onDelete: "no action" },
    ),
    variableProducers: jsonb("variable_producers").$type<
      EnvironmentSnapshotVariableProducer[] | null
    >(),
    deployManifest: jsonb("deploy_manifest").$type<RedactedDeployManifest>(),
    deployPreview:
      jsonb("deploy_preview").$type<EnvironmentDeploymentPreview>(),
    failureCode: text("failure_code"),
    failureMessage: text("failure_message"),
    message: text("message"),
    cancellationRequestedAt: timestamp("cancellation_requested_at", {
      mode: "date",
      withTimezone: true,
    }),
    dispatchRequestedAt: timestamp("dispatch_requested_at", {
      mode: "date",
      withTimezone: true,
    }),
    startedAt: timestamp("started_at", {
      mode: "date",
      withTimezone: true,
    }),
    finishedAt: timestamp("finished_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    index("environment_deployment_organization_id_idx").on(
      table.organizationId,
    ),
    index("environment_deployment_environment_id_idx").on(table.environmentId),
    index("environment_deployment_saved_state_snapshot_id_idx").on(
      table.savedStateSnapshotId,
    ),
    foreignKey({
      columns: [table.environmentId, table.savedStateSnapshotId],
      foreignColumns: [
        environmentSavedStateSnapshot.environmentId,
        environmentSavedStateSnapshot.id,
      ],
    }).onDelete("restrict"),
    uniqueIndex("environment_deployment_one_queued_target_idx")
      .on(table.environmentId)
      .where(sql`${table.status} = 'queued'`),
    uniqueIndex("environment_deployment_one_started_attempt_idx")
      .on(table.environmentId)
      .where(sql`${table.status} in ('planning','deploying')`),
    index("environment_deployment_inngest_run_id_idx").on(table.inngestRunId),
    index("environment_deployment_retry_of_idx").on(table.retryOfDeploymentId),
    check(
      "environment_deployment_cancellation_shape_check",
      sql`(
        (${table.status} = 'cancelled' and ${table.cancellationRequestedAt} is not null and ${table.finishedAt} is not null)
        or (${table.status} <> 'cancelled' and ${table.cancellationRequestedAt} is null)
      )`,
    ),
  ],
);

export const environmentDeploymentSecret = pgTable(
  "environment_deployment_secret",
  {
    environmentDeploymentId: uuid("environment_deployment_id")
      .primaryKey()
      .references(() => environmentDeployment.id, { onDelete: "cascade" }),
    encryptedRuntimeOutcome: jsonb("encrypted_runtime_outcome").$type<EncryptedSecretValue>(),
  },
);

export const environmentSavedStateSnapshot = pgTable(
  "environment_saved_state_snapshot",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    actorId: uuid("actor_id")
      .notNull()
      .references(() => user.id, { onDelete: "restrict" }),
    message: text("message"),
    // Parsed at the Saved State repository boundary. Keeping this unknown
    // prevents the database schema from depending on a domain compiler.
    intent: jsonb("intent").notNull(),
    // Admission authority is revision-owned metadata, not authored topology.
    // It is parsed alongside intent at the Saved State repository seam.
    volumeDeletionAuthorizations: jsonb(
      "volume_deletion_authorizations",
    ).notNull(),
    createdAt,
  },
  (table) => [
    unique().on(table.environmentId, table.id),
    index("environment_saved_state_snapshot_organization_id_idx").on(
      table.organizationId,
    ),
    index("environment_saved_state_snapshot_environment_created_at_idx").on(
      table.environmentId,
      table.createdAt,
    ),
  ],
);
