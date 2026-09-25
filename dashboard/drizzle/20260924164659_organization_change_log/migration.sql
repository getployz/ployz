CREATE TABLE "organization_change" (
	"seq" bigint PRIMARY KEY GENERATED ALWAYS AS IDENTITY (sequence name "organization_change_seq_seq" INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 START WITH 1 CACHE 1),
	"xid" xid8 DEFAULT pg_current_xact_id() NOT NULL,
	"organization_id" uuid NOT NULL,
	"source_table" text NOT NULL,
	"changed_ids" text[] NOT NULL,
	"deleted_ids" text[] NOT NULL,
	"all_rows" boolean NOT NULL,
	"created_at" timestamp with time zone DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE INDEX "organization_change_organization_id_xid_idx" ON "organization_change" ("organization_id","xid");
--> statement-breakpoint
CREATE INDEX "organization_change_xid_idx" ON "organization_change" ("xid");
--> statement-breakpoint
-- One log row per statement per Organization. Arguments: the Organization column, then the key columns
-- (joined with ':' into the collection key). Over 100 keys set all_rows and drop the ids.
CREATE FUNCTION organization_change_log() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
  key_expression text;
  old_keys text := 'SELECT NULL::uuid AS organization_id, NULL::text AS key WHERE false';
  new_keys text := old_keys;
BEGIN
  SELECT string_agg(format('%I::text', key_column), ' || '':'' || ')
    INTO key_expression FROM unnest(TG_ARGV[1:TG_NARGS - 1]) AS key_column;
  IF TG_OP IN ('INSERT', 'UPDATE') THEN
    new_keys := format('SELECT %I AS organization_id, %s AS key FROM new_rows', TG_ARGV[0], key_expression);
  END IF;
  IF TG_OP IN ('UPDATE', 'DELETE') THEN
    old_keys := format('SELECT %I AS organization_id, %s AS key FROM old_rows', TG_ARGV[0], key_expression);
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
$$;--> statement-breakpoint
-- Transition tables allow one event per trigger, so each table gets three.
CREATE FUNCTION organization_change_attach(target regclass, organization_column text, VARIADIC key_columns text[])
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
  arguments text := (SELECT string_agg(quote_literal(argument), ', ') FROM unnest(organization_column || key_columns) AS argument);
BEGIN
  EXECUTE format('CREATE TRIGGER organization_change_insert AFTER INSERT ON %s REFERENCING NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION organization_change_log(%s)', target, arguments);
  EXECUTE format('CREATE TRIGGER organization_change_update AFTER UPDATE ON %s REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows
    FOR EACH STATEMENT EXECUTE FUNCTION organization_change_log(%s)', target, arguments);
  EXECUTE format('CREATE TRIGGER organization_change_delete AFTER DELETE ON %s REFERENCING OLD TABLE AS old_rows
    FOR EACH STATEMENT EXECUTE FUNCTION organization_change_log(%s)', target, arguments);
END
$$;--> statement-breakpoint
-- Every organization-owned table logs its changes; `organization` is keyed by its own id.
-- Organization and key columns match changeSources in src/modules/organization/change-log.sources.ts.
SELECT organization_change_attach('organization', 'id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('project', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('user_project_preference', 'organization_id', 'project_id');
--> statement-breakpoint
SELECT organization_change_attach('service', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('resource_lineage', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_resource', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_canvas_node_position', 'organization_id', 'resource_type', 'resource_id');
--> statement-breakpoint
SELECT organization_change_attach('environment_deployment', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_deployment_event', 'organization_id', 'deployment_id');
--> statement-breakpoint
SELECT organization_change_attach('environment_saved_state_snapshot', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_node_config_snapshot', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_node_introduction', 'organization_id', 'node_type', 'node_id');
--> statement-breakpoint
SELECT organization_change_attach('volume_remove_attempt', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('organization_pairing', 'organization_id', 'organization_id');
--> statement-breakpoint
SELECT organization_change_attach('core_operation_event', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('core_operation_watch', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('enrollment_allocation', 'organization_id', 'cluster_key');
--> statement-breakpoint
SELECT organization_change_attach('environment_deployment_build_output', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_deployment_build_step', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('environment_deployment_secret', 'organization_id', 'environment_deployment_id');
--> statement-breakpoint
SELECT organization_change_attach('environment_node_config_snapshot_secret', 'organization_id', 'snapshot_id');
--> statement-breakpoint
SELECT organization_change_attach('environment_node_introduction_secret', 'organization_id', 'environment_id', 'node_type', 'node_id');
--> statement-breakpoint
SELECT organization_change_attach('github_environment_trigger', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('invitation', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('machine_enrollment_token', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('machine_remove_attempt', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('member', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('organization_billing_state', 'organization_id', 'organization_id');
--> statement-breakpoint
SELECT organization_change_attach('organization_machine', 'organization_id', 'machine_id');
--> statement-breakpoint
SELECT organization_change_attach('service_lineage', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('service_registry_credential', 'organization_id', 'service_id');
--> statement-breakpoint
SELECT organization_change_attach('teardown_attempt', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('variable', 'organization_id', 'id');
--> statement-breakpoint
SELECT organization_change_attach('variable_secret', 'organization_id', 'variable_id');
