CREATE TABLE "organization_server_upgrades" (
	"organization_id" uuid PRIMARY KEY,
	"automatic" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "organization_server_upgrades" ADD CONSTRAINT "organization_server_upgrades_eLbQmpv5tk3t_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('organization_server_upgrades', 'organization_id', 'organization_id');
