CREATE TABLE "crash_event" (
	"id" uuid PRIMARY KEY DEFAULT gen_random_uuid(),
	"organization_id" uuid NOT NULL,
	"machine_id" text NOT NULL,
	"seq" bigint NOT NULL,
	"service_id" text NOT NULL,
	"service_name" text NOT NULL,
	"replica" text NOT NULL,
	"container_id" text NOT NULL,
	"deployment_id" text,
	"exit_code" bigint,
	"oom" boolean NOT NULL,
	"occurred_at" timestamp with time zone NOT NULL,
	"received_at" timestamp with time zone DEFAULT now() NOT NULL,
	"mailed_at" timestamp with time zone
);
--> statement-breakpoint
CREATE TABLE "observe_ingest_cursor" (
	"organization_id" uuid,
	"machine_id" text,
	"stream" text,
	"seq" bigint NOT NULL,
	"updated_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "observe_ingest_cursor_pkey" PRIMARY KEY("organization_id","machine_id","stream"),
	CONSTRAINT "observe_ingest_cursor_stream_check" CHECK ("stream" in ('crash'))
);
--> statement-breakpoint
CREATE UNIQUE INDEX "crash_event_machine_seq_idx" ON "crash_event" ("organization_id","machine_id","seq");--> statement-breakpoint
CREATE INDEX "crash_event_service_idx" ON "crash_event" ("organization_id","service_id","mailed_at");--> statement-breakpoint
CREATE INDEX "crash_event_occurred_idx" ON "crash_event" ("occurred_at");--> statement-breakpoint
ALTER TABLE "crash_event" ADD CONSTRAINT "crash_event_machine_fk" FOREIGN KEY ("organization_id","machine_id") REFERENCES "organization_machine"("organization_id","machine_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "observe_ingest_cursor" ADD CONSTRAINT "observe_ingest_cursor_machine_fk" FOREIGN KEY ("organization_id","machine_id") REFERENCES "organization_machine"("organization_id","machine_id") ON DELETE CASCADE;--> statement-breakpoint
ALTER TABLE "notification_preference" DROP CONSTRAINT "notification_preference_type_check", ADD CONSTRAINT "notification_preference_type_check" CHECK ("type" in ('uptime','deploy-failed','upgrade-failed','crash'));