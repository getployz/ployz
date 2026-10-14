import type { MachineUpgradeAttemptId, RowId } from "@ployz/sdk";

/** The daemon's attempt ID for a UUID: the UUID without its dashes. */
export const machineUpgradeAttemptIdOf = (uuid: string) =>
  // SAFETY: a lowercase UUID without its dashes is 32 lowercase hex digits, the daemon's attempt ID form.
  uuid.replaceAll("-", "") as MachineUpgradeAttemptId;

export const newMachineUpgradeAttemptId = () => machineUpgradeAttemptIdOf(crypto.randomUUID());

/** A Service's node row, which the Store names `{lineage}:node`. */
export const nodeRowId = (lineage: string) =>
  // SAFETY: the Store forms a node's RowId as its lineage followed by `:node`.
  `${lineage}:node` as RowId;
