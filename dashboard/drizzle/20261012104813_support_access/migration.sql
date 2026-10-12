CREATE TABLE "organization_support_access" (
	"organization_id" uuid PRIMARY KEY,
	"allowed" boolean DEFAULT false NOT NULL,
	"changed_by_user_id" uuid,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
ALTER TABLE "organization_support_access" ADD CONSTRAINT "organization_support_access_kLfPQs8MyHZC_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_support_access" ADD CONSTRAINT "organization_support_access_changed_by_user_id_user_id_fkey" FOREIGN KEY ("changed_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
SELECT organization_change_attach('organization_support_access', 'organization_id', 'organization_id');