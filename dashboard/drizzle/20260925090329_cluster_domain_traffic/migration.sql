ALTER TABLE "organization_cluster_domain" DROP CONSTRAINT "organization_cluster_domain_record_addresses_check";--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" ADD COLUMN "traffic" jsonb;--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" ADD COLUMN "checked_at" timestamp with time zone;--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" DROP COLUMN "record_addresses";--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" DROP COLUMN "unreachable";--> statement-breakpoint
ALTER TABLE "organization_cluster_domain" ADD CONSTRAINT "organization_cluster_domain_traffic_check" CHECK ("traffic" is null or "traffic"->>'kind' in ('no_servers', 'no_public_ip', 'probed'));