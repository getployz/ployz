ALTER TABLE "conditional_save" ADD COLUMN "merge_commit_sha" text;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD COLUMN "landed_saved_state_id" uuid;--> statement-breakpoint
ALTER TABLE "conditional_save" ADD CONSTRAINT "conditional_save_landed_frozen_check" CHECK ("landed_saved_state_id" is null or "merge_commit_sha" is not null);