DROP TABLE "user_project_preference";--> statement-breakpoint
ALTER TABLE "project" ADD COLUMN "default_environment_id" uuid;--> statement-breakpoint
ALTER TABLE "project" ADD CONSTRAINT "project_default_environment_id_environment_id_fkey" FOREIGN KEY ("default_environment_id") REFERENCES "environment"("id") ON DELETE SET NULL;--> statement-breakpoint
UPDATE "project" SET "default_environment_id" = (SELECT "id" FROM "environment" WHERE "environment"."project_id" = "project"."id" ORDER BY "created_at" LIMIT 1);
