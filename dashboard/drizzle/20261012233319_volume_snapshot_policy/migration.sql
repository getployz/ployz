CREATE TABLE "volume_snapshot_policy" (
	"volume_id" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"policy" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX "volume_snapshot_policy_environment_idx" ON "volume_snapshot_policy" ("organization_id","environment_id");--> statement-breakpoint
ALTER TABLE "volume_snapshot_policy" ADD CONSTRAINT "volume_snapshot_policy_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;