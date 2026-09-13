import { createdAt, sqlStringLiterals, updatedAt } from "#/db/tables";

import { GITHUB_CHECK_SUITE_ACTIONS, GITHUB_CHECK_SUITE_CONCLUSIONS, GITHUB_CHECK_SUITE_STATUSES, type GithubCheckSuiteAction, type GithubCheckSuiteConclusion, type GithubCheckSuiteStatus } from "#/modules/github/github-check-suite-vocabulary";

import { user } from "#/modules/identity/tables";

import { environment } from "#/modules/project/tables";

import { sql } from "drizzle-orm";

import { bigint, bigserial, boolean, check, foreignKey, index, integer, pgTable, primaryKey, text, timestamp, unique, uniqueIndex, uuid } from "drizzle-orm/pg-core";



export {
  GITHUB_CHECK_SUITE_ACTIONS,
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
};

export type {
  GithubCheckSuiteAction,
  GithubCheckSuiteConclusion,
  GithubCheckSuiteStatus,
};

export const GITHUB_WEBHOOK_EVENT_KINDS = ["push", "check_suite"] as const;

export type GithubWebhookEventKind =
  (typeof GITHUB_WEBHOOK_EVENT_KINDS)[number];

export const GITHUB_WEBHOOK_PROCESSING_STATES = [
  "received",
  "processing",
  "processed",
  "rejected",
  "failed",
  "cancelled",
] as const;

export type GithubWebhookProcessingState =
  (typeof GITHUB_WEBHOOK_PROCESSING_STATES)[number];

export const GITHUB_WEBHOOK_OUTCOMES = [
  "branch_projected",
  "branch_deleted",
  "branch_rebased_all_services",
  "check_suite_projected",
  "check_suite_unchanged",
  "ignored_stale",
  "ignored_unconfigured_repository",
  "ignored_no_matching_service",
  "malformed",
  "identity_unresolved",
  "unsupported_action",
  "processing_failed",
  "cancelled",
] as const;

export type GithubWebhookOutcome = (typeof GITHUB_WEBHOOK_OUTCOMES)[number];

export const GITHUB_WEBHOOK_FAILURE_CODES = [
  "malformed_payload",
  "identity_unresolved",
  "unsupported_action",
  "observation_failed",
  "persistence_failed",
  "publication_failed",
  "retry_exhausted",
  "unexpected_error",
  "inngest_cancelled",
] as const;

export type GithubWebhookFailureCode =
  (typeof GITHUB_WEBHOOK_FAILURE_CODES)[number];

export const GITHUB_BRANCH_STATES = ["active", "deleted"] as const;

export type GithubBranchState = (typeof GITHUB_BRANCH_STATES)[number];

export const GITHUB_BRANCH_EVALUATION_REASONS = [
  "first_observation",
  "changed_paths",
  "rebaseline_all_services",
  "branch_deleted",
] as const;

export type GithubBranchEvaluationReason =
  (typeof GITHUB_BRANCH_EVALUATION_REASONS)[number];

export const GITHUB_TRIGGER_SELECTION_MODES = [
  "paths",
  "all_services",
] as const;

export type GithubTriggerSelectionMode =
  (typeof GITHUB_TRIGGER_SELECTION_MODES)[number];

export const GITHUB_TRIGGER_REASONS = [
  "first_observation",
  "changed_paths",
  "force_rebaseline",
  "non_ancestor_rebaseline",
  "changed_paths_incomplete_rebaseline",
] as const;

export type GithubTriggerReason = (typeof GITHUB_TRIGGER_REASONS)[number];

export const GITHUB_OUTBOX_STATES = ["pending", "published"] as const;

export type GithubOutboxState = (typeof GITHUB_OUTBOX_STATES)[number];

export const githubInstallation = pgTable(
  "github_installation",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    installationId: integer("installation_id").notNull(),
    accountLogin: text("account_login").notNull(),
    accountType: text("account_type").notNull(),
    accountAvatarUrl: text("account_avatar_url"),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.userId, table.installationId),
    index("github_installation_user_idx").on(table.userId),
    index("github_installation_installation_id_idx").on(table.installationId),
  ],
);

export const githubRepositoryCache = pgTable(
  "github_repository_cache",
  {
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    installationId: integer("installation_id").notNull(),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    name: text("name").notNull(),
    fullName: text("full_name").notNull(),
    defaultBranch: text("default_branch").notNull(),
    private: boolean("private").notNull(),
    htmlUrl: text("html_url").notNull(),
    repoUpdatedAt: timestamp("repo_updated_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    syncedAt: timestamp("synced_at", {
      mode: "date",
      withTimezone: true,
    })
      .defaultNow()
      .notNull(),
  },
  (table) => [
    primaryKey({
      columns: [table.userId, table.installationId, table.repositoryId],
    }),
    index("github_repository_cache_installation_idx").on(table.installationId),
    index("github_repository_cache_user_idx").on(table.userId),
    index("github_repository_cache_full_name_idx").on(table.fullName),
  ],
);

export const githubWebhookDelivery = pgTable(
  "github_webhook_delivery",
  {
    deliveryId: text("delivery_id").primaryKey(),
    receiptSequence: bigserial("receipt_sequence", { mode: "number" })
      .notNull()
      .unique(),
    eventKind: text("event_kind").notNull().$type<GithubWebhookEventKind>(),
    processingState: text("processing_state")
      .default("received")
      .notNull()
      .$type<GithubWebhookProcessingState>(),
    outcome: text("outcome").$type<GithubWebhookOutcome | null>(),
    failureCode: text("failure_code").$type<GithubWebhookFailureCode | null>(),
    installationId: integer("installation_id"),
    repositoryId: bigint("repository_id", { mode: "number" }),
    ref: text("ref"),
    branchState: text("branch_state").$type<GithubBranchState | null>(),
    headSha: text("head_sha"),
    checkSuiteId: bigint("check_suite_id", { mode: "number" }),
    checkSuiteAction: text(
      "check_suite_action",
    ).$type<GithubCheckSuiteAction | null>(),
    checkSuiteStatus: text(
      "check_suite_status",
    ).$type<GithubCheckSuiteStatus | null>(),
    checkSuiteConclusion: text(
      "check_suite_conclusion",
    ).$type<GithubCheckSuiteConclusion | null>(),
    processingRunId: text("processing_run_id"),
    processingStartedAt: timestamp("processing_started_at", {
      mode: "date",
      withTimezone: true,
    }),
    processedAt: timestamp("processed_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.deliveryId, table.receiptSequence),
    index("github_webhook_delivery_processing_idx").on(
      table.processingState,
      table.receiptSequence,
    ),
    index("github_webhook_delivery_identity_idx").on(
      table.installationId,
      table.repositoryId,
    ),
    uniqueIndex("github_webhook_delivery_processing_run_id_idx")
      .on(table.processingRunId)
      .where(sql`${table.processingRunId} is not null`),
    check(
      "github_webhook_delivery_identity_check",
      sql`length(trim(${table.deliveryId})) > 0 and ${table.receiptSequence} > 0 and (${table.installationId} is null or ${table.installationId} > 0) and (${table.repositoryId} is null or ${table.repositoryId} > 0) and (${table.checkSuiteId} is null or ${table.checkSuiteId} > 0)`,
    ),
    check(
      "github_webhook_delivery_event_kind_check",
      sql`${table.eventKind} in ('push','check_suite')`,
    ),
    check(
      "github_webhook_delivery_processing_state_check",
      sql`${table.processingState} in ('received','processing','processed','rejected','failed','cancelled')`,
    ),
    check(
      "github_webhook_delivery_outcome_check",
      sql`${table.outcome} is null or ${table.outcome} in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service','malformed','identity_unresolved','unsupported_action','processing_failed','cancelled')`,
    ),
    check(
      "github_webhook_delivery_branch_state_check",
      sql`${table.branchState} is null or ${table.branchState} in ('active','deleted')`,
    ),
    check(
      "github_webhook_delivery_failure_code_check",
      sql`${table.failureCode} is null or ${table.failureCode} in ('malformed_payload','identity_unresolved','unsupported_action','observation_failed','persistence_failed','publication_failed','retry_exhausted','unexpected_error','inngest_cancelled')`,
    ),
    check(
      "github_webhook_delivery_check_suite_action_check",
      sql`${table.checkSuiteAction} is null or ${
        table.checkSuiteAction
      } in (${sqlStringLiterals(GITHUB_CHECK_SUITE_ACTIONS)})`,
    ),
    check(
      "github_webhook_delivery_check_suite_status_check",
      sql`${table.checkSuiteStatus} is null or ${
        table.checkSuiteStatus
      } in (${sqlStringLiterals(GITHUB_CHECK_SUITE_STATUSES)})`,
    ),
    check(
      "github_webhook_delivery_check_suite_conclusion_check",
      sql`${table.checkSuiteConclusion} is null or ${
        table.checkSuiteConclusion
      } in (${sqlStringLiterals(GITHUB_CHECK_SUITE_CONCLUSIONS)})`,
    ),
    check(
      "github_webhook_delivery_head_sha_check",
      sql`${table.headSha} is null or ${table.headSha} ~ '^[0-9a-f]{40}$'`,
    ),
    check(
      "github_webhook_delivery_processing_evidence_check",
      sql`(
        (${table.processingState} = 'received' and ${table.processingRunId} is null and ${table.processingStartedAt} is null and ${table.outcome} is null and ${table.failureCode} is null and ${table.processedAt} is null)
        or
        (${table.processingState} = 'processing' and ${table.processingRunId} is not null and ${table.processingStartedAt} is not null and ${table.outcome} is null and ${table.failureCode} is null and ${table.processedAt} is null)
        or
        (${table.processingState} = 'processed' and ${table.processingRunId} is not null and ${table.processingStartedAt} is not null and ${table.outcome} in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service') and ${table.failureCode} is null and ${table.processedAt} is not null)
        or
        (${table.processingState} = 'rejected' and (
          (${table.outcome} = 'malformed' and ${table.failureCode} = 'malformed_payload')
          or (${table.outcome} = 'identity_unresolved' and ${table.failureCode} = 'identity_unresolved')
          or (${table.outcome} = 'unsupported_action' and ${table.failureCode} = 'unsupported_action')
        ) and ${table.processingRunId} is null and ${table.processingStartedAt} is null and ${table.processedAt} is not null)
        or
        (${table.processingState} = 'failed' and ${table.processingRunId} is not null and ${table.processingStartedAt} is not null and ${table.outcome} = 'processing_failed' and ${table.failureCode} in ('observation_failed','persistence_failed','publication_failed','retry_exhausted','unexpected_error') and ${table.processedAt} is not null)
        or
        (${table.processingState} = 'cancelled' and ${table.processingRunId} is not null and ${table.processingStartedAt} is not null and ${table.outcome} = 'cancelled' and ${table.failureCode} = 'inngest_cancelled' and ${table.processedAt} is not null)
      )`,
    ),
    check(
      "github_webhook_delivery_processed_summary_check",
      sql`(
        ${table.processingState} not in ('processing','processed','failed','cancelled')
        or (
          ${table.installationId} is not null
          and ${table.repositoryId} is not null
          and (
            (
              ${table.eventKind} = 'push'
              and ${table.ref} is not null
              and ${table.ref} like 'refs/heads/%'
              and ${table.branchState} is not null
              and ${table.checkSuiteId} is null
              and ${table.checkSuiteAction} is null
              and ${table.checkSuiteStatus} is null
              and ${table.checkSuiteConclusion} is null
              and (
                (${table.branchState} = 'active' and ${table.headSha} is not null)
                or (${table.branchState} = 'deleted' and ${table.headSha} is null)
              )
            )
            or (
              ${table.eventKind} = 'check_suite'
              and ${table.branchState} is null
              and ${table.headSha} is not null
              and ${table.checkSuiteId} is not null
              and ${table.checkSuiteAction} is not null
              and ${table.checkSuiteStatus} is not null
            )
          )
        )
      )`,
    ),
    check(
      "github_webhook_delivery_outcome_kind_check",
      sql`(
        ${table.outcome} not in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged')
        or (${table.outcome} in ('branch_projected','branch_deleted','branch_rebased_all_services') and ${table.eventKind} = 'push')
        or (${table.outcome} in ('check_suite_projected','check_suite_unchanged') and ${table.eventKind} = 'check_suite')
      )`,
    ),
  ],
);

export const githubBranchProjection = pgTable(
  "github_branch_projection",
  {
    installationId: integer("installation_id").notNull(),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    ref: text("ref").notNull(),
    state: text("state").notNull().$type<GithubBranchState>(),
    evaluatedHeadSha: text("evaluated_head_sha"),
    evaluationReason: text("evaluation_reason")
      .notNull()
      .$type<GithubBranchEvaluationReason>(),
    lastDeliveryId: text("last_delivery_id").notNull(),
    lastReceiptSequence: bigint("last_receipt_sequence", {
      mode: "number",
    }).notNull(),
    evaluationRevision: bigint("evaluation_revision", { mode: "number" })
      .default(1)
      .notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    primaryKey({
      columns: [table.installationId, table.repositoryId, table.ref],
    }),
    index("github_branch_projection_head_idx").on(
      table.installationId,
      table.repositoryId,
      table.evaluatedHeadSha,
    ),
    foreignKey({
      columns: [table.lastDeliveryId, table.lastReceiptSequence],
      foreignColumns: [
        githubWebhookDelivery.deliveryId,
        githubWebhookDelivery.receiptSequence,
      ],
    }).onDelete("restrict"),
    check(
      "github_branch_projection_identity_check",
      sql`${table.installationId} > 0 and ${table.repositoryId} > 0 and ${table.lastReceiptSequence} > 0 and ${table.evaluationRevision} > 0`,
    ),
    check(
      "github_branch_projection_ref_check",
      sql`${table.ref} like 'refs/heads/%' and length(${table.ref}) > length('refs/heads/')`,
    ),
    check(
      "github_branch_projection_state_check",
      sql`${table.state} in ('active','deleted')`,
    ),
    check(
      "github_branch_projection_evaluation_reason_check",
      sql`${table.evaluationReason} in ('first_observation','changed_paths','rebaseline_all_services','branch_deleted')`,
    ),
    check(
      "github_branch_projection_head_check",
      sql`(
        (${table.state} = 'active' and ${table.evaluatedHeadSha} ~ '^[0-9a-f]{40}$' and ${table.evaluationReason} != 'branch_deleted')
        or (${table.state} = 'deleted' and ${table.evaluatedHeadSha} is null and ${table.evaluationReason} = 'branch_deleted')
      )`,
    ),
  ],
);

export const githubEnvironmentTrigger = pgTable(
  "github_environment_trigger",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    installationId: integer("installation_id").notNull(),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    ref: text("ref").notNull(),
    headSha: text("head_sha").notNull(),
    environmentId: uuid("environment_id")
      .notNull()
      .references(() => environment.id, { onDelete: "cascade" }),
    serviceIds: text("service_ids").array().notNull(),
    selectionMode: text("selection_mode")
      .notNull()
      .$type<GithubTriggerSelectionMode>(),
    reason: text("reason").notNull().$type<GithubTriggerReason>(),
    sourceDeliveryId: text("source_delivery_id").notNull(),
    sourceReceiptSequence: bigint("source_receipt_sequence", {
      mode: "number",
    }).notNull(),
    branchEvaluationRevision: bigint("branch_evaluation_revision", {
      mode: "number",
    })
      .default(1)
      .notNull(),
    triggerRevision: bigint("trigger_revision", { mode: "number" })
      .default(1)
      .notNull(),
    publishedRevision: bigint("published_revision", { mode: "number" })
      .default(0)
      .notNull(),
    publishState: text("publish_state")
      .default("pending")
      .notNull()
      .$type<GithubOutboxState>(),
    publishedAt: timestamp("published_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(
      table.installationId,
      table.repositoryId,
      table.ref,
      table.branchEvaluationRevision,
      table.environmentId,
    ),
    index("github_environment_trigger_environment_idx").on(
      table.environmentId,
      table.createdAt,
    ),
    index("github_environment_trigger_pending_idx")
      .on(table.createdAt)
      .where(sql`${table.publishState} = 'pending'`),
    foreignKey({
      columns: [table.sourceDeliveryId, table.sourceReceiptSequence],
      foreignColumns: [
        githubWebhookDelivery.deliveryId,
        githubWebhookDelivery.receiptSequence,
      ],
    }).onDelete("restrict"),
    check(
      "github_environment_trigger_identity_check",
      sql`${table.installationId} > 0 and ${table.repositoryId} > 0 and ${table.sourceReceiptSequence} > 0 and ${table.branchEvaluationRevision} > 0`,
    ),
    check(
      "github_environment_trigger_ref_check",
      sql`${table.ref} like 'refs/heads/%' and length(${table.ref}) > length('refs/heads/')`,
    ),
    check(
      "github_environment_trigger_head_sha_check",
      sql`${table.headSha} ~ '^[0-9a-f]{40}$'`,
    ),
    check(
      "github_environment_trigger_service_ids_check",
      sql`cardinality(${table.serviceIds}) > 0 and array_position(${table.serviceIds}, null) is null`,
    ),
    check(
      "github_environment_trigger_selection_mode_check",
      sql`${table.selectionMode} in ('paths','all_services')`,
    ),
    check(
      "github_environment_trigger_reason_check",
      sql`${table.reason} in ('first_observation','changed_paths','force_rebaseline','non_ancestor_rebaseline','changed_paths_incomplete_rebaseline')`,
    ),
    check(
      "github_environment_trigger_selection_reason_check",
      sql`(
        (${table.selectionMode} = 'paths' and ${table.reason} = 'changed_paths')
        or (${table.selectionMode} = 'all_services' and ${table.reason} in ('first_observation','force_rebaseline','non_ancestor_rebaseline','changed_paths_incomplete_rebaseline'))
      )`,
    ),
    check(
      "github_environment_trigger_publish_revision_check",
      sql`${table.triggerRevision} = ${table.branchEvaluationRevision} and ${table.publishedRevision} >= 0 and ${table.publishedRevision} <= ${table.triggerRevision}`,
    ),
    check(
      "github_environment_trigger_publish_state_check",
      sql`(
        (${table.publishState} = 'pending' and ${table.publishedRevision} < ${table.triggerRevision} and ${table.publishedAt} is null)
        or (${table.publishState} = 'published' and ${table.publishedRevision} = ${table.triggerRevision} and ${table.publishedAt} is not null)
      )`,
    ),
  ],
);

export const githubCheckSuiteProjection = pgTable(
  "github_check_suite_projection",
  {
    installationId: integer("installation_id").notNull(),
    repositoryId: bigint("repository_id", { mode: "number" }).notNull(),
    checkSuiteId: bigint("check_suite_id", { mode: "number" }).notNull(),
    headSha: text("head_sha").notNull(),
    status: text("status").notNull().$type<GithubCheckSuiteStatus>(),
    conclusion: text("conclusion").$type<GithubCheckSuiteConclusion | null>(),
    sourceUpdatedAt: timestamp("source_updated_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    lastDeliveryId: text("last_delivery_id").notNull(),
    lastReceiptSequence: bigint("last_receipt_sequence", {
      mode: "number",
    }).notNull(),
    transitionRevision: integer("transition_revision").default(1).notNull(),
    publishedRevision: integer("published_revision").default(0).notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    primaryKey({
      columns: [table.installationId, table.repositoryId, table.checkSuiteId],
    }),
    index("github_check_suite_projection_head_idx").on(
      table.installationId,
      table.repositoryId,
      table.headSha,
    ),
    index("github_check_suite_projection_unpublished_idx")
      .on(table.updatedAt)
      .where(sql`${table.publishedRevision} < ${table.transitionRevision}`),
    foreignKey({
      columns: [table.lastDeliveryId, table.lastReceiptSequence],
      foreignColumns: [
        githubWebhookDelivery.deliveryId,
        githubWebhookDelivery.receiptSequence,
      ],
    }).onDelete("restrict"),
    check(
      "github_check_suite_projection_identity_check",
      sql`${table.installationId} > 0 and ${table.repositoryId} > 0 and ${table.checkSuiteId} > 0 and ${table.lastReceiptSequence} > 0`,
    ),
    check(
      "github_check_suite_projection_head_sha_check",
      sql`${table.headSha} ~ '^[0-9a-f]{40}$'`,
    ),
    check(
      "github_check_suite_projection_status_check",
      sql`${table.status} in ('queued','in_progress','completed','pending','waiting','requested')`,
    ),
    check(
      "github_check_suite_projection_conclusion_check",
      sql`${table.conclusion} is null or ${table.conclusion} in ('action_required','cancelled','failure','neutral','success','skipped','stale','timed_out','startup_failure')`,
    ),
    check(
      "github_check_suite_projection_revision_check",
      sql`${table.transitionRevision} >= 1 and ${table.publishedRevision} >= 0 and ${table.publishedRevision} <= ${table.transitionRevision}`,
    ),
  ],
);
