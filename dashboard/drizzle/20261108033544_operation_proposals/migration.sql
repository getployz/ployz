CREATE TABLE "operation_proposals" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"environment_id" text NOT NULL,
	"project" text NOT NULL,
	"environment" text NOT NULL,
	"namespace" text NOT NULL,
	"requested_by_user_id" uuid,
	"credential_kind" text NOT NULL,
	"credential_id" text NOT NULL,
	"steps" jsonb DEFAULT '[]' NOT NULL,
	"status" text DEFAULT 'draft' NOT NULL,
	"bundle" jsonb,
	"approvals" jsonb,
	"halt" jsonb,
	"reason" text,
	"decided_by_user_id" uuid,
	"decided_at" timestamp with time zone,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "operation_proposals_status_check" CHECK ("status" in ('draft','presented','approved','denied')),
	CONSTRAINT "operation_proposals_steps_check" CHECK (jsonb_typeof("steps") = 'array'),
	CONSTRAINT "operation_proposals_reason_check" CHECK ("reason" is null or length("reason") between 1 and 1024),
	CONSTRAINT "operation_proposals_decided_check" CHECK (("status" in ('approved', 'denied')) = ("decided_at" is not null))
);
--> statement-breakpoint
ALTER TABLE "service_act" ADD COLUMN "proposal_id" uuid;--> statement-breakpoint
ALTER TABLE "service_act" ADD COLUMN "proposal_step" integer;--> statement-breakpoint
CREATE INDEX "operation_proposals_waiting_idx" ON "operation_proposals" ("organization_id","status","created_at" DESC NULLS LAST);--> statement-breakpoint
ALTER TABLE "operation_proposals" ADD CONSTRAINT "operation_proposals_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "operation_proposals" ADD CONSTRAINT "operation_proposals_requested_by_user_id_user_id_fkey" FOREIGN KEY ("requested_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "operation_proposals" ADD CONSTRAINT "operation_proposals_decided_by_user_id_user_id_fkey" FOREIGN KEY ("decided_by_user_id") REFERENCES "user"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "service_act" ADD CONSTRAINT "service_act_proposal_id_operation_proposals_id_fkey" FOREIGN KEY ("proposal_id") REFERENCES "operation_proposals"("id") ON DELETE SET NULL;--> statement-breakpoint
ALTER TABLE "service_act" ADD CONSTRAINT "service_act_proposal_check" CHECK (("proposal_id" is null) = ("proposal_step" is null));