CREATE TABLE "deployment_run" (
	"run_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"deployment_id" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "deployment_run" ADD CONSTRAINT "deployment_run_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;