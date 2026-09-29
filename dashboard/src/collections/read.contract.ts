import { Schema } from "effect";

/** An xid8 horizon from the Organization change log: every change before it has been read. */
export const changeCursorSchema = Schema.String.check(Schema.isPattern(/^\d{1,20}$/u));

export const collectionNames = [
  "environment_canvas_node_position", "organization_enrollment", "organization_cluster_domain",
] as const;

export const collectionReadInput = Schema.Struct({
  table: Schema.Literals(collectionNames),
  userId: Schema.String,
  organizationSlug: Schema.String,
  since: Schema.optional(changeCursorSchema),
});

export type CollectionReadInput = typeof collectionReadInput.Type;
export type CollectionName = CollectionReadInput["table"];

/** The Config Store's table families: each refreshes the Store views its tables back (`store-view.queries.ts`). */
export const storeViewNames = ["store_project", "store_environment", "store_deployment", "store_organization"] as const;
export type StoreViewName = (typeof storeViewNames)[number];

/**
 * What a change stream event names: an Org Store collection, `organization` for the organization state read, or a
 * Config Store table family.
 */
export const changeNameSchema = Schema.Literals([...collectionNames, "organization", ...storeViewNames]);
export type ChangeName = typeof changeNameSchema.Type;

/** A collection read. `full` replaces every row; otherwise drop `deleted`, then upsert `rows`. `cursor` is the next `since`. */
export type CollectionRead<Row> =
  | { full: true; rows: Row[]; cursor: string }
  | { full: false; rows: Row[]; deleted: string[]; cursor: string };
