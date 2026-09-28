ALTER TABLE "conditional_save" RENAME COLUMN "approved_by_user_id" TO "saved_by_user_id";--> statement-breakpoint
ALTER TABLE "conditional_save" RENAME COLUMN "approved_at" TO "saved_at";--> statement-breakpoint
ALTER TABLE "conditional_save" RENAME CONSTRAINT "conditional_save_approved_by_user_id_user_id_fkey" TO "conditional_save_saved_by_user_id_user_id_fkey";