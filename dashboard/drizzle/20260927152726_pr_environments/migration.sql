CREATE TABLE "conditional_save" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"state" text DEFAULT 'standing' NOT NULL,
	"pr_environment_id" uuid,
	"repository_id" bigint NOT NULL,
	"pr_number" integer NOT NULL,
	"destination_environment_id" uuid NOT NULL,
	"rows" jsonb NOT NULL,
	"picks" jsonb NOT NULL,
	"landing" jsonb NOT NULL,
	"working_revision" uuid NOT NULL,
	"target_branch" text NOT NULL,
	"approved_by_user_id" uuid,
	"approved_at" timestamp with time zone DEFAULT now() NOT NULL,
	"merge_commit_sha" text,
	"landed_saved_state_id" uuid,
	CONSTRAINT "conditional_save_state_check" CHECK ((
      "state" = 'standing' and "pr_environment_id" is not null and "merge_commit_sha" is null and "landed_saved_state_id" is null
    ) or (
      "state" = 'frozen' and "pr_environment_id" is null and "merge_commit_sha" is not null and "landed_saved_state_id" is null
    ) or (
      "state" = 'landed' and "pr_environment_id" is null and "merge_commit_sha" is not null and "landed_saved_state_id" is not null
    ))
);
--> statement-breakpoint
CREATE TABLE "pr_environment" (
	"environment_id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"repository_id" bigint NOT NULL,
	"number" integer NOT NULL,
	"title" text NOT NULL,
	"author" text NOT NULL,
	"head_branch" text NOT NULL,
	"target_branch" text NOT NULL,
	"commits" integer NOT NULL,
	"closed" boolean DEFAULT false NOT NULL
);
--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ADD COLUMN "conditional_save_ids" uuid[] DEFAULT '{}'::uuid[] NOT NULL;--> statement-breakpoint
CREATE UNIQUE INDEX "conditional_save_pr_destination_idx" ON "conditional_save" ("pr_environment_id","destination_environment_id");--> statement-breakpoint
CREATE INDEX "conditional_save_organization_idx" ON "conditional_save" ("organization_id");--> statement-breakpoint
CREATE INDEX "conditional_save_destination_idx" ON "conditional_save" ("destination_environment_id");--> statement-breakpoint
CREATE UNIQUE INDEX "pr_environment_pull_request_idx" ON "pr_environment" ("repository_id","number","project_id") WHERE not "closed";--> statement-breakpoint
CREATE INDEX "pr_environment_organization_idx" ON "pr_environment" ("organization_id");--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_pr_environment_id_environment_id_fkey" FOREIGN KEY ("pr_environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_destination_environment_id_environment_id_fkey" FOREIGN KEY ("destination_environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_approved_by_user_id_user_id_fkey" FOREIGN KEY ("approved_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_project_fkey" FOREIGN KEY ("organization_id","project_id") REFERENCES "project"("organization_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "pr_environment" ADD CONSTRAINT "pr_environment_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "pr_environment" ADD CONSTRAINT "pr_environment_branch_fkey" FOREIGN KEY ("environment_id") REFERENCES "environment_branch"("environment_id") ON DELETE CASCADE;--> statement-breakpoint
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
      ));--> statement-breakpoint
SELECT organization_change_attach('conditional_save', 'organization_id', 'id');--> statement-breakpoint
SELECT organization_change_attach('pr_environment', 'organization_id', 'environment_id');
