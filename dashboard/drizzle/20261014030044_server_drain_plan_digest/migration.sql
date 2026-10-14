ALTER TABLE "server_drain_attempt" ADD COLUMN "plan_digest" text;--> statement-breakpoint
-- A Drain admitted before this ran under the approval it consumed; that approval names the plan it covered.
UPDATE "server_drain_attempt" AS "drain" SET "plan_digest" = "approval"."digest" FROM "operation_approvals" AS "approval" WHERE "drain"."approval_id" = "approval"."id" AND "approval"."status" = 'approved';
