CREATE TABLE "conditional_save" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"pr_environment_id" uuid,
	"repository_id" bigint NOT NULL,
	"pr_number" integer NOT NULL,
	"destination_environment_id" uuid NOT NULL,
	"rows" jsonb NOT NULL,
	"picks" jsonb NOT NULL,
	"approved_against" jsonb NOT NULL,
	"landing" jsonb NOT NULL,
	"working_revision" uuid NOT NULL,
	"target_branch" text NOT NULL,
	"approved_by_user_id" uuid,
	"approved_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX "conditional_save_pr_destination_idx" ON "conditional_save" ("pr_environment_id","destination_environment_id");--> statement-breakpoint
CREATE INDEX "conditional_save_organization_idx" ON "conditional_save" ("organization_id");--> statement-breakpoint
CREATE INDEX "conditional_save_destination_idx" ON "conditional_save" ("destination_environment_id");--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_pr_environment_id_environment_id_fkey" FOREIGN KEY ("pr_environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_destination_environment_id_environment_id_fkey" FOREIGN KEY ("destination_environment_id") REFERENCES "environment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_approved_by_user_id_user_id_fkey" FOREIGN KEY ("approved_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_project_fkey" FOREIGN KEY ("organization_id","project_id") REFERENCES "project"("organization_id","id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('conditional_save', 'organization_id', 'id');
