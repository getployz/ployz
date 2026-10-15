CREATE TABLE "analytics"."bot_daily" (
	"environment_id" text,
	"day" date,
	"reason" text,
	"count" bigint NOT NULL,
	CONSTRAINT "bot_daily_pkey" PRIMARY KEY("environment_id","day","reason")
);
--> statement-breakpoint
CREATE TABLE "analytics"."daily" (
	"environment_id" text,
	"day" date,
	"dim" text,
	"value" text,
	"pageviews" bigint NOT NULL,
	"visitors" bigint NOT NULL,
	"visits" bigint NOT NULL,
	"bounces" bigint NOT NULL,
	"total_duration" bigint NOT NULL,
	CONSTRAINT "daily_pkey" PRIMARY KEY("environment_id","day","dim","value")
);
--> statement-breakpoint
CREATE TABLE "analytics"."gap_daily" (
	"environment_id" text,
	"day" date,
	"count" bigint NOT NULL,
	CONSTRAINT "gap_daily_pkey" PRIMARY KEY("environment_id","day")
);
--> statement-breakpoint
CREATE TABLE "analytics"."goal" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"environment_id" text NOT NULL,
	"name" text NOT NULL,
	"path_pattern" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "analytics"."pageview" (
	"environment_id" text,
	"ts" timestamp with time zone,
	"visitor_id" bigint,
	"host" text,
	"pathname" text,
	"source" text NOT NULL,
	"referrer_host" text,
	"utm_source" text,
	"utm_medium" text,
	"utm_campaign" text,
	"utm_term" text,
	"utm_content" text,
	"ref" text,
	"language" text,
	"flags" smallint DEFAULT 0 NOT NULL,
	CONSTRAINT "pageview_pkey" PRIMARY KEY("environment_id","ts","visitor_id","host","pathname")
) PARTITION BY RANGE ("ts");
--> statement-breakpoint
CREATE TABLE "analytics"."rollup_state" (
	"environment_id" text PRIMARY KEY,
	"rolled_up_to" date,
	"dirty" date[] DEFAULT '{}'::date[] NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "analytics"."session_hourly" (
	"environment_id" text,
	"hour" timestamp with time zone,
	"pageviews" bigint NOT NULL,
	"visitors" bigint NOT NULL,
	"visits" bigint NOT NULL,
	"bounces" bigint NOT NULL,
	"total_duration" bigint NOT NULL,
	CONSTRAINT "session_hourly_pkey" PRIMARY KEY("environment_id","hour")
);
--> statement-breakpoint
CREATE UNIQUE INDEX "goal_environment_path_idx" ON "analytics"."goal" ("environment_id","path_pattern");--> statement-breakpoint
-- The Config Store creates config_environment when it opens, after these migrations, and attaches this then.
-- Raw pageviews are left to the partition drops. Each environment's rollup_state row is made if missing and locked
-- first, one at a time in id order, the way ingest takes them, so deleting several environments waits behind a batch
-- spanning them instead of deadlocking, and an enable racing the delete waits for it rather than slipping a row in.
CREATE FUNCTION "analytics"."forget_environments"() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
  environment text;
BEGIN
  FOR environment IN SELECT DISTINCT id COLLATE "C" FROM old_rows ORDER BY 1 LOOP
    INSERT INTO analytics.rollup_state (environment_id) VALUES (environment) ON CONFLICT DO NOTHING;
    PERFORM 1 FROM analytics.rollup_state WHERE environment_id = environment FOR UPDATE;
  END LOOP;
  DELETE FROM analytics.rollup_state USING old_rows WHERE rollup_state.environment_id = old_rows.id;
  DELETE FROM analytics.site USING old_rows WHERE site.environment_id = old_rows.id;
  DELETE FROM analytics.daily USING old_rows WHERE daily.environment_id = old_rows.id;
  DELETE FROM analytics.session_hourly USING old_rows WHERE session_hourly.environment_id = old_rows.id;
  DELETE FROM analytics.bot_daily USING old_rows WHERE bot_daily.environment_id = old_rows.id;
  DELETE FROM analytics.gap_daily USING old_rows WHERE gap_daily.environment_id = old_rows.id;
  DELETE FROM analytics.goal USING old_rows WHERE goal.environment_id = old_rows.id;
  DELETE FROM analytics.salt USING old_rows WHERE salt.environment_id = old_rows.id;
  RETURN NULL;
END
$$;
