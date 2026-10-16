CREATE SEQUENCE "analytics"."clickhouse_attempt" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1;--> statement-breakpoint
CREATE TABLE "analytics"."clickhouse_site" (
	"environment_id" text PRIMARY KEY,
	"rolled_up_to" date,
	"dirty" date[] DEFAULT '{}'::date[] NOT NULL,
	"reads" boolean DEFAULT false NOT NULL,
	"backfilled_at" timestamp with time zone,
	"copy" bigint DEFAULT 0 NOT NULL,
	"floor" bigint DEFAULT 0 NOT NULL,
	"generation" bigint DEFAULT 0 NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "analytics"."outbox" (
	"id" bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY (sequence name "analytics"."outbox_id_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1),
	"environment_id" text NOT NULL,
	"rows" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
