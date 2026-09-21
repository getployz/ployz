ALTER TABLE "service" ADD COLUMN "name" text;--> statement-breakpoint
ALTER TABLE "service" ADD COLUMN "policy" jsonb DEFAULT '{"autoDeploy":true,"waitForCi":false,"watchPaths":[],"imageUpdate":{"type":"off"}}' NOT NULL;--> statement-breakpoint
ALTER TABLE "service_registry_credential" ADD COLUMN "revision" uuid DEFAULT gen_random_uuid() NOT NULL;--> statement-breakpoint
ALTER TABLE "environment_node_config_snapshot_secret" ADD COLUMN "credential_revision" uuid;
--> statement-breakpoint
UPDATE service s SET name = COALESCE(
  (SELECT node->'config'->>'name' FROM environment e, jsonb_array_elements(e.intent->'services') node
   WHERE e.id = s.environment_id AND node->>'id' = s.id::text),
  (SELECT canonical_name FROM service_lineage WHERE id = s.lineage_id)
);--> statement-breakpoint
ALTER TABLE service ALTER COLUMN name SET NOT NULL;--> statement-breakpoint
UPDATE service s SET policy = jsonb_build_object(
 'autoDeploy', COALESCE(node->'config'->'source'->'autoDeploy', 'true'::jsonb),
 'waitForCi', COALESCE(node->'config'->'source'->'waitForCi', 'false'::jsonb),
 'watchPaths', COALESCE(node->'config'->'build'->'watchPaths', '[]'::jsonb),
 'imageUpdate', COALESCE(node->'config'->'source'->'autoUpdate', '{"type":"off"}'::jsonb)
) FROM environment e, LATERAL jsonb_array_elements(e.intent->'services') node
WHERE e.id = s.environment_id AND node->>'id' = s.id::text;--> statement-breakpoint
CREATE FUNCTION pg_temp.service_configuration(config jsonb, service_id text) RETURNS jsonb LANGUAGE sql AS $$
 SELECT (config - 'name') || jsonb_build_object(
  'source', ((config->'source') - 'autoDeploy' - 'waitForCi' - 'autoUpdate') ||
    CASE WHEN config->'source'->'credentials'->>'type' = 'configured'
     THEN jsonb_build_object('credentials', jsonb_build_object('type','configured','credentialId',service_id)) ELSE '{}'::jsonb END,
  'build', COALESCE((config->'build') - 'watchPaths', '{"builder":"railpack","dockerfilePath":null}'::jsonb)
 );
$$;--> statement-breakpoint
CREATE FUNCTION pg_temp.environment_configuration(intent jsonb) RETURNS jsonb LANGUAGE sql AS $$
 SELECT jsonb_set(intent, '{services}', COALESCE((SELECT jsonb_agg(
  (node - 'encryptedRegistryUsername' - 'encryptedRegistrySecret') || jsonb_build_object('config', pg_temp.service_configuration(node->'config',node->>'id')) ORDER BY ordinal)
  FROM jsonb_array_elements(intent->'services') WITH ORDINALITY AS nodes(node,ordinal)), '[]'::jsonb));
$$;--> statement-breakpoint
UPDATE environment SET intent = pg_temp.environment_configuration(intent), revision = gen_random_uuid();--> statement-breakpoint
UPDATE environment_saved_state_snapshot SET intent = pg_temp.environment_configuration(intent);--> statement-breakpoint
UPDATE environment_node_introduction SET config = pg_temp.service_configuration(config,node_id::text) WHERE node_type = 'service';--> statement-breakpoint
UPDATE environment_node_config_snapshot SET config = pg_temp.service_configuration(config,node_id::text) WHERE node_type = 'service';--> statement-breakpoint
UPDATE environment_node_introduction_secret SET authored_intent = pg_temp.environment_configuration(authored_intent);
