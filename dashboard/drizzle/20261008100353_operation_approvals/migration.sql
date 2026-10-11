CREATE TABLE "operation_approvals" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"requested_by_user_id" uuid,
	"credential_kind" text NOT NULL,
	"credential_id" text NOT NULL,
	"command" text NOT NULL,
	"review" jsonb NOT NULL,
	"digest" text NOT NULL,
	"status" text DEFAULT 'pending' NOT NULL,
	"reason" text,
	"decided_by_user_id" uuid,
	"decided_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "operation_approvals_status_check" CHECK ("status" in ('pending','approved','denied','superseded')),
	CONSTRAINT "operation_approvals_credential_kind_check" CHECK ("credential_kind" in ('session', 'token')),
	CONSTRAINT "operation_approvals_reason_check" CHECK ("reason" is null or "status" = 'denied'),
	CONSTRAINT "operation_approvals_decided_check" CHECK (("status" in ('approved', 'denied')) = ("decided_at" is not null))
);
--> statement-breakpoint
CREATE TABLE "organization_settings" (
	"organization_id" uuid PRIMARY KEY,
	"ask_before_destructive" boolean DEFAULT true NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE UNIQUE INDEX "operation_approvals_one_pending_idx" ON "operation_approvals" ("organization_id","digest") WHERE "status" = 'pending';--> statement-breakpoint
CREATE INDEX "operation_approvals_environment_idx" ON "operation_approvals" ("organization_id","environment_id","status");--> statement-breakpoint
ALTER TABLE "operation_approvals" ADD CONSTRAINT "operation_approvals_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "operation_approvals" ADD CONSTRAINT "operation_approvals_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "operation_approvals" ADD CONSTRAINT "operation_approvals_decided_by_user_id_user_id_fkey" FOREIGN KEY ("decided_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "organization_settings" ADD CONSTRAINT "organization_settings_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('organization_settings', 'organization_id', 'organization_id');--> statement-breakpoint
SELECT organization_change_attach('operation_approvals', 'organization_id', 'id');
