CREATE TABLE "environment_deployment_build_output" (
	"id" bigserial PRIMARY KEY,
	"deployment_id" uuid NOT NULL,
	"step_id" bigint NOT NULL,
	"stderr" boolean DEFAULT false NOT NULL,
	"text" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "environment_deployment_build_step" (
	"id" bigserial PRIMARY KEY,
	"deployment_id" uuid NOT NULL,
	"key" text NOT NULL,
	"name" text NOT NULL,
	"started_at" timestamp with time zone,
	"completed_at" timestamp with time zone,
	"cached" boolean DEFAULT false NOT NULL,
	"error" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "environment_deployment_build_step_key_unique" UNIQUE("deployment_id","key")
);
--> statement-breakpoint
CREATE INDEX "environment_deployment_build_output_cursor_idx" ON "environment_deployment_build_output" ("deployment_id","id");--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" ADD CONSTRAINT "environment_deployment_build_output_Xp68IrJouNSa_fkey" FOREIGN KEY ("deployment_id") REFERENCES "environment_deployment"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_output" ADD CONSTRAINT "environment_deployment_build_output_XF27nu5BhEx1_fkey" FOREIGN KEY ("step_id") REFERENCES "environment_deployment_build_step"("id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "environment_deployment_build_step" ADD CONSTRAINT "environment_deployment_build_step_mAOz3YRgkCxT_fkey" FOREIGN KEY ("deployment_id") REFERENCES "environment_deployment"("id") ON DELETE CASCADE;