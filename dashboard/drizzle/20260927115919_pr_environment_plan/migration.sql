CREATE TABLE "pr_environment_plan" (
	"organization_id" uuid NOT NULL,
	"project_id" uuid,
	"repository_id" bigint,
	"installation_id" bigint NOT NULL,
	"repository" text NOT NULL,
	"enabled" boolean DEFAULT false NOT NULL,
	"start_from_environment_id" uuid,
	"picks" jsonb DEFAULT '{"preset":"only"}' NOT NULL,
	"setup_commands" jsonb DEFAULT '[]' NOT NULL,
	"remove_on_close" boolean DEFAULT true NOT NULL,
	"include_bots" boolean DEFAULT false NOT NULL,
	"enabled_by_user_id" uuid,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "pr_environment_plan_pkey" PRIMARY KEY("project_id","repository_id")
);
--> statement-breakpoint
CREATE INDEX "pr_environment_plan_organization_idx" ON "pr_environment_plan" ("organization_id");--> statement-breakpoint
ALTER TABLE "pr_environment_plan" ADD CONSTRAINT "pr_environment_plan_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "pr_environment_plan" ADD CONSTRAINT "pr_environment_plan_enabled_by_user_id_user_id_fkey" FOREIGN KEY ("enabled_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "pr_environment_plan" ADD CONSTRAINT "pr_environment_plan_start_from_fkey" FOREIGN KEY ("start_from_environment_id") REFERENCES "environment"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "pr_environment_plan" ADD CONSTRAINT "pr_environment_plan_project_fkey" FOREIGN KEY ("organization_id","project_id") REFERENCES "project"("organization_id","id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('pr_environment_plan', 'organization_id', 'project_id', 'repository_id');
