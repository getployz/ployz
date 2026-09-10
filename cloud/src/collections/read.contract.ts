import { Schema } from "effect";

export const collectionReadInput = Schema.Struct({
  table: Schema.Literals([
    "project", "environment", "service", "resource_lineage",
    "environment_resource", "environment_canvas_node_position",
    "environment_deployment", "environment_saved_state_snapshot",
    "environment_node_config_snapshot", "environment_node_introduction",
    "volume_remove_attempt", "github_repository_cache",
  ]),
  userId: Schema.String,
  organizationSlug: Schema.optional(Schema.String),
});

export type CollectionReadInput = typeof collectionReadInput.Type;
