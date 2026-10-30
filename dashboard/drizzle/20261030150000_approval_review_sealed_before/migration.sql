-- Before 0.3.11 an approval's stored review kept the plain value a variable had before the change that sealed it. Store
-- each such `before` sealed, as `withoutSealedBefore` writes it now: a variable row (by `holds`) whose `after` is sealed
-- and whose `before` is set and plain. Rows already in that shape compare equal and are left alone.
WITH "scrubbed" AS (
	SELECT "approval"."id", "approval"."review" || jsonb_build_object('diff', ("approval"."review"->'diff') || "lists"."changes") AS "review"
	FROM "operation_approvals" AS "approval"
	CROSS JOIN LATERAL (
		SELECT jsonb_object_agg("list"."key", (
			SELECT coalesce(jsonb_agg(
				CASE WHEN jsonb_typeof("node"."value"->'settings') = 'array' THEN jsonb_set("node"."value", '{settings}', (
					SELECT coalesce(jsonb_agg(
						CASE WHEN jsonb_typeof("setting"."value"->'after'->'secret') = 'boolean'
							AND "setting"."value"->'before' IS DISTINCT FROM 'null'::jsonb
							AND jsonb_typeof("setting"."value"->'before'->'secret') IS DISTINCT FROM 'boolean'
							AND coalesce("setting"."value"->>'path', '') !~ '(^|\.)env\.[^.]+\.(exported|description)$'
							AND CASE WHEN coalesce("setting"."value"->>'row', '') <> ''
								THEN left(substr("setting"."value"->>'row', strpos("setting"."value"->>'row', ':') + 1), 10) = 'variables.'
								ELSE coalesce("setting"."value"->>'path', '') ~ '(^|\.)(env|variables)\.[^.]+$' END
						THEN jsonb_set("setting"."value", '{before}', '{"secret":true}'::jsonb) ELSE "setting"."value" END
						ORDER BY "setting"."at"), '[]'::jsonb)
					FROM jsonb_array_elements("node"."value"->'settings') WITH ORDINALITY AS "setting"("value", "at")
				)) ELSE "node"."value" END
				ORDER BY "node"."at"), '[]'::jsonb)
			FROM jsonb_array_elements("list"."value") WITH ORDINALITY AS "node"("value", "at")
		)) AS "changes"
		FROM jsonb_each("approval"."review"->'diff') AS "list"("key", "value")
		WHERE "list"."key" IN ('changes', 'draft_changes') AND jsonb_typeof("list"."value") = 'array'
	) AS "lists"
	WHERE jsonb_typeof("approval"."review"->'diff') = 'object' AND "lists"."changes" IS NOT NULL
)
UPDATE "operation_approvals" SET "review" = "scrubbed"."review"
FROM "scrubbed"
WHERE "operation_approvals"."id" = "scrubbed"."id" AND "operation_approvals"."review" IS DISTINCT FROM "scrubbed"."review";
