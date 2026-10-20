CREATE TABLE "fleet_token_attempt" (
	"organization_id" uuid NOT NULL,
	"fleet_token_id" uuid,
	"checkin_key" text,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "fleet_token_attempt_pkey" PRIMARY KEY("fleet_token_id","checkin_key")
);
--> statement-breakpoint
ALTER TABLE "fleet_token_attempt" ADD CONSTRAINT "fleet_token_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
INSERT INTO "fleet_token_attempt" ("organization_id", "fleet_token_id", "checkin_key", "created_at")
SELECT e."organization_id", e."fleet_token_id", w."checkin_key", min(e."requested_at")
FROM "server_enrollment" e INNER JOIN "waiting_server" w ON w."id" = e."waiting_server_id"
GROUP BY e."organization_id", e."fleet_token_id", w."checkin_key";
