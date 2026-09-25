import { Schema } from "effect";

/** An xid8 horizon from the Organization change log: every change before it has been read. */
export const changeCursorSchema = Schema.String.check(Schema.isPattern(/^\d{1,20}$/u));

const collectionNames = [
  "project", "environment_summary", "project_preference", "environment", "service", "resource_lineage",
  "environment_resource", "environment_canvas_node_position",
  "environment_deployment", "environment_saved_state_snapshot",
  "environment_node_config_snapshot", "environment_node_introduction",
  "volume_remove_attempt", "organization_enrollment", "organization_cluster_domain",
] as const;

export const collectionReadInput = Schema.Struct({
  table: Schema.Literals(collectionNames),
  userId: Schema.String,
  organizationSlug: Schema.String,
  since: Schema.optional(changeCursorSchema),
});

export type CollectionReadInput = typeof collectionReadInput.Type;
export type CollectionName = CollectionReadInput["table"];

/** What a change stream event names: an Org Store collection, or `organization` for the organization state read. */
export const changeNameSchema = Schema.Literals([...collectionNames, "organization"]);
export type ChangeName = typeof changeNameSchema.Type;

/** A collection read. `full` replaces every row; otherwise drop `deleted`, then upsert `rows`. `cursor` is the next `since`. */
export type CollectionRead<Row> =
  | { full: true; rows: Row[]; cursor: string }
  | { full: false; rows: Row[]; deleted: string[]; cursor: string };
