DROP INDEX "crash_event_machine_seq_idx";--> statement-breakpoint
ALTER TABLE "crash_event" ADD COLUMN "event_id" text;--> statement-breakpoint
UPDATE "crash_event" SET "event_id" = "id"::text;--> statement-breakpoint
ALTER TABLE "crash_event" ALTER COLUMN "event_id" SET NOT NULL;--> statement-breakpoint
ALTER TABLE "crash_event" DROP COLUMN "seq";--> statement-breakpoint
CREATE UNIQUE INDEX "crash_event_machine_event_idx" ON "crash_event" ("organization_id","machine_id","event_id");--> statement-breakpoint
DELETE FROM "observe_ingest_cursor" WHERE "stream" = 'crash';--> statement-breakpoint
ALTER TABLE "observe_ingest_cursor" DROP CONSTRAINT "observe_ingest_cursor_stream_check", ADD CONSTRAINT "observe_ingest_cursor_stream_check" CHECK ("stream" in ('requests'));
