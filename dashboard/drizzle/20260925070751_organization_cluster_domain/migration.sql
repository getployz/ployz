CREATE TABLE "organization_cluster_domain" (
	"organization_id" uuid PRIMARY KEY,
	"endpoint" text NOT NULL,
	"name" text NOT NULL,
	"encrypted_token" jsonb NOT NULL,
	"reserved_at" timestamp with time zone NOT NULL,
	"lease_renewed_at" timestamp with time zone NOT NULL,
	"records_synced_at" timestamp with time zone,
	"record_addresses" jsonb DEFAULT '[]' NOT NULL,
	"unreachable" jsonb DEFAULT '[]' NOT NULL,
	"encrypted_certificate_private_key" jsonb,
	"certificate_chain" text,
	"certificate_not_after" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_cluster_domain_record_addresses_check" CHECK (jsonb_typeof("record_addresses") = 'array'),
	CONSTRAINT "organization_cluster_domain_certificate_check" CHECK (num_nulls("encrypted_certificate_private_key", "certificate_chain", "certificate_not_after") in (0, 3))
);
--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" ADD CONSTRAINT "organization_cluster_domain_eJqKfNjJBhUj_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('organization_cluster_domain', 'organization_id', 'organization_id');
