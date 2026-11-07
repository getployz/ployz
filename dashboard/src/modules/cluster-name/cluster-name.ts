import { Schema } from "effect";

export const CLUSTER_NAME_MAX = 63;

export const CLUSTER_NAME_TOO_LONG = `${CLUSTER_NAME_MAX} characters at most.`;
export const CLUSTER_NAME_RULE = "Use lowercase letters, numbers and hyphens. Start and end with a letter or number.";

const LABEL = /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$/u;

/** One DNS label, as the Engine's `ClusterName` takes it. */
export const ClusterName = Schema.String.check(
  Schema.isMaxLength(CLUSTER_NAME_MAX, { message: CLUSTER_NAME_TOO_LONG }),
  Schema.isPattern(LABEL, { message: CLUSTER_NAME_RULE }),
);

/** Why `name` can't be a Cluster Name, or null when it can. */
export function clusterNameProblem(name: string) {
  if (name.length > CLUSTER_NAME_MAX) return CLUSTER_NAME_TOO_LONG;
  return LABEL.test(name) ? null : CLUSTER_NAME_RULE;
}

/** The name a first Server founds the Cluster with when nobody chose one: the Organization slug, cut to one DNS label. */
export const defaultClusterName = (organizationSlug: string) =>
  organizationSlug.slice(0, CLUSTER_NAME_MAX).replace(/-+$/u, "");

/** The Org Store's view of the Organization's Cluster Name, keyed by Organization ID. */
export type ClusterNameRow = {
  readonly id: string;
  readonly name: string;
  /** The name a Server last confirmed the Cluster holds; null until one does. */
  readonly appliedName: string | null;
  /** Why the Cluster refused `name`; null unless it did. */
  readonly refusal: string | null;
};

export type ClusterNameView =
  /** `current`: what the Cluster holds, or what its first Server founds it with. `pending`: saved, not yet on a Server. */
  | { readonly state: "current" | "pending"; readonly name: string }
  | { readonly state: "refused"; readonly name: string; readonly refusal: string }
  /** The Cluster was founded before Cloud named Clusters, and nobody named it since. */
  | { readonly state: "unset"; readonly name: null };

export function clusterNameView(row: ClusterNameRow | undefined, organizationSlug: string, founded: boolean): ClusterNameView {
  if (row === undefined) return founded ? { state: "unset", name: null } : { state: "current", name: defaultClusterName(organizationSlug) };
  if (row.refusal !== null) return { state: "refused", name: row.name, refusal: row.refusal };
  return { state: row.appliedName === row.name || !founded ? "current" : "pending", name: row.name };
}

export const RenameClusterInput = Schema.Struct({ organizationSlug: Schema.String, name: ClusterName });
