ALTER TABLE "github_installation" ADD COLUMN "repositories_synced_at" timestamp with time zone;--> statement-breakpoint
-- Installations from before this column were synced by earlier runs; only ones linked from now on wait on a first sync.
UPDATE "github_installation" SET "repositories_synced_at" = now();
