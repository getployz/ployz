import type { CapabilityName } from "@ployz/sdk/capabilities";
import { Data } from "effect";

export type Requirement = { readonly capability: CapabilityName; readonly feature: string; readonly plural?: true };

export const requirements = {
  liveLogs: { capability: "ployz.logs.tail.v1", feature: "Logs", plural: true },
  olderLogs: { capability: "ployz.logs.history.v2", feature: "Older logs", plural: true },
  volumeMove: { capability: "ployz.volume.mirror.declare.v1", feature: "Volume move" },
  snapshots: { capability: "ployz.volume.snapshot.v1", feature: "Snapshots", plural: true },
  snapshotList: { capability: "ployz.volume.snapshot.list.v1", feature: "Snapshots", plural: true },
} as const satisfies Record<string, Requirement>;

export function requirementFor(capability: string): Requirement | null {
  return Object.values(requirements).find((requirement) => requirement.capability === capability) ?? null;
}

export class ServerLacks extends Data.TaggedError("ServerLacks")<{
  readonly machineId: string;
  readonly capability: string;
}> {
  readonly publicErrorCategory = "server-lacks" as const;

  override get message() {
    return `${this.machineId} cannot serve ${this.capability}`;
  }
}

/** Whether a Server's record lists the capability; `unknown` when it lists none, as from a daemon older than the list. */
export function serverSupports(machine: { readonly capabilities: readonly string[] }, requirement: Requirement): boolean | "unknown" {
  if (machine.capabilities.length === 0) return "unknown";
  return machine.capabilities.includes(requirement.capability);
}
