CREATE TABLE "volume_placement" (
	"volume_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"mirrors" jsonb,
	"staged" jsonb,
	"deployment_id" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX "volume_placement_environment_idx" ON "volume_placement" ("organization_id","environment_id");--> statement-breakpoint
ALTER TABLE "volume_placement" ADD CONSTRAINT "volume_placement_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('volume_placement', 'organization_id', 'volume_id');
