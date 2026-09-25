CREATE FUNCTION pg_temp.build_method_service(config jsonb) RETURNS jsonb LANGUAGE sql AS $$
 SELECT CASE WHEN config->'build' ? 'builder'
  THEN jsonb_set(config, '{build}', ((config->'build') - 'builder') || jsonb_build_object('buildMethod', config->'build'->'builder'))
  ELSE config END;
$$;--> statement-breakpoint
CREATE FUNCTION pg_temp.build_method_environment(intent jsonb) RETURNS jsonb LANGUAGE sql AS $$
 SELECT jsonb_set(intent, '{services}', COALESCE((SELECT jsonb_agg(
  node || jsonb_build_object('config', pg_temp.build_method_service(node->'config')) ORDER BY ordinal)
  FROM jsonb_array_elements(intent->'services') WITH ORDINALITY AS nodes(node,ordinal)), '[]'::jsonb));
$$;--> statement-breakpoint
UPDATE environment SET intent = pg_temp.build_method_environment(intent), revision = gen_random_uuid();--> statement-breakpoint
UPDATE environment_saved_state_snapshot SET intent = pg_temp.build_method_environment(intent);--> statement-breakpoint
UPDATE environment_node_introduction SET config = pg_temp.build_method_service(config) WHERE node_type = 'service';--> statement-breakpoint
UPDATE environment_node_config_snapshot SET config = pg_temp.build_method_service(config) WHERE node_type = 'service';--> statement-breakpoint
UPDATE environment_node_introduction_secret SET authored_intent = pg_temp.build_method_environment(authored_intent);
