CREATE TABLE "analytics"."request" (
	"environment_id" text,
	"ts" timestamp with time zone,
	"request_id" text,
	"service" text NOT NULL,
	"host" text NOT NULL,
	"method" text NOT NULL,
	"path" text NOT NULL,
	"status" smallint NOT NULL,
	"duration_ms" double precision NOT NULL,
	"bytes_in" bigint NOT NULL,
	"bytes_out" bigint NOT NULL,
	"visitor_id" bigint,
	"kind" text NOT NULL,
	"bot_reason" text,
	"source" text NOT NULL,
	"referrer_host" text,
	"language" text,
	"utm_source" text,
	"utm_medium" text,
	"utm_campaign" text,
	"utm_term" text,
	"utm_content" text,
	CONSTRAINT "request_pkey" PRIMARY KEY("environment_id","ts","request_id"),
	CONSTRAINT "request_kind_check" CHECK ("kind" in ('pageview', 'bot', 'other'))
) PARTITION BY RANGE ("ts");
--> statement-breakpoint
DROP TABLE "analytics"."bot_daily";--> statement-breakpoint
DROP TABLE "analytics"."daily";--> statement-breakpoint
DROP TABLE "analytics"."gap_daily";--> statement-breakpoint
DROP TABLE "analytics"."pageview";--> statement-breakpoint
DROP TABLE "analytics"."rollup_state";--> statement-breakpoint
DROP TABLE "analytics"."session_hourly";--> statement-breakpoint
-- Request rows are left to the partition drops.
CREATE OR REPLACE FUNCTION "analytics"."forget_environments"() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM analytics.site USING old_rows WHERE site.environment_id = old_rows.id;
  DELETE FROM analytics.goal USING old_rows WHERE goal.environment_id = old_rows.id;
  DELETE FROM analytics.salt USING old_rows WHERE salt.environment_id = old_rows.id;
  RETURN NULL;
END
$$;
