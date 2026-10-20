CREATE TABLE "fleet_token_attempt" (
	"organization_id" uuid NOT NULL,
	"fleet_token_id" uuid NOT NULL,
	"checkin_key" text NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "fleet_token_attempt_pkey" PRIMARY KEY("fleet_token_id","checkin_key")
);
--> statement-breakpoint
ALTER TABLE "fleet_token_attempt" ADD CONSTRAINT "fleet_token_attempt_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
-- Every attempt records itself, whichever app version inserts it, so an app older than this table can't start an unrecorded one.
CREATE FUNCTION fleet_token_attempt_record() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO fleet_token_attempt (organization_id, fleet_token_id, checkin_key, created_at)
  SELECT NEW.organization_id, NEW.fleet_token_id, w.checkin_key, NEW.requested_at
  FROM waiting_server w WHERE w.id = NEW.waiting_server_id
  ON CONFLICT DO NOTHING;
  RETURN NULL;
END
$$;
--> statement-breakpoint
-- Created before the backfill: its lock waits out every open insert, so each attempt is either backfilled or recorded here.
CREATE TRIGGER fleet_token_attempt_record AFTER INSERT ON "server_enrollment" FOR EACH ROW EXECUTE FUNCTION fleet_token_attempt_record();--> statement-breakpoint
INSERT INTO "fleet_token_attempt" ("organization_id", "fleet_token_id", "checkin_key", "created_at")
SELECT e."organization_id", e."fleet_token_id", w."checkin_key", min(e."requested_at")
FROM "server_enrollment" e INNER JOIN "waiting_server" w ON w."id" = e."waiting_server_id"
GROUP BY e."organization_id", e."fleet_token_id", w."checkin_key"
ON CONFLICT DO NOTHING;
