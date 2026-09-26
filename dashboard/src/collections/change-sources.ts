import * as EffectRecord from "effect/Record";
import type { ChangeName } from "./read.contract";
import type { ChangeSource } from "#/modules/organization/change-log.sources";

/**
 * The source tables each change stream name reads: an Org Store collection, `organization`, or the
 * change-state projection. A collection's first source is its key table: its rows are keyed by the key
 * that table logs, and every other source logs that same key through a foreign key to it.
 */
export const changeNameSources = {
  organization: ["organization"],
  // Progress lands in environment_deployment_event, which is deliberately not a source.
  environment_change_state: ["environment_deployment", "environment_saved_state_snapshot"],
  project: ["project"],
  environment: ["environment"],
  environment_summary: ["environment"],
  project_preference: ["user_project_preference"],
  service: ["service"],
  resource_lineage: ["resource_lineage"],
  environment_resource: ["environment_resource"],
  environment_canvas_node_position: ["environment_canvas_node_position"],
  environment_deployment: ["environment_deployment", "environment_deployment_event"],
  environment_node_introduction: ["environment_node_introduction"],
  organization_enrollment: ["organization_pairing"],
  organization_cluster_domain: ["organization_cluster_domain"],
  organization_build_order: ["organization_build_order"],
} satisfies Record<ChangeName, readonly [ChangeSource, ...ChangeSource[]]>;

export function collectionsOf(sourceTables: Iterable<ChangeSource>) {
  const tables = new Set(sourceTables);
  return EffectRecord.keys(changeNameSources).filter((name) => changeNameSources[name].some((table) => tables.has(table)));
}
