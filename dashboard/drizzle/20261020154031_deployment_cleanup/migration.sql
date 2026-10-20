CREATE TABLE "deployment_cleanup" (
	"deployment_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"run_id" text NOT NULL,
	"state" text NOT NULL,
	"left" jsonb DEFAULT '[]' NOT NULL,
	"message" text,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "deployment_cleanup_state_check" CHECK ("state" in ('waiting','done','left_running','cancelled','failed'))
);
--> statement-breakpoint
ALTER TABLE "deployment_cleanup" ADD CONSTRAINT "deployment_cleanup_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;