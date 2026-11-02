CREATE TABLE "sealing_key_owner" (
	"public_key" text PRIMARY KEY,
	"organization_id" uuid NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL,
	CONSTRAINT "sealing_key_owner_public_key_check" CHECK ("public_key" ~ '^[A-Za-z0-9+/]{43}=$')
);
--> statement-breakpoint
ALTER TABLE "sealing_key_owner" ADD CONSTRAINT "sealing_key_owner_organization_id_organization_id_fkey" FOREIGN KEY ("organization_id") REFERENCES "organization"("id") ON DELETE CASCADE;--> statement-breakpoint
-- The earliest record of each key owns it, counting keys an Organization kept after forgetting its Servers. A key already
-- recorded by more than one Organization is reported in the database's log, not removed.
INSERT INTO "sealing_key_owner" ("public_key", "organization_id", "created_at")
SELECT DISTINCT ON ("public_key") "public_key", "organization_id", "created_at"
FROM (
  SELECT "public_key", "organization_id", "created_at" FROM "cluster_sealing_key"
  UNION ALL
  SELECT "public_key", "organization_id", "created_at" FROM "organization_sealing_key"
) AS "records"
ORDER BY "public_key", "created_at", "organization_id"
ON CONFLICT ("public_key") DO NOTHING;
--> statement-breakpoint
DO $$
DECLARE shared integer;
BEGIN
  SELECT count(*) INTO shared FROM (
    SELECT "public_key" FROM (
      SELECT "public_key", "organization_id" FROM "cluster_sealing_key"
      UNION ALL
      SELECT "public_key", "organization_id" FROM "organization_sealing_key"
    ) AS "records" GROUP BY "public_key" HAVING count(DISTINCT "organization_id") > 1
  ) AS keys;
  IF shared > 0 THEN
    RAISE WARNING 'sealing_key_owner: % Sealing public key(s) are recorded for more than one Organization; the earliest keeps each', shared
      USING HINT = 'List them with: SELECT "public_key", array_agg(DISTINCT "organization_id") FROM (SELECT "public_key", "organization_id" FROM "cluster_sealing_key" UNION ALL SELECT "public_key", "organization_id" FROM "organization_sealing_key") AS "records" GROUP BY "public_key" HAVING count(DISTINCT "organization_id") > 1';
  END IF;
END $$;
