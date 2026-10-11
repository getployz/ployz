ALTER TABLE "uptime_result_hourly" ADD COLUMN "dns_ms_sum" bigint DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "connect_ms_sum" bigint DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "tls_ms_sum" bigint DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "ttfb_ms_sum" bigint DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_status" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_timeout" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_refused" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_dns" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_connect" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_tls" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "failures_other" integer DEFAULT 0 NOT NULL;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "last_error" text;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "last_error_class" text;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD COLUMN "last_error_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" DROP COLUMN "pass_ms_p95";--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD CONSTRAINT "uptime_result_hourly_last_error_check" CHECK (("last_error" is null) = ("last_error_class" is null)
      and ("last_error" is null) = ("last_error_at" is null));--> statement-breakpoint
ALTER TABLE "uptime_result_hourly" ADD CONSTRAINT "uptime_result_hourly_last_error_class_check" CHECK ("last_error_class" in ('status', 'timeout', 'refused', 'dns', 'connect', 'tls', 'other'));