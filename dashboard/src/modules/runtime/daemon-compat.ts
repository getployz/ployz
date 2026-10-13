import type { CapabilityName } from "@ployz/sdk/capabilities";
import { Data } from "effect";

export type Requirement = { readonly capability: CapabilityName; readonly feature: string; readonly plural?: true };

export const requirements = {
  liveLogs: { capability: "ployz.logs.tail.v1", feature: "Logs", plural: true },
  olderLogs: { capability: "ployz.logs.history.v2", feature: "Older logs", plural: true },
  volumeMove: { capability: "ployz.volume.copy.set.v1", feature: "Volume move" },
  pruneImages: { capability: "ployz.image.prune.v1", feature: "Image cleanup" },
  buildGrantInspect: { capability: "ployz.build.grant.inspect.v1", feature: "GitHub builds", plural: true },
  leave: { capability: "ployz.machine.leave.v1", feature: "Server removal" },
  forget: { capability: "ployz.machine.forget.v1", feature: "Server removal" },
  ensureContainer: { capability: "ployz.container.ensure.v1", feature: "Deploys", plural: true },
  metrics: { capability: "ployz.metrics.history.v1", feature: "Metrics", plural: true },
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

/** The Servers whose records say they lack the capability. A Server whose record lists none is not named: the call is attempted. */
export function serversLacking<T extends { readonly name: string; readonly capabilities: readonly string[] }>(machines: readonly T[], requirement: Requirement) {
  return machines.filter((machine) => serverSupports(machine, requirement) === false).map((machine) => machine.name);
}
