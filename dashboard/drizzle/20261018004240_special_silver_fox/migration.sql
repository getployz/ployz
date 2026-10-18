CREATE TABLE "fleet_token_join" (
	"attempt_id" uuid PRIMARY KEY,
	"fleet_token_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX "fleet_token_join_token_idx" ON "fleet_token_join" ("fleet_token_id");--> statement-breakpoint
ALTER TABLE "fleet_token_join" ADD CONSTRAINT "fleet_token_join_fleet_token_id_fleet_token_id_fkey" FOREIGN KEY ("fleet_token_id") REFERENCES "fleet_token"("id") ON DELETE CASCADE;--> statement-breakpoint
INSERT INTO "fleet_token_join" ("attempt_id", "fleet_token_id", "created_at")
SELECT e."id", e."fleet_token_id", e."ended_at"
FROM "server_enrollment" e INNER JOIN "fleet_token" t ON t."id" = e."fleet_token_id"
WHERE e."state" = 'joined';

--> statement-breakpoint
CREATE INDEX "server_enrollment_token_open_idx" ON "server_enrollment" ("fleet_token_id") WHERE ended_at is null;