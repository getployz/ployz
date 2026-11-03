-- Gives an owner to each key recorded without one, as while Cloud ran a release from before owners: the earliest record
-- owns it, counting keys an Organization kept after forgetting its Servers. Existing owners are kept. A key already recorded
-- by more than one Organization is reported in the database's log, not removed.
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
