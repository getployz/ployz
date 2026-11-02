CREATE TABLE "sealing_key_owner" (
	"public_key" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "sealing_key_owner_public_key_check" CHECK ("public_key" ~ '^[A-Za-z0-9+/]{43}=$')
);
--> statement-breakpoint
ALTER TABLE "sealing_key_owner" ADD CONSTRAINT "sealing_key_owner_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
-- The earliest record of each key owns it. A key already recorded by more than one Organization is reported, not removed.
INSERT INTO "sealing_key_owner" ("public_key", "organization_id", "created_at")
SELECT DISTINCT ON ("public_key") "public_key", "organization_id", "created_at"
FROM "cluster_sealing_key"
ORDER BY "public_key", "created_at", "organization_id"
ON CONFLICT ("public_key") DO NOTHING;
--> statement-breakpoint
DO $$
DECLARE shared integer;
BEGIN
  SELECT count(*) INTO shared FROM (
    SELECT "public_key" FROM "cluster_sealing_key" GROUP BY "public_key" HAVING count(DISTINCT "organization_id") > 1
  ) AS keys;
  IF shared > 0 THEN
    RAISE WARNING 'sealing_key_owner: % Sealing public key(s) are recorded for more than one Organization; the earliest keeps each', shared;
  END IF;
END $$;
