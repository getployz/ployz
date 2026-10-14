-- Before the rowless sync, a mirror on a silent Server read as refreshed when the Volume's newest done sync run finished.
INSERT INTO "volume_mirror_status" ("organization_id", "volume_id", "machine_id", "matched_at", "updated_at")
SELECT "placement"."organization_id", "placement"."volume_id", "mirror"."machine_id", "run"."finished_at", now()
FROM "volume_placement" AS "placement"
CROSS JOIN LATERAL jsonb_array_elements_text("placement"."mirrors") AS "mirror"("machine_id")
JOIN LATERAL (
	SELECT "finished_at" FROM "volume_run"
	WHERE "organization_id" = "placement"."organization_id" AND "volume_id" = "placement"."volume_id" AND "kind" = 'sync' AND "state" = 'done'
	ORDER BY "created_at" DESC, "id" DESC LIMIT 1
) AS "run" ON "run"."finished_at" IS NOT NULL
ON CONFLICT DO NOTHING;
