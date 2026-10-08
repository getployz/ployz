ALTER TABLE "operation_approvals" RENAME COLUMN "environment_id" TO "subject";--> statement-breakpoint
ALTER INDEX "operation_approvals_environment_idx" RENAME TO "operation_approvals_subject_idx";
