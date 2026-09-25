CREATE TABLE "environment_deployment_image_build" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"deployment_id" uuid NOT NULL,
	"service_id" text NOT NULL,
	"image" text NOT NULL,
	"status" text DEFAULT 'building' NOT NULL,
	"machine_id" text,
	"server_choice" jsonb,
	"encrypted_receipt" jsonb,
	"failure_message" text,
	"inngest_run_id" text NOT NULL,
	"finished_at" timestamp with time zone,
	"builder" text DEFAULT 'server' NOT NULL,
	"github_run_id" bigint,
	"checked_in_at" timestamp with time zone,
	"github" jsonb,
	"skips" jsonb DEFAULT '[]' NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_deployment_image_build_service_unique" UNIQUE("deployment_id","service_id"),
	CONSTRAINT "environment_deployment_image_build_receipt_check" CHECK (("status" = 'built') = ("encrypted_receipt" is not null)),
	CONSTRAINT "environment_deployment_image_build_builder_check" CHECK ("builder" in ('server', 'github')),
	CONSTRAINT "environment_deployment_image_build_github_check" CHECK (("builder" = 'github') = ("github" is not null and "github_run_id" is not null)),
	CONSTRAINT "environment_deployment_image_build_check_in_check" CHECK ("checked_in_at" is null or "builder" = 'github')
);
--> statement-breakpoint
CREATE TABLE "organization_build_order" (
	"organization_id" uuid PRIMARY KEY,
	"build_order" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "organization_build_order_check" CHECK ("build_order" in ('servers-only', 'github-then-servers', 'servers-then-github', 'github-only'))
);
--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" ADD COLUMN "image" text;--> statement-breakpoint
ALTER TABLE "environment_deployment_secret" DROP COLUMN "encrypted_build_receipts";--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" DROP CONSTRAINT "environment_deployment_build_step_key_unique";--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" ADD CONSTRAINT "environment_deployment_build_step_key_unique" UNIQUE NULLS NOT DISTINCT("deployment_id","image","build","key");--> statement-breakpoint
ALTER TABLE "environment_deployment_image_build" ADD CONSTRAINT "environment_deployment_image_build_omhRa2gD2UgI_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_image_build" ADD CONSTRAINT "environment_deployment_image_build_Ss4XYtuD2fFu_fkey" FOREIGN KEY ("deployment_id") REFERENCES "environment_deployment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "organization_build_order" ADD CONSTRAINT "organization_build_order_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
SELECT organization_change_attach('environment_deployment_image_build', 'organization_id', 'id');--> statement-breakpoint
SELECT organization_change_attach('organization_build_order', 'organization_id', 'organization_id');
