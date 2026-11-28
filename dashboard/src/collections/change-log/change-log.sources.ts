import { storeChangeSources } from "#/modules/config-store/store.change-sources";

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
  server_web_traffic_attempt: { key: ["id"] },
  organization_cloudflare_ingress: { key: ["organization_id"] },
  ingress_doors_attempt: { key: ["id"] },
  volume_run: { key: ["id"] },
  service_act: { key: ["id"] },
  volume_placement: { key: ["volume_id"] },
  namespace_cleanup: { key: ["id"] },
  organization_server_upgrades: { key: ["organization_id"] },
  organization_cluster_name: { key: ["organization_id"] },
  organization_support_access: { key: ["organization_id"] },
  operation_approvals: { key: ["id"] },
  organization_github_installation: { key: ["installation_id"] },
  uptime_check: { key: ["id"] },
  // Logs the check id, so a change names the one check whose open incident it touched.
  uptime_incident: { key: ["check_id"] },
  ...storeChangeSources,
} satisfies Record<string, { organizationColumn?: string; key: readonly string[] }>;

export type ChangeSource = keyof typeof changeSources;
