/**
 * The Config Store's organization-owned tables, keyed by the scope whose views a change invalidates. The Store
 * creates them when Cloud first opens it, so Cloud attaches their triggers then (see config-store.server.ts).
 * The Organization column is text there, because the Store's SQL also runs on SQLite.
 */
export const storeChangeSources = {
  config_project: { key: ["id"] },
  config_environment: { key: ["id"] },
  config_node_introduction: { key: ["environment_id"] },
  config_saved: { key: ["environment_id"] },
  config_namespace: { key: ["environment_id"] },
  config_deployment: { key: ["id"] },
  config_build: { key: ["deployment_id"] },
  config_deployment_row: { key: ["deployment_id"] },
  config_applied: { key: ["environment_id"] },
  config_registry_credential: { key: ["environment_id"] },
  config_service_policy: { key: ["environment_id"] },
  config_environment_branch: { key: ["environment_id"] },
  config_sync_base: { key: ["environment_id"] },
  config_never_sync: { key: ["environment_id"] },
  // ponytail: logs the receiver only; another tab on the sender sees what it synced once the sender changes too.
  config_sync_arrival: { key: ["environment_id"] },
  config_build_order: { key: ["organization_id"] },
  config_pr_plan: { key: ["project_id"] },
  config_pr_environment: { key: ["environment_id"] },
  config_conditional_sync: { key: ["environment_id"] },
  config_held_secret: { key: ["environment_id"] },
  config_pull_request: { key: ["repository_id", "number"] },
} satisfies Record<string, { key: readonly string[] }>;

/**
 * Every organization-owned table: the column naming its Organization (`organization_id` unless given)
 * and the key columns its change trigger logs (joined with ':'). The spec logs every organization-owned
 * table (#1042 user story 21), including tables that feed no collection yet. The migration attaches each
 * trigger with these columns; the ownership test checks the database against this list.
 */
export const changeSources = {
  organization: { organizationColumn: "id", key: ["id"] },
  environment_canvas_node_position: { key: ["resource_type", "resource_id"] },
  organization_pairing: { key: ["organization_id"] },
  organization_cluster_domain: { key: ["organization_id"] },
  enrollment_allocation: { key: ["cluster_key"] },
  invitation: { key: ["id"] },
  machine_enrollment_token: { key: ["id"] },
  machine_remove_attempt: { key: ["id"] },
  member: { key: ["id"] },
  organization_billing_state: { key: ["organization_id"] },
  organization_machine: { key: ["machine_id"] },
  server_upgrade_attempt: { key: ["id"] },
  server_drain_attempt: { key: ["id"] },
  organization_server_upgrades: { key: ["organization_id"] },
  organization_settings: { key: ["organization_id"] },
  operation_approvals: { key: ["id"] },
  ...storeChangeSources,
} satisfies Record<string, { organizationColumn?: string; key: readonly string[] }>;

export type ChangeSource = keyof typeof changeSources;
