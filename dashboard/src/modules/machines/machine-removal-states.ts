export const MACHINE_REMOVE_ATTEMPT_STATES = [
  "pending",
  "running",
  "succeeded",
  "failed",
  "cancelled",
  "missing_identities",
] as const;

export type MachineRemoveAttemptState =
  (typeof MACHINE_REMOVE_ATTEMPT_STATES)[number];

/** What became of Cloud's hold once a removed Server left the Cluster. */
export type ServerRelease =
  /** Other Servers keep the Cluster; Cloud dropped only this Server's key. */
  | { kind: "others_remain" }
  /** It was the last: Cloud forgot the Cluster, and the next Server founds a new one. */
  | { kind: "released" }
  /** Cloud kept its hold, for `reason`. */
  | { kind: "kept"; reason: string };

/** A settled removal: what became of Cloud's hold. A result an older Cloud saved may carry more; nothing reads it. */
export type MachineRemoveResult = { release: ServerRelease };
