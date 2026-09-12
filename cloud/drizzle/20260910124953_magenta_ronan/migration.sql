CREATE TABLE "environment_deployment_event" (
	"id" bigserial PRIMARY KEY,
	"deployment_id" uuid NOT NULL,
	"progress" jsonb NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX "environment_deployment_event_cursor_idx" ON "environment_deployment_event" ("deployment_id","id");--> statement-breakpoint
ALTER TABLE "environment_deployment_event" ADD CONSTRAINT "environment_deployment_event_nkhfxU66kCFO_fkey" FOREIGN KEY ("deployment_id") REFERENCES "environment_deployment"("id") ON DELETE CASCADE;