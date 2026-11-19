import type { ConfigQuery } from "@ployz/sdk";

/**
 * The Config Store queries the Store answers only after Cloud looks at the Cluster (`domain-evidence.server.ts`): a
 * domain's state lives on the Servers. A loader streams these (`streamStoreViews`) and never awaits them.
 */
export const CLUSTER_QUERIES = ["domains", "domain"] as const;

export type ClusterQuery = Extract<ConfigQuery, { query: (typeof CLUSTER_QUERIES)[number] }>;

/** A Config Store query the Store answers from its own rows, which a loader may await. */
export type AwaitableQuery = Exclude<ConfigQuery, ClusterQuery>;
