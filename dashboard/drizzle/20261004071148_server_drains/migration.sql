CREATE TABLE "server_drain_attempt" (
	"id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"machine_id" text NOT NULL,
	"requested_by_user_id" uuid,
	"state" text DEFAULT 'pending' NOT NULL,
	"inngest_run_id" text,
	"report" jsonb,
	"end_code" text,
	"refusal_message" text,
	"requested_at" timestamp with time zone DEFAULT now() NOT NULL,
	"started_at" timestamp with time zone,
	"ended_at" timestamp with time zone,
	CONSTRAINT "server_drain_attempt_machine_id_check" CHECK ("machine_id" ~ '^[0-9a-f]{32}$'),
	CONSTRAINT "server_drain_attempt_state_check" CHECK ("state" in ('pending','running','finished','failed','cancelled','unknown')),
	CONSTRAINT "server_drain_attempt_end_code_check" CHECK ("end_code" is null or ("end_code", "state") in (('refused','failed'),('not_started','failed'),('cancelled','cancelled'),('interrupted','unknown'),('lost','unknown'))),
	CONSTRAINT "server_drain_attempt_refusal_check" CHECK (("end_code" is not distinct from 'refused') = ("refusal_message" is not null)
        and ("refusal_message" is null or length("refusal_message") between 1 and 1024)),
	CONSTRAINT "server_drain_attempt_run_check" CHECK ("inngest_run_id" is null or length("inngest_run_id") between 1 and 255),
	CONSTRAINT "server_drain_attempt_state_shape_check" CHECK ((
        ("state" = 'pending'
          and "started_at" is null and "ended_at" is null and "report" is null and "end_code" is null)
        or ("state" = 'running' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is null and "report" is null and "end_code" is null)
        or ("state" = 'finished' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null
          and jsonb_typeof("report") = 'object' and "end_code" is null)
        or ("state" in ('failed', 'cancelled')
          and "ended_at" is not null and "report" is null and "end_code" is not null)
        or ("state" = 'unknown' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null and "report" is null
          and "end_code" is not null)
      ))
);
--> statement-breakpoint
CREATE UNIQUE INDEX "server_drain_attempt_one_active_idx" ON "server_drain_attempt" ("organization_id","machine_id") WHERE "state" in ('pending', 'running');--> statement-breakpoint
CREATE UNIQUE INDEX "server_drain_attempt_run_uidx" ON "server_drain_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE INDEX "server_drain_attempt_latest_idx" ON "server_drain_attempt" ("organization_id","machine_id","requested_at");--> statement-breakpoint
ALTER TABLE "server_drain_attempt" ADD CONSTRAINT "server_drain_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "server_drain_attempt" ADD CONSTRAINT "server_drain_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
SELECT organization_change_attach('server_drain_attempt', 'organization_id', 'id');
