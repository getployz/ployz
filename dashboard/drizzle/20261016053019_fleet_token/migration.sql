CREATE TABLE "fleet_token" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"name" text NOT NULL,
	"token_hash" text NOT NULL,
	"expires_at" timestamp with time zone NOT NULL,
	"auto_enroll_max" integer,
	"created_by_user_id" uuid NOT NULL,
	"last_used_at" timestamp with time zone,
	"revoked_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX "fleet_token_hash_idx" ON "fleet_token" ("token_hash");--> statement-breakpoint
CREATE UNIQUE INDEX "fleet_token_name_idx" ON "fleet_token" ("organization_id","name") WHERE revoked_at is null;--> statement-breakpoint
ALTER TABLE "fleet_token" ADD CONSTRAINT "fleet_token_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "fleet_token" ADD CONSTRAINT "fleet_token_created_by_user_id_user_id_fkey" FOREIGN KEY ("created_by_user_id") REFERENCES "user"("id") ON DELETE RESTRICT;--> statement-breakpoint
SELECT organization_change_attach('fleet_token', 'organization_id', 'id');
