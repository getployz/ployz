CREATE TABLE "volume_run" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"volume_id" text NOT NULL,
	"volume_name" text NOT NULL,
	"docker_volume" text NOT NULL,
	"kind" text NOT NULL,
	"args" jsonb NOT NULL,
	"orphan" boolean DEFAULT false NOT NULL,
	"refquota_bytes" bigint DEFAULT 0 NOT NULL,
	"state" text DEFAULT 'requested' NOT NULL,
	"lease" bigint,
	"inngest_run_id" text,
	"requested_by_user_id" uuid,
	"message" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"finished_at" timestamp with time zone,
	CONSTRAINT "volume_run_kind_check" CHECK ("kind" in ('mirror','sync','delete_mirror')),
	CONSTRAINT "volume_run_state_check" CHECK ("state" in ('requested','running','done','failed','cancelled','lost','not_started')),
	CONSTRAINT "volume_run_message_check" CHECK ("message" is null or length("message") between 1 and 1024)
);
--> statement-breakpoint
CREATE UNIQUE INDEX "volume_run_one_active_idx" ON "volume_run" ("volume_id") WHERE "state" in ('requested', 'running');--> statement-breakpoint
CREATE INDEX "volume_run_latest_idx" ON "volume_run" ("organization_id","volume_id","created_at" DESC NULLS LAST);--> statement-breakpoint
ALTER TABLE "volume_run" ADD CONSTRAINT "volume_run_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "volume_run" ADD CONSTRAINT "volume_run_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;