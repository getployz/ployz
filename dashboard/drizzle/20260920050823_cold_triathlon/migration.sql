ALTER TABLE "github_environment_trigger" ADD COLUMN "admission_state" text DEFAULT 'waiting' NOT NULL;--> statement-breakpoint
ALTER TABLE "github_environment_trigger" ADD COLUMN "changed_paths" jsonb DEFAULT '[]' NOT NULL;
--> statement-breakpoint
UPDATE github_environment_trigger SET admission_state = 'admitted';
