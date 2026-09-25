/**
 * Every organization-owned table: the column naming its Organization (`organization_id` unless given)
 * and the key columns its change trigger logs (joined with ':'). The spec logs every organization-owned
 * table (#1042 user story 21), including tables that feed no collection yet. The migration attaches each
 * trigger with these columns; the ownership test checks the database against this list.
 */
export const changeSources = {
  organization: { organizationColumn: "id", key: ["id"] },
  project: { key: ["id"] },
  environment: { key: ["id"] },
  user_project_preference: { key: ["project_id"] },
  service: { key: ["id"] },
  resource_lineage: { key: ["id"] },
  environment_resource: { key: ["id"] },
  environment_canvas_node_position: { key: ["resource_type", "resource_id"] },
  environment_deployment: { key: ["id"] },
  environment_deployment_event: { key: ["deployment_id"] },
  environment_saved_state_snapshot: { key: ["id"] },
  environment_node_config_snapshot: { key: ["id"] },
  environment_node_introduction: { key: ["node_type", "node_id"] },
  volume_remove_attempt: { key: ["id"] },
  organization_pairing: { key: ["organization_id"] },
  core_operation_event: { key: ["id"] },
  core_operation_watch: { key: ["id"] },
  enrollment_allocation: { key: ["cluster_key"] },
  environment_deployment_build_output: { key: ["id"] },
  environment_deployment_build_step: { key: ["id"] },
  environment_deployment_secret: { key: ["environment_deployment_id"] },
  environment_node_config_snapshot_secret: { key: ["snapshot_id"] },
  environment_node_introduction_secret: { key: ["environment_id", "node_type", "node_id"] },
  github_environment_trigger: { key: ["id"] },
  invitation: { key: ["id"] },
  machine_enrollment_token: { key: ["id"] },
  machine_remove_attempt: { key: ["id"] },
  member: { key: ["id"] },
  organization_billing_state: { key: ["organization_id"] },
  organization_machine: { key: ["machine_id"] },
  service_lineage: { key: ["id"] },
  service_registry_credential: { key: ["service_id"] },
  teardown_attempt: { key: ["id"] },
  variable: { key: ["id"] },
  variable_secret: { key: ["variable_id"] },
} satisfies Record<string, { organizationColumn?: string; key: readonly string[] }>;

export type ChangeSource = keyof typeof changeSources;
