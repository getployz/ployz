CREATE TABLE "analytics_goal" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"environment_id" text NOT NULL,
	"name" text NOT NULL,
	"path_pattern" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "analytics_salt" (
	"organization_id" text,
	"environment_id" text,
	"day" date,
	"salt" bytea NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "analytics_salt_pkey" PRIMARY KEY("organization_id","environment_id","day")
);
--> statement-breakpoint
CREATE TABLE "analytics_site" (
	"organization_id" text,
	"environment_id" text,
	"enabled" boolean DEFAULT false NOT NULL,
	"timezone" text DEFAULT 'UTC' NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "analytics_site_pkey" PRIMARY KEY("organization_id","environment_id")
);
--> statement-breakpoint
CREATE UNIQUE INDEX "analytics_goal_environment_path_idx" ON "analytics_goal" ("environment_id","path_pattern");--> statement-breakpoint
-- The analytics schema held these three tables and the request table that now lives in ClickHouse. Fresh installs never had it.
DO $$
BEGIN
  IF to_regclass('analytics.site') IS NOT NULL THEN
    INSERT INTO "analytics_site" (organization_id, environment_id, enabled, timezone, updated_at)
      SELECT organization_id, environment_id, enabled, timezone, updated_at FROM analytics.site;
    INSERT INTO "analytics_salt" (organization_id, environment_id, day, salt, created_at)
      SELECT organization_id, environment_id, day, salt, created_at FROM analytics.salt;
    INSERT INTO "analytics_goal" (id, environment_id, name, path_pattern, created_at)
      SELECT id, environment_id, name, path_pattern, created_at FROM analytics.goal;
  END IF;
END
$$;
--> statement-breakpoint
-- Dropping the schema also drops the trigger on config_environment that ran analytics.forget_environments.
DROP SCHEMA IF EXISTS "analytics" CASCADE;
--> statement-breakpoint
DROP TABLE IF EXISTS "drizzle"."__drizzle_migrations_analytics";
--> statement-breakpoint
-- Request rows are not kept in Postgres, so an Environment's delete forgets only its config.
CREATE FUNCTION "analytics_forget_environments"() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  DELETE FROM analytics_site USING old_rows WHERE analytics_site.environment_id = old_rows.id;
  DELETE FROM analytics_goal USING old_rows WHERE analytics_goal.environment_id = old_rows.id;
  DELETE FROM analytics_salt USING old_rows WHERE analytics_salt.environment_id = old_rows.id;
  RETURN NULL;
END
$$;
--> statement-breakpoint
-- The Config Store creates config_environment when it opens, and attaches the trigger then on a fresh install.
-- An install that already has the table keeps forgetting without a gap.
DO $$
BEGIN
  IF to_regclass('config_environment') IS NOT NULL THEN
    CREATE TRIGGER analytics_forget_environments AFTER DELETE ON config_environment
      REFERENCING OLD TABLE AS old_rows FOR EACH STATEMENT EXECUTE FUNCTION analytics_forget_environments();
  END IF;
END
$$;
