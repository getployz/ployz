CREATE TABLE "namespace_cleanup" (
	"id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"namespace" text NOT NULL,
	"requested_by_user_id" uuid,
	"approval_id" uuid,
	"confirm_data_loss" jsonb NOT NULL,
	"state" text DEFAULT 'pending' NOT NULL,
	"inngest_run_id" text,
	"volumes" jsonb,
	"end_code" text,
	"end_message" text,
	"requested_at" timestamp with time zone DEFAULT now() NOT NULL,
	"started_at" timestamp with time zone,
	"ended_at" timestamp with time zone,
	CONSTRAINT "namespace_cleanup_namespace_check" CHECK (length("namespace") between 1 and 255 and "namespace" !~ '[[:cntrl:]]'),
	CONSTRAINT "namespace_cleanup_confirm_data_loss_check" CHECK (jsonb_typeof("confirm_data_loss") = 'array'),
	CONSTRAINT "namespace_cleanup_state_check" CHECK ("state" in ('pending','running','finished','failed','cancelled','unknown')),
	CONSTRAINT "namespace_cleanup_end_code_check" CHECK ("end_code" is null or ("end_code", "state") in (('refused','failed'),('incomplete','failed'),('not_started','failed'),('cancelled','cancelled'),('interrupted','unknown'),('lost','unknown'))),
	CONSTRAINT "namespace_cleanup_end_message_check" CHECK (("end_code" is not distinct from 'refused' or "end_code" is not distinct from 'incomplete') = ("end_message" is not null)
        and ("end_message" is null or length("end_message") between 1 and 1024)),
	CONSTRAINT "namespace_cleanup_run_check" CHECK ("inngest_run_id" is null or length("inngest_run_id") between 1 and 255),
	CONSTRAINT "namespace_cleanup_state_shape_check" CHECK ((
        ("state" = 'pending'
          and "started_at" is null and "ended_at" is null and "volumes" is null and "end_code" is null)
        or ("state" = 'running' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is null and "volumes" is null and "end_code" is null)
        or ("state" = 'finished' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null
          and jsonb_typeof("volumes") = 'array' and "end_code" is null)
        or ("state" in ('failed', 'cancelled')
          and "ended_at" is not null and "volumes" is null and "end_code" is not null)
        or ("state" = 'unknown' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null and "volumes" is null
          and "end_code" is not null)
      ))
);
--> statement-breakpoint
ALTER TABLE "server_drain_attempt" ADD COLUMN "targets" jsonb;--> statement-breakpoint
ALTER TABLE "server_drain_attempt" ADD COLUMN "approval_id" uuid;--> statement-breakpoint
ALTER TABLE "machine_remove_attempt" ADD COLUMN "approval_id" uuid;--> statement-breakpoint
CREATE UNIQUE INDEX "namespace_cleanup_one_active_idx" ON "namespace_cleanup" ("organization_id","namespace") WHERE "state" in ('pending', 'running');--> statement-breakpoint
CREATE UNIQUE INDEX "namespace_cleanup_run_uidx" ON "namespace_cleanup" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "namespace_cleanup_approval_uidx" ON "namespace_cleanup" ("approval_id") WHERE "approval_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "server_drain_attempt_approval_uidx" ON "server_drain_attempt" ("approval_id") WHERE "approval_id" is not null;--> statement-breakpoint
CREATE UNIQUE INDEX "machine_remove_attempt_approval_uidx" ON "machine_remove_attempt" ("approval_id") WHERE "approval_id" is not null;--> statement-breakpoint
ALTER TABLE "namespace_cleanup" ADD CONSTRAINT "namespace_cleanup_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "namespace_cleanup" ADD CONSTRAINT "namespace_cleanup_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "server_drain_attempt" ADD CONSTRAINT "server_drain_attempt_targets_check" CHECK ("targets" is null or jsonb_typeof("targets") = 'array');--> statement-breakpoint
SELECT organization_change_attach('namespace_cleanup', 'organization_id', 'id');
