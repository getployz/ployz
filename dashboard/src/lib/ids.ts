import type { RowId } from "@ployz/sdk";

/** A Service's node row, which the Store names `{lineage}:node`. */
export const nodeRowId = (lineage: string) =>
  // SAFETY: the Store forms a node's RowId as its lineage followed by `:node`.
  `${lineage}:node` as RowId;
