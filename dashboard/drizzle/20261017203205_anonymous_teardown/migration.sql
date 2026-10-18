CREATE TABLE "anonymous_teardown" (
	"organization_id" uuid PRIMARY KEY,
	"waiting_since" timestamp with time zone DEFAULT clock_timestamp() NOT NULL,
	"silent_since" timestamp with time zone,
	"warned_at" timestamp with time zone
);
--> statement-breakpoint
ALTER TABLE "organization_pairing" ADD COLUMN "removal_answered_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "anonymous_teardown" ADD CONSTRAINT "anonymous_teardown_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;