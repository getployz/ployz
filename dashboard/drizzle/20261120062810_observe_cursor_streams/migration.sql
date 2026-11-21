-- Request rows now reach ClickHouse through the Collector, and crash events reach Cloud through the OTLP route,
-- so a Machine's only signed contact left is its salts fetch.
DELETE FROM "observe_ingest_cursor" WHERE "stream" IN ('crash', 'requests');
--> statement-breakpoint
ALTER TABLE "observe_ingest_cursor" DROP CONSTRAINT "observe_ingest_cursor_stream_check", ADD CONSTRAINT "observe_ingest_cursor_stream_check" CHECK ("stream" in ('salts'));
