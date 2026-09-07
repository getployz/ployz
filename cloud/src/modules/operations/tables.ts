import { createdAt, type JsonObject, updatedAt } from "#/db/tables";

import { environmentDeployment } from "#/modules/deployments/tables";

import { type AttemptFailure, DESTRUCTIVE_VOLUME_ATTEMPT_DISPOSITIONS, type DestructiveVolumeAttemptDisposition, type ReviewedDestructiveVolumeEvidence, type ReviewedDestructiveVolumeTarget } from "#/modules/operations/destructive-volume-attempt";

import { organization } from "#/modules/organization/tables";

import { inArray, sql } from "drizzle-orm";

import { type AnyPgColumn, check, index, integer, jsonb, pgTable, text, timestamp, unique, uniqueIndex, uuid } from "drizzle-orm/pg-core";



export type CoreOperationWatchCursorState =
  | "more"
  | "caught_up"
  | "terminal";

export type CoreOperationObservationState =
  | "active"
  | "core_terminal"
  | "cloud_timeout"
  | "cloud_cancelled";

export type StoredCoreOperationExpectedKind =
  | "deploy"
  | "cert"
  | "machine_add"
  | "machine_update"
  | "machine_lifecycle"
  | "core_replace"
  | "credential_grant"
  | "network_repair"
  | "service_restart"
  | "managed_dns_reconcile"
  | "ingress_configure"
  | "ingress_refresh"
  | "namespace_remove"
  | "volume_create"
  | "volume_remove";

export const coreOperationWatch = pgTable(
  "core_operation_watch",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    operationId: text("operation_id").notNull(),
    expectedKind: text("expected_kind")
      .notNull()
      .$type<StoredCoreOperationExpectedKind>(),
    startSequence: text("start_sequence").notNull(),
    nextSequence: text("next_sequence").notNull(),
    cursorState: text("cursor_state")
      .notNull()
      .$type<CoreOperationWatchCursorState>(),
    observationState: text("observation_state")
      .notNull()
      .default("active")
      .$type<CoreOperationObservationState>(),
    observationDetail: jsonb("observation_detail").$type<JsonObject | null>(),
    inngestRunId: text("inngest_run_id"),
    deadlineAt: timestamp("deadline_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    terminalAt: timestamp("terminal_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.organizationId, table.operationId),
    check(
      "core_operation_watch_expected_kind_check",
      sql`${table.expectedKind} in ('deploy','cert','machine_add','machine_update','machine_lifecycle','core_replace','credential_grant','network_repair','service_restart','managed_dns_reconcile','ingress_configure','ingress_refresh','namespace_remove','volume_create','volume_remove')`,
    ),
    check(
      "core_operation_watch_cursor_state_check",
      sql`${table.cursorState} in ('more','caught_up','terminal')`,
    ),
    check(
      "core_operation_watch_observation_state_check",
      sql`${table.observationState} in ('active','core_terminal','cloud_timeout','cloud_cancelled')`,
    ),
  ],
);

export const coreOperationEvent = pgTable(
  "core_operation_event",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    watchId: uuid("watch_id")
      .notNull()
      .references(() => coreOperationWatch.id, { onDelete: "cascade" }),
    sequence: text("sequence").notNull(),
    eventType: text("event_type").notNull(),
    schemaVersion: integer("schema_version").default(1).notNull(),
    payload: jsonb("payload").notNull().$type<JsonObject>(),
    createdAt,
  },
  (table) => [unique().on(table.watchId, table.sequence)],
);

export const destructiveVolumeAttempt = pgTable(
  "destructive_volume_attempt",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentDeploymentId: uuid("environment_deployment_id")
      .notNull()
      .references(() => environmentDeployment.id, { onDelete: "restrict" }),
    environmentResourceId: uuid("environment_resource_id").notNull(),
    retryOfAttemptId: uuid("retry_of_attempt_id").references(
      (): AnyPgColumn => destructiveVolumeAttempt.id,
      { onDelete: "restrict" },
    ),
    target: jsonb("target").notNull().$type<ReviewedDestructiveVolumeTarget>(),
    evidence: jsonb("evidence")
      .notNull()
      .$type<ReviewedDestructiveVolumeEvidence>(),
    evidenceFingerprint: text("evidence_fingerprint").notNull(),
    disposition: text("disposition")
      .default("active")
      .notNull()
      .$type<DestructiveVolumeAttemptDisposition>(),
    operationId: text("operation_id"),
    startSequence: text("start_sequence"),
    inngestRunId: text("inngest_run_id"),
    requestPublishedAt: timestamp("request_published_at", {
      mode: "date",
      withTimezone: true,
    }),
    acceptedAt: timestamp("accepted_at", {
      mode: "date",
      withTimezone: true,
    }),
    terminalEvent: jsonb("terminal_event").$type<JsonObject | null>(),
    failure: jsonb("failure").$type<AttemptFailure | null>(),
    deadlineAt: timestamp("deadline_at", {
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
    index("destructive_volume_attempt_organization_id_idx").on(
      table.organizationId,
    ),
    index("destructive_volume_attempt_deployment_idx").on(
      table.environmentDeploymentId,
      table.createdAt,
    ),
    index("destructive_volume_attempt_resource_idx").on(
      table.environmentResourceId,
      table.createdAt,
    ),
    uniqueIndex("destructive_volume_attempt_retry_of_idx")
      .on(table.retryOfAttemptId)
      .where(sql`${table.retryOfAttemptId} is not null`),
    uniqueIndex("destructive_volume_attempt_one_active_target_idx")
      .on(table.environmentResourceId)
      .where(sql`${table.disposition} in ('active', 'accepted')`),
    index("destructive_volume_attempt_operation_id_idx")
      .on(table.operationId)
      .where(sql`${table.operationId} is not null`),
    uniqueIndex("destructive_volume_attempt_inngest_run_id_idx")
      .on(table.inngestRunId)
      .where(sql`${table.inngestRunId} is not null`),
    index("destructive_volume_attempt_unpublished_request_idx")
      .on(table.createdAt, table.id)
      .where(sql`${table.requestPublishedAt} is null`),
    check(
      "destructive_volume_attempt_disposition_check",
      inArray(table.disposition, [...DESTRUCTIVE_VOLUME_ATTEMPT_DISPOSITIONS]),
    ),
    check(
      "destructive_volume_attempt_identity_check",
      sql`${table.target}->>'resourceId' = ${table.environmentResourceId}::text
        and ${table.evidence}->>'fingerprint' = ${table.evidenceFingerprint}`,
    ),
    check(
      "destructive_volume_attempt_evidence_check",
      sql`(
        ${table.disposition} = 'active'
        and ${table.operationId} is null
        and ${table.startSequence} is null
        and ${table.acceptedAt} is null
        and ${table.terminalAt} is null
      ) or (
        ${table.disposition} = 'accepted'
        and ${table.operationId} is not null
        and ${table.startSequence} is not null
        and ${table.acceptedAt} is not null
        and ${table.terminalAt} is null
      ) or (
        ${table.disposition} in ('completed', 'partial', 'core_terminal', 'cloud_timeout')
        and ${table.operationId} is not null
        and ${table.startSequence} is not null
        and ${table.acceptedAt} is not null
        and ${table.terminalAt} is not null
      ) or (
        ${table.disposition} = 'cloud_cancelled'
        and (
          (${table.operationId} is null and ${table.startSequence} is null and ${table.acceptedAt} is null)
          or (${table.operationId} is not null and ${table.startSequence} is not null and ${table.acceptedAt} is not null)
        )
        and ${table.terminalAt} is not null
      ) or (
        ${table.disposition} = 'failed'
        and ${table.operationId} is null
        and ${table.startSequence} is null
        and ${table.acceptedAt} is null
        and ${table.terminalAt} is not null
      )`,
    ),
    check(
      "destructive_volume_attempt_terminal_payload_check",
      sql`(
        ${table.disposition} in ('active', 'accepted', 'completed')
        and ${table.failure} is null
      ) or (
        ${table.disposition} in ('partial', 'failed')
        and ${table.failure} is not null
      ) or ${table.disposition} in ('core_terminal', 'cloud_timeout', 'cloud_cancelled')`,
    ),
    check(
      "destructive_volume_attempt_workflow_check",
      sql`(${table.inngestRunId} is null) = (${table.deadlineAt} is null)`,
    ),
  ],
);
