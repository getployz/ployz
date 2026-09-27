CREATE TABLE "environment_branch" (
	"environment_id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"project_id" uuid NOT NULL,
	"parent_environment_id" uuid NOT NULL,
	"kept" boolean DEFAULT false NOT NULL,
	"base" jsonb NOT NULL,
	"setup_commands" jsonb DEFAULT '[]' NOT NULL,
	"created_by_user_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_branch_not_own_parent" CHECK ("environment_id" <> "parent_environment_id")
);
--> statement-breakpoint
CREATE INDEX "environment_branch_organization_idx" ON "environment_branch" ("organization_id");--> statement-breakpoint
CREATE INDEX "environment_branch_parent_idx" ON "environment_branch" ("parent_environment_id");--> statement-breakpoint
ALTER TABLE "environment_branch" ADD CONSTRAINT "environment_branch_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD CONSTRAINT "environment_branch_created_by_user_id_user_id_fkey" FOREIGN KEY ("created_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD CONSTRAINT "environment_branch_environment_fkey" FOREIGN KEY ("project_id","environment_id") REFERENCES "environment"("project_id","id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_branch" ADD CONSTRAINT "environment_branch_parent_fkey" FOREIGN KEY ("project_id","parent_environment_id") REFERENCES "environment"("project_id","id");--> statement-breakpoint
SELECT organization_change_attach('environment_branch', 'organization_id', 'environment_id');
