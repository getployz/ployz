ALTER TABLE "pr_environment" ADD COLUMN "shutdown" text;--> statement-breakpoint
CREATE UNIQUE INDEX "teardown_attempt_one_active_shutdown_idx" ON "teardown_attempt" ("environment_id") WHERE "status" in ('pending', 'running')
          and "scope" = 'shutdown';--> statement-breakpoint
ALTER TABLE "pr_environment" ADD CONSTRAINT "pr_environment_shutdown_check" CHECK ("shutdown" in ('running','off','failed'));--> statement-breakpoint
ALTER TABLE "teardown_attempt" DROP CONSTRAINT "teardown_attempt_scope_check", ADD CONSTRAINT "teardown_attempt_scope_check" CHECK ("scope" in ('environment','project','organization','shutdown'));--> statement-breakpoint
ALTER TABLE "teardown_attempt" DROP CONSTRAINT "teardown_attempt_scope_ids_check", ADD CONSTRAINT "teardown_attempt_scope_ids_check" CHECK ((
        ("scope" in ('environment', 'shutdown') and "environment_id" is not null
          and "project_id" is not null)
        or ("scope" = 'project' and "project_id" is not null
          and "environment_id" is null)
        or ("scope" = 'organization' and "project_id" is null
          and "environment_id" is null)
      ));