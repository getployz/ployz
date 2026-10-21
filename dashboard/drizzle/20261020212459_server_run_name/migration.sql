ALTER TABLE "machine_remove_attempt" ADD COLUMN "server_name" text;--> statement-breakpoint
ALTER TABLE "server_drain_attempt" ADD COLUMN "server_name" text;--> statement-breakpoint
ALTER TABLE "server_upgrade_attempt" ADD COLUMN "server_name" text;