-- No path ever accepted an invitation, so existing rows have no token and cannot be used.
DELETE FROM "invitation";--> statement-breakpoint
ALTER TABLE "invitation" ADD COLUMN "token_hash" text NOT NULL;--> statement-breakpoint
ALTER TABLE "invitation" ADD COLUMN "accepted_by_user_id" uuid;--> statement-breakpoint
ALTER TABLE "invitation" ALTER COLUMN "email" DROP NOT NULL;--> statement-breakpoint
ALTER TABLE "invitation" ALTER COLUMN "role" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "invitation" ADD CONSTRAINT "invitation_token_hash_key" UNIQUE("token_hash");--> statement-breakpoint
ALTER TABLE "invitation" ADD CONSTRAINT "invitation_accepted_by_user_id_user_id_fkey" FOREIGN KEY ("accepted_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;