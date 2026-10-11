CREATE TABLE "uptime_alert" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"incident_id" uuid NOT NULL,
	"kind" text NOT NULL,
	"payload" jsonb NOT NULL,
	"state" text DEFAULT 'pending' NOT NULL,
	"attempts" integer DEFAULT 0 NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	"sent_at" timestamp with time zone,
	CONSTRAINT "uptime_alert_kind_check" CHECK ("kind" in ('opened', 'resolved')),
	CONSTRAINT "uptime_alert_state_check" CHECK ("state" in ('pending', 'sending', 'sent', 'failed', 'expired')),
	CONSTRAINT "uptime_alert_sent_check" CHECK (("state" = 'sent') = ("sent_at" is not null))
);
--> statement-breakpoint
CREATE TABLE "uptime_check" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"name" text NOT NULL,
	"url" text NOT NULL,
	"interval_seconds" integer DEFAULT 300 NOT NULL,
	"timeout_ms" integer DEFAULT 10000 NOT NULL,
	"expect" text DEFAULT 'below_400' NOT NULL,
	"enabled" boolean DEFAULT true NOT NULL,
	"revision" integer DEFAULT 1 NOT NULL,
	"source" text DEFAULT 'service' NOT NULL,
	"environment_id" text NOT NULL,
	"service_id" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "uptime_check_name_check" CHECK (length("name") between 1 and 128),
	CONSTRAINT "uptime_check_url_check" CHECK ("url" ~ '^https?://[^/?#@[:space:]]+([/?]|$)' and length("url") <= 2048),
	CONSTRAINT "uptime_check_interval_check" CHECK ("interval_seconds" between 30 and 3600),
	CONSTRAINT "uptime_check_timeout_check" CHECK ("timeout_ms" between 1000 and 30000 and "timeout_ms" < "interval_seconds" * 1000),
	CONSTRAINT "uptime_check_expect_check" CHECK ("expect" in ('below_400')),
	CONSTRAINT "uptime_check_revision_check" CHECK ("revision" >= 1),
	CONSTRAINT "uptime_check_source_check" CHECK ("source" in ('service'))
);
--> statement-breakpoint
CREATE TABLE "uptime_incident" (
	"id" uuid PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"check_id" uuid NOT NULL,
	"check_revision" integer NOT NULL,
	"opened_at" timestamp with time zone NOT NULL,
	"opened_by" jsonb NOT NULL,
	"closed_at" timestamp with time zone,
	"closed_by" jsonb,
	CONSTRAINT "uptime_incident_closed_check" CHECK (("closed_at" is null) = ("closed_by" is null)),
	CONSTRAINT "uptime_incident_order_check" CHECK ("closed_at" is null or "closed_at" >= "opened_at")
);
--> statement-breakpoint
CREATE TABLE "uptime_result" (
	"check_id" uuid,
	"organization_id" uuid NOT NULL,
	"region" text,
	"scheduled_at" timestamp with time zone,
	"kind" text,
	"start_lag_ms" integer NOT NULL,
	"verdict" text NOT NULL,
	"status" smallint NOT NULL,
	"failed_phase" text NOT NULL,
	"error" text NOT NULL,
	"detail" text NOT NULL,
	"dns_ms" integer NOT NULL,
	"connect_ms" integer NOT NULL,
	"tls_ms" integer NOT NULL,
	"ttfb_ms" integer NOT NULL,
	"total_ms" integer NOT NULL,
	"remote_ip" inet,
	"cert_not_after" timestamp with time zone,
	"agent" text NOT NULL,
	CONSTRAINT "uptime_result_pkey" PRIMARY KEY("check_id","region","scheduled_at","kind"),
	CONSTRAINT "uptime_result_kind_check" CHECK ("kind" in ('routine', 'confirm', 'recheck')),
	CONSTRAINT "uptime_result_verdict_check" CHECK ("verdict" in ('pass', 'fail', 'inconclusive')),
	CONSTRAINT "uptime_result_phase_check" CHECK ("failed_phase" in ('none', 'dns', 'connect', 'tls', 'request'))
);
--> statement-breakpoint
CREATE TABLE "uptime_result_hourly" (
	"check_id" uuid,
	"organization_id" uuid NOT NULL,
	"region" text,
	"hour" timestamp with time zone,
	"probes" integer NOT NULL,
	"failures" integer NOT NULL,
	"inconclusive" integer NOT NULL,
	"pass_ms_sum" bigint NOT NULL,
	"pass_ms_max" integer NOT NULL,
	"pass_ms_p50" integer,
	"pass_ms_p95" integer,
	CONSTRAINT "uptime_result_hourly_pkey" PRIMARY KEY("check_id","region","hour")
);
--> statement-breakpoint
CREATE UNIQUE INDEX "uptime_alert_transition_idx" ON "uptime_alert" ("incident_id","kind");--> statement-breakpoint
CREATE INDEX "uptime_alert_pending_idx" ON "uptime_alert" ("created_at") WHERE "state" in ('pending', 'sending');--> statement-breakpoint
CREATE INDEX "uptime_check_organization_idx" ON "uptime_check" ("organization_id");--> statement-breakpoint
CREATE UNIQUE INDEX "uptime_check_id_organization_idx" ON "uptime_check" ("id","organization_id");--> statement-breakpoint
CREATE UNIQUE INDEX "uptime_check_service_idx" ON "uptime_check" ("environment_id","service_id");--> statement-breakpoint
CREATE UNIQUE INDEX "uptime_incident_one_open_idx" ON "uptime_incident" ("check_id") WHERE "closed_at" is null;--> statement-breakpoint
CREATE INDEX "uptime_incident_history_idx" ON "uptime_incident" ("check_id","opened_at" DESC NULLS LAST);--> statement-breakpoint
CREATE INDEX "uptime_incident_closed_idx" ON "uptime_incident" ("closed_at") WHERE "closed_at" is not null;--> statement-breakpoint
CREATE INDEX "uptime_result_scheduled_brin" ON "uptime_result" USING brin ("scheduled_at");--> statement-breakpoint
CREATE INDEX "uptime_result_hourly_brin" ON "uptime_result_hourly" USING brin ("hour");--> statement-breakpoint
ALTER TABLE "uptime_alert" ADD CONSTRAINT "uptime_alert_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "uptime_alert" ADD CONSTRAINT "uptime_alert_incident_id_uptime_incident_id_fkey" FOREIGN KEY ("incident_id") REFERENCES "uptime_incident"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "uptime_check" ADD CONSTRAINT "uptime_check_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "uptime_incident" ADD CONSTRAINT "uptime_incident_check_fk" FOREIGN KEY ("check_id","organization_id") REFERENCES "uptime_check"("id","organization_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "uptime_result" ADD CONSTRAINT "uptime_result_check_fk" FOREIGN KEY ("check_id","organization_id") REFERENCES "uptime_check"("id","organization_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD CONSTRAINT "uptime_result_hourly_check_fk" FOREIGN KEY ("check_id","organization_id") REFERENCES "uptime_check"("id","organization_id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('uptime_check', 'organization_id', 'id');--> statement-breakpoint
SELECT organization_change_attach('uptime_incident', 'organization_id', 'check_id');--> statement-breakpoint
-- The Config Store creates config_applied when it opens, after these migrations, and attaches this then.
CREATE FUNCTION uptime_check_forget_services() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM uptime_check USING old_rows
  WHERE old_rows.node_type = 'service'
    AND uptime_check.environment_id = old_rows.environment_id
    AND uptime_check.service_id = old_rows.node_id;
  RETURN NULL;
END
$$;
