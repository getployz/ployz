CREATE TABLE "offsite_bucket" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"project_id" text NOT NULL,
	"endpoint" text NOT NULL,
	"region" text NOT NULL,
	"bucket" text NOT NULL,
	"prefix" text NOT NULL,
	"access_key" text NOT NULL,
	"encrypted_secret_key" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "volume_offsite" (
	"volume_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"bucket_id" uuid NOT NULL,
	"keep" integer DEFAULT 7 NOT NULL,
	"uploaded_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX "offsite_bucket_place_idx" ON "offsite_bucket" ("organization_id","project_id","endpoint","bucket","prefix");--> statement-breakpoint
CREATE INDEX "volume_offsite_environment_idx" ON "volume_offsite" ("organization_id","environment_id");--> statement-breakpoint
ALTER TABLE "offsite_bucket" ADD CONSTRAINT "offsite_bucket_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "volume_offsite" ADD CONSTRAINT "volume_offsite_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "volume_offsite" ADD CONSTRAINT "volume_offsite_bucket_id_offsite_bucket_id_fkey" FOREIGN KEY ("bucket_id") REFERENCES "offsite_bucket"("id") ON DELETE RESTRICT;