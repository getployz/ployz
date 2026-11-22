import type { Departed } from "@ployz/sdk";

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

/**
 * A settled removal: how the Server departed, and what became of Cloud's hold. `resetWarning` is `departed` as prose,
 * all a result older than `departed` holds. A removal without a reset has no `departed`.
 */
export type MachineRemoveResult = { resetWarning: string | null; departed?: Departed; release: ServerRelease };
