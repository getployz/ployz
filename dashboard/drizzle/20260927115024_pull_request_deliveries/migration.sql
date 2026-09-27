ALTER TABLE "github_webhook_delivery" ADD COLUMN "pull_request_number" bigint;--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" ADD COLUMN "pull_request_action" text;--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" ADD CONSTRAINT "github_webhook_delivery_pull_request_action_check" CHECK ("pull_request_action" is null or "pull_request_action" in ('opened','reopened','synchronize','closed','edited'));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_identity_check", ADD CONSTRAINT "github_webhook_delivery_identity_check" CHECK (length(trim("delivery_id")) > 0 and "receipt_sequence" > 0 and ("installation_id" is null or "installation_id" > 0) and ("repository_id" is null or "repository_id" > 0) and ("check_suite_id" is null or "check_suite_id" > 0) and ("pull_request_number" is null or "pull_request_number" > 0));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_event_kind_check", ADD CONSTRAINT "github_webhook_delivery_event_kind_check" CHECK ("event_kind" in ('push','check_suite','pull_request'));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_outcome_check", ADD CONSTRAINT "github_webhook_delivery_outcome_check" CHECK ("outcome" is null or "outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service','ignored_fork','ignored_pull_request','malformed','identity_unresolved','unsupported_action','processing_failed','cancelled'));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_processing_evidence_check", ADD CONSTRAINT "github_webhook_delivery_processing_evidence_check" CHECK ((
        ("processing_state" = 'received' and "processing_run_id" is null and "processing_started_at" is null and "outcome" is null and "failure_code" is null and "processed_at" is null)
        or
        ("processing_state" = 'processing' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" is null and "failure_code" is null and "processed_at" is null)
        or
        ("processing_state" = 'processed' and "processing_run_id" is not null and "processing_started_at" is not null and "outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_stale','ignored_unconfigured_repository','ignored_no_matching_service','ignored_fork','ignored_pull_request') and "failure_code" is null and "processed_at" is not null)
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
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_processed_summary_check", ADD CONSTRAINT "github_webhook_delivery_processed_summary_check" CHECK ((
        "processing_state" not in ('processing','processed','failed','cancelled')
        or (
          "installation_id" is not null
          and "repository_id" is not null
          and (
            (
              "event_kind" = 'push'
              and "ref" is not null
              and "ref" like 'refs/heads/%'
              and "branch_state" is not null
              and "check_suite_id" is null
              and "check_suite_action" is null
              and "check_suite_status" is null
              and "check_suite_conclusion" is null
              and "pull_request_number" is null
              and "pull_request_action" is null
              and (
                ("branch_state" = 'active' and "head_sha" is not null)
                or ("branch_state" = 'deleted' and "head_sha" is null)
              )
            )
            or (
              "event_kind" = 'check_suite'
              and "branch_state" is null
              and "head_sha" is not null
              and "check_suite_id" is not null
              and "check_suite_action" is not null
              and "check_suite_status" is not null
              and "pull_request_number" is null
              and "pull_request_action" is null
            )
            or (
              "event_kind" = 'pull_request'
              and "ref" is null
              and "branch_state" is null
              and "head_sha" is not null
              and "check_suite_id" is null
              and "check_suite_action" is null
              and "check_suite_status" is null
              and "check_suite_conclusion" is null
              and "pull_request_number" is not null
              and "pull_request_action" is not null
            )
          )
        )
      ));--> statement-breakpoint
ALTER TABLE "github_webhook_delivery" DROP CONSTRAINT "github_webhook_delivery_outcome_kind_check", ADD CONSTRAINT "github_webhook_delivery_outcome_kind_check" CHECK ((
        "outcome" not in ('branch_projected','branch_deleted','branch_rebased_all_services','check_suite_projected','check_suite_unchanged','ignored_fork','ignored_pull_request')
        or ("outcome" in ('branch_projected','branch_deleted','branch_rebased_all_services') and "event_kind" = 'push')
        or ("outcome" in ('check_suite_projected','check_suite_unchanged') and "event_kind" = 'check_suite')
        or ("outcome" in ('ignored_fork','ignored_pull_request') and "event_kind" = 'pull_request')
      ));