import * as EffectRecord from "effect/Record";
import type { ChangeName, StoreViewName } from "./read.contract";
import type { ChangeSource } from "#/modules/organization/change-log.sources";

/**
 * The Config Store's table families, shaped like `changeNameSources`: a family's first source is its key table and
 * every other source logs that table's key, so a logged key names the Project, Environment or Deployment whose views
 * changed. A created or deleted row is one a view gains or drops. Which views each family refreshes is
 * `refreshedBy` in `store-view.queries.ts`.
 */
export const storeViewSources = {
  store_project: ["config_project", "config_pr_plan"],
  store_environment: ["config_environment", "config_node_introduction", "config_saved", "config_namespace", "config_applied", "config_registry_credential", "config_service_policy", "config_environment_branch", "config_sync_base", "config_never_sync", "config_sync_pending", "config_followed", "config_pr_environment", "config_conditional_sync"],
  store_deployment: ["config_deployment", "config_build"],
  store_organization: ["config_build_order"],
  store_pull_request: ["config_pull_request"],
} satisfies Record<StoreViewName, readonly [ChangeSource, ...ChangeSource[]]>;

/**
 * The source tables each change stream name reads: an Org Store collection, `organization`, or a Config Store table
 * family. A collection's first source is its key table: its rows are keyed by the key that table logs, and every other
 * source logs that same key through a foreign key to it.
 */
export const changeNameSources = {
  organization: ["organization"],
  environment_canvas_node_position: ["environment_canvas_node_position"],
  organization_enrollment: ["organization_pairing"],
  organization_cluster_domain: ["organization_cluster_domain"],
  ...storeViewSources,
} satisfies Record<ChangeName, readonly [ChangeSource, ...ChangeSource[]]>;

export function collectionsOf(sourceTables: Iterable<ChangeSource>) {
  const tables = new Set(sourceTables);
  return EffectRecord.keys(changeNameSources).filter((name) => changeNameSources[name].some((table) => tables.has(table)));
}
