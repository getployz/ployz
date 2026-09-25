import * as EffectRecord from "effect/Record";
import type { ChangeName } from "./read.contract";
import type { ChangeSource } from "#/modules/organization/change-log.sources";

/**
 * The source tables each change stream name reads: an Org Store collection or `organization`. The first is
 * its key table: the collection's rows are keyed by the key that table logs, and every other source
 * logs that same key through a foreign key to it.
 */
export const changeNameSources = {
  organization: ["organization"],
  project: ["project"],
  environment: ["environment"],
  environment_summary: ["environment"],
  project_preference: ["user_project_preference"],
  service: ["service"],
  resource_lineage: ["resource_lineage"],
  environment_resource: ["environment_resource"],
  environment_canvas_node_position: ["environment_canvas_node_position"],
  environment_deployment: ["environment_deployment", "environment_deployment_event"],
  environment_saved_state_snapshot: ["environment_saved_state_snapshot"],
  environment_node_config_snapshot: ["environment_node_config_snapshot"],
  environment_node_introduction: ["environment_node_introduction"],
  volume_remove_attempt: ["volume_remove_attempt"],
  organization_enrollment: ["organization_pairing"],
} satisfies Record<ChangeName, readonly [ChangeSource, ...ChangeSource[]]>;

export function collectionsOf(sourceTables: Iterable<ChangeSource>) {
  const tables = new Set(sourceTables);
  return EffectRecord.keys(changeNameSources).filter((name) => changeNameSources[name].some((table) => tables.has(table)));
}
