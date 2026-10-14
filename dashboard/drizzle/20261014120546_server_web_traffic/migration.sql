CREATE TABLE "server_web_traffic_attempt" (
	"id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"machine_id" text NOT NULL,
	"requested_by_user_id" uuid,
	"want" text NOT NULL,
	"state" text DEFAULT 'pending' NOT NULL,
	"inngest_run_id" text,
	"failure" text,
	"requested_at" timestamp with time zone DEFAULT now() NOT NULL,
	"started_at" timestamp with time zone,
	"ended_at" timestamp with time zone,
	CONSTRAINT "server_web_traffic_attempt_machine_id_check" CHECK ("machine_id" ~ '^[0-9a-f]{32}$'),
	CONSTRAINT "server_web_traffic_attempt_want_check" CHECK ("want" in ('on','off')),
	CONSTRAINT "server_web_traffic_attempt_state_check" CHECK ("state" in ('pending','running','finished','failed')),
	CONSTRAINT "server_web_traffic_attempt_failure_check" CHECK (("state" = 'failed') = ("failure" is not null)
        and ("failure" is null or length("failure") between 1 and 1024)),
	CONSTRAINT "server_web_traffic_attempt_run_check" CHECK ("inngest_run_id" is null or length("inngest_run_id") between 1 and 255),
	CONSTRAINT "server_web_traffic_attempt_state_shape_check" CHECK ((
        ("state" = 'pending' and "started_at" is null and "ended_at" is null)
        or ("state" = 'running' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is null)
        or ("state" = 'finished' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null)
        or ("state" = 'failed' and "ended_at" is not null)
      ))
);
--> statement-breakpoint
CREATE UNIQUE INDEX "server_web_traffic_attempt_one_active_idx" ON "server_web_traffic_attempt" ("organization_id","machine_id") WHERE "state" in ('pending', 'running');--> statement-breakpoint
CREATE UNIQUE INDEX "server_web_traffic_attempt_run_uidx" ON "server_web_traffic_attempt" ("inngest_run_id") WHERE "inngest_run_id" is not null;--> statement-breakpoint
CREATE INDEX "server_web_traffic_attempt_latest_idx" ON "server_web_traffic_attempt" ("organization_id","machine_id","requested_at");--> statement-breakpoint
ALTER TABLE "server_web_traffic_attempt" ADD CONSTRAINT "server_web_traffic_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "server_web_traffic_attempt" ADD CONSTRAINT "server_web_traffic_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
SELECT organization_change_attach('server_web_traffic_attempt', 'organization_id', 'id');
