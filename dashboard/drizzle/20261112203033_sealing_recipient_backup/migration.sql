ALTER TABLE "organization_sealing_key" ADD COLUMN "recipient_public_key" text;--> statement-breakpoint
ALTER TABLE "organization_sealing_key" ADD COLUMN "recipient_blob" text;--> statement-breakpoint
ALTER TABLE "organization_sealing_key" ADD COLUMN "recipient_rotation" jsonb;--> statement-breakpoint
ALTER TABLE "organization_sealing_key" ADD CONSTRAINT "organization_sealing_key_recipient_public_key_check" CHECK ("recipient_public_key" ~ '^[A-Za-z0-9+/]{43}=$');--> statement-breakpoint
ALTER TABLE "organization_sealing_key" ADD CONSTRAINT "organization_sealing_key_recipient_blob_check" CHECK ("recipient_blob" ~ '^[A-Za-z0-9+/]{103}=$');--> statement-breakpoint
ALTER TABLE "organization_sealing_key" ADD CONSTRAINT "organization_sealing_key_recipient_whole_check" CHECK (("recipient_public_key" IS NULL) = ("recipient_blob" IS NULL) AND ("recipient_blob" IS NULL) = ("recipient_rotation" IS NULL));