CREATE SCHEMA "analytics";
--> statement-breakpoint
CREATE TABLE "analytics"."salt" (
	"organization_id" text,
	"environment_id" text,
	"day" date,
	"salt" bytea NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "salt_pkey" PRIMARY KEY("organization_id","environment_id","day")
);
--> statement-breakpoint
CREATE TABLE "analytics"."site" (
	"organization_id" text,
	"environment_id" text,
	"enabled" boolean DEFAULT false NOT NULL,
	"timezone" text DEFAULT 'UTC' NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "site_pkey" PRIMARY KEY("organization_id","environment_id")
);
