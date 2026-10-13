CREATE TABLE "failed_restore" (
	"volume_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "failed_restore" ADD CONSTRAINT "failed_restore_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;