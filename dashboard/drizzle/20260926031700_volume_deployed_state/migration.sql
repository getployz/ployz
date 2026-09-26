ALTER TABLE "environment_resource" ADD COLUMN "deployed_name" text;--> statement-breakpoint
ALTER TABLE "environment_resource" ADD COLUMN "removed_at" timestamp with time zone;