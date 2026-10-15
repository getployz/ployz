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
	"visits" bigint NOT NULL,
	"bounces" bigint NOT NULL,
	"total_duration" bigint NOT NULL,
	CONSTRAINT "session_hourly_pkey" PRIMARY KEY("environment_id","hour")
);
--> statement-breakpoint
CREATE UNIQUE INDEX "goal_environment_path_idx" ON "analytics"."goal" ("environment_id","path_pattern");