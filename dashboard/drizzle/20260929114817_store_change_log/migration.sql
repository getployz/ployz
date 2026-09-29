-- Config Store tables keep their Organization as text (their SQL also runs on SQLite), so the log casts it.
CREATE OR REPLACE FUNCTION organization_change_log() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
  key_expression text;
  old_keys text := 'SELECT NULL::uuid AS organization_id, NULL::text AS key WHERE false';
  new_keys text := old_keys;
BEGIN
  SELECT string_agg(format('%I::text', key_column), ' || '':'' || ')
    INTO key_expression FROM unnest(TG_ARGV[1:TG_NARGS - 1]) AS key_column;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    new_keys := format('SELECT %I::uuid AS organization_id, %s AS key FROM new_rows', TG_ARGV[0], key_expression);
  END IF;
  IF TG_OP IN ('UPDATE', 'DELETE') THEN
    old_keys := format('SELECT %I::uuid AS organization_id, %s AS key FROM old_rows', TG_ARGV[0], key_expression);
  END IF;
  EXECUTE format($sql$
    INSERT INTO organization_change (organization_id, source_table, changed_ids, deleted_ids, all_rows)
    SELECT organization_id, %L,
      CASE WHEN count(*) > 100 THEN '{}' ELSE coalesce(array_agg(key) FILTER (WHERE NOT deleted), '{}') END,
      CASE WHEN count(*) > 100 THEN '{}' ELSE coalesce(array_agg(key) FILTER (WHERE deleted), '{}') END,
      count(*) > 100
    FROM (
      SELECT DISTINCT organization_id, key, false AS deleted FROM (%s) changed
      UNION ALL
      (SELECT organization_id, key, true FROM (%s) gone EXCEPT SELECT organization_id, key, true FROM (%s) changed)
    ) keys
    GROUP BY organization_id
  $sql$, TG_TABLE_NAME, new_keys, old_keys, new_keys);
  RETURN NULL;
END
$$;
