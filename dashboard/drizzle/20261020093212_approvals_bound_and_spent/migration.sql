ALTER TABLE "operation_approvals" ADD COLUMN "used_at" timestamp with time zone;--> statement-breakpoint
-- An approval given before approvals ran once may already have run, so each one asks again.
UPDATE "operation_approvals" SET "used_at" = "decided_at" WHERE "status" = 'approved';--> statement-breakpoint
DROP INDEX "operation_approvals_one_pending_idx";--> statement-breakpoint
CREATE UNIQUE INDEX "operation_approvals_one_pending_idx" ON "operation_approvals" ("organization_id","digest","command","credential_kind","credential_id") WHERE "status" = 'pending';