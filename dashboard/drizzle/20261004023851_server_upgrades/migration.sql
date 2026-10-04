CREATE TABLE "organization_server_upgrades" (
	"organization_id" uuid PRIMARY KEY,
	"automatic" boolean DEFAULT false NOT NULL,
	"channel" text DEFAULT 'stable' NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_server_upgrades_channel_check" CHECK ("channel" in ('stable','beta'))
);
--> statement-breakpoint
CREATE TABLE "server_upgrade_attempt" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"machine_id" text NOT NULL,
	"attempt_id" text NOT NULL,
	"trigger" text NOT NULL,
	"requested_by_user_id" uuid,
	"channel" text NOT NULL,
	"from_version" text NOT NULL,
	"target_version" text,
	"outcome" text DEFAULT 'running' NOT NULL,
	"stage" text,
	"error" text,
	"inngest_run_id" text NOT NULL,
	"started_at" timestamp with time zone NOT NULL,
	"ended_at" timestamp with time zone,
	CONSTRAINT "server_upgrade_attempt_attempt_id_check" CHECK ("attempt_id" ~ '^[0-9a-f]{32}$'),
	CONSTRAINT "server_upgrade_attempt_machine_id_check" CHECK ("machine_id" ~ '^[0-9a-f]{32}$'),
	CONSTRAINT "server_upgrade_attempt_trigger_check" CHECK ("trigger" in ('automatic','manual')),
	CONSTRAINT "server_upgrade_attempt_channel_check" CHECK ("channel" in ('stable','beta')),
	CONSTRAINT "server_upgrade_attempt_outcome_check" CHECK ("outcome" in ('running','succeeded','failed','interrupted','unknown')),
	CONSTRAINT "server_upgrade_attempt_ended_check" CHECK (("outcome" = 'running') = ("ended_at" is null)),
	CONSTRAINT "server_upgrade_attempt_error_check" CHECK ("error" is null or "outcome" = 'failed')
);
--> statement-breakpoint
CREATE UNIQUE INDEX "server_upgrade_attempt_attempt_uidx" ON "server_upgrade_attempt" ("organization_id","machine_id","attempt_id");--> statement-breakpoint
CREATE INDEX "server_upgrade_attempt_latest_idx" ON "server_upgrade_attempt" ("organization_id","machine_id","started_at");--> statement-breakpoint
CREATE INDEX "server_upgrade_attempt_run_idx" ON "server_upgrade_attempt" ("inngest_run_id");--> statement-breakpoint
ALTER TABLE "organization_server_upgrades" ADD CONSTRAINT "organization_server_upgrades_eLbQmpv5tk3t_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "server_upgrade_attempt" ADD CONSTRAINT "server_upgrade_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "server_upgrade_attempt" ADD CONSTRAINT "server_upgrade_attempt_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
SELECT organization_change_attach('server_upgrade_attempt', 'organization_id', 'id');--> statement-breakpoint
SELECT organization_change_attach('organization_server_upgrades', 'organization_id', 'organization_id');
