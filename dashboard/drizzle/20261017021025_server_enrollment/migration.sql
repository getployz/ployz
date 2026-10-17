CREATE TABLE "server_enrollment" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"waiting_server_id" uuid NOT NULL,
	"fleet_token_id" uuid NOT NULL,
	"role" text NOT NULL,
	"instruction" jsonb NOT NULL,
	"enroll_token_id" uuid NOT NULL,
	"encrypted_token" jsonb,
	"state" text NOT NULL,
	"stage" text,
	"machine_id" text,
	"failure" jsonb,
	"inngest_run_id" text,
	"requested_by_user_id" uuid,
	"requested_at" timestamp with time zone DEFAULT now() NOT NULL,
	"ended_at" timestamp with time zone
);
--> statement-breakpoint
ALTER TABLE "organization_pairing" ADD COLUMN "founder_claim_token_id" uuid;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD COLUMN "enrollment_role" text;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD COLUMN "server_enrollment_id" uuid;--> statement-breakpoint
CREATE UNIQUE INDEX "server_enrollment_one_open_idx" ON "server_enrollment" ("waiting_server_id") WHERE ended_at is null;--> statement-breakpoint
CREATE UNIQUE INDEX "server_enrollment_one_founder_idx" ON "server_enrollment" ("organization_id") WHERE ended_at is null and role = 'founder';--> statement-breakpoint
CREATE INDEX "server_enrollment_organization_idx" ON "server_enrollment" ("organization_id");--> statement-breakpoint
ALTER TABLE "server_enrollment" ADD CONSTRAINT "server_enrollment_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "server_enrollment" ADD CONSTRAINT "server_enrollment_waiting_server_id_waiting_server_id_fkey" FOREIGN KEY ("waiting_server_id") REFERENCES "waiting_server"("id") ON DELETE CASCADE;