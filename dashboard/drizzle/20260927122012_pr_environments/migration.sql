ALTER TABLE "environment_branch" ADD COLUMN "pr_repository_id" bigint;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_repository" text;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_number" integer;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_title" text;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_author" text;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_head_branch" text;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_head_sha" text;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD COLUMN "pr_target_branch" text;--> statement-breakpoint
CREATE UNIQUE INDEX "environment_branch_pull_request_idx" ON "environment_branch" ("project_id","pr_repository_id","pr_number");--> statement-breakpoint
ALTER TABLE "environment_branch" ADD CONSTRAINT "environment_branch_pull_request" CHECK (num_nulls("pr_repository_id", "pr_repository", "pr_number", "pr_title", "pr_author", "pr_head_branch", "pr_head_sha", "pr_target_branch") in (0, 8));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_outcome_check", ADD CONSTRAINT "github_webhook_delivery_outcome_check" CHECK ("outcome" is null or "outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service','ignored_fork','ignored_pull_request','ignored_nothing_from_repository','pull_request_projected','malformed','identity_unresolved','unsupported_action','processing_failed','cancelled'));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_processing_evidence_check", ADD CONSTRAINT "github_webhook_delivery_processing_evidence_check" CHECK ((
        ("processing_state" = 'received' and "processing_run_id" is null and "processing_started_at" is null and "outcome" is null and "failure_code" is null and "processed_at" is null)
        or
        ("processing_state" = 'processing' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" is null and "failure_code" is null and "processed_at" is null)
        or
        ("processing_state" = 'processed' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service','ignored_fork','ignored_pull_request','ignored_nothing_from_repository','pull_request_projected') and "failure_code" is null and "processed_at" is not null)
        or
        ("processing_state" = 'rejected' and (
          ("outcome" = 'malformed' and "failure_code" = 'malformed_payload')
          or ("outcome" = 'identity_unresolved' and "failure_code" = 'identity_unresolved')
          or ("outcome" = 'unsupported_action' and "failure_code" = 'unsupported_action')
        ) and "processing_run_id" is null and "processing_started_at" is null and "processed_at" is not null)
        or
        ("processing_state" = 'failed' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" = 'processing_failed' and "failure_code" in ('observation_failed','persistence_failed','publication_failed','retry_exhausted','unexpected_error') and "processed_at" is not null)
        or
        ("processing_state" = 'cancelled' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" = 'cancelled' and "failure_code" = 'inngest_cancelled' and "processed_at" is not null)
      ));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_outcome_kind_check", ADD CONSTRAINT "github_webhook_delivery_outcome_kind_check" CHECK ((
        "outcome" not in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_fork','ignored_pull_request','ignored_nothing_from_repository','pull_request_projected')
        or ("outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services') and "event_kind" = 'push')
        or ("outcome" in ('check_suite_projected','check_suite_unchanged') and "event_kind" = 'check_suite')
        or ("outcome" in ('ignored_fork','ignored_pull_request','ignored_nothing_from_repository','pull_request_projected') and "event_kind" = 'pull_request')
      ));