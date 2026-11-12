ALTER TABLE "machine_remove_attempt" ADD COLUMN "teardown" jsonb;--> statement-breakpoint
ALTER TABLE "namespace_cleanup" ADD COLUMN "teardown" jsonb;