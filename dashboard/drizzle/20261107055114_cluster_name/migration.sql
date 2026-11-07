CREATE TABLE "organization_cluster_name" (
	"organization_id" uuid PRIMARY KEY,
	"name" text NOT NULL,
	"applied_name" text,
	"refusal" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_cluster_name_name_check" CHECK ("name" ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$')
);
--> statement-breakpoint
ALTER TABLE "organization_cluster_name" ADD CONSTRAINT "organization_cluster_name_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('organization_cluster_name', 'organization_id', 'organization_id');
