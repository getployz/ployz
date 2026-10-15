ALTER TABLE "machine_enrollment_token" ADD COLUMN "claimed_machine_id" text;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD COLUMN "claimed_public_key" text;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD COLUMN "claimed_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "machine_enrollment_token" ADD CONSTRAINT "machine_enrollment_token_claim_check" CHECK (("claimed_machine_id" is null) = ("claimed_public_key" is null)
      and ("claimed_machine_id" is null) = ("claimed_at" is null));