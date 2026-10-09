ALTER TABLE "agent_interrupts" ADD COLUMN IF NOT EXISTS "claim" text;--> statement-breakpoint
ALTER TABLE "agent_interrupts" ADD COLUMN IF NOT EXISTS "claimed_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "agent_interrupts" DROP COLUMN IF EXISTS "claimed_by_run_id";
