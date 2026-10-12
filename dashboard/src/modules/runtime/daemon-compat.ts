import type { CapabilityName } from "@ployz/sdk/capabilities";
import { Data } from "effect";

export type Requirement = { readonly capability: CapabilityName; readonly feature: string; readonly plural?: true };

export const requirements = {
  liveLogs: { capability: "ployz.logs.tail.v1", feature: "Logs", plural: true },
  olderLogs: { capability: "ployz.logs.history.v2", feature: "Older logs", plural: true },
  volumeMove: { capability: "ployz.volume.mirror.declare.v1", feature: "Volume move" },
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

export function lacks<F extends { readonly machineId: string; readonly capability?: string | undefined }>(failures: readonly F[]) {
  const found: ServerLacks[] = [];
  const rest: F[] = [];
  for (const failure of failures) {
    if (failure.capability === undefined) rest.push(failure);
    else found.push(new ServerLacks({ machineId: failure.machineId, capability: failure.capability }));
  }
  return { lacks: found, rest };
}

/** Whether a Server's record lists the capability; `unknown` when it lists none, as from a daemon older than the list. */
export function serverSupports(machine: { readonly capabilities: readonly string[] }, requirement: Requirement): boolean | "unknown" {
  if (machine.capabilities.length === 0) return "unknown";
  return machine.capabilities.includes(requirement.capability);
}
