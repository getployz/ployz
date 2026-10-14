ALTER TABLE "volume_run" ADD COLUMN "held_by" text;--> statement-breakpoint
CREATE INDEX "volume_run_held_idx" ON "volume_run" ("volume_id") WHERE "held_by" is not null;