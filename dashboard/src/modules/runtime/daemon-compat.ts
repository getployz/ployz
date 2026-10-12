import type { CapabilityName } from "@ployz/sdk/capabilities";
import { useLiveQuery } from "@tanstack/react-db";
import { Data } from "effect";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getRuntimeCollections } from "#/modules/runtime/runtime.collection";

/** A feature Cloud offers and the daemon capability it needs. `plural` agrees the sentence with the feature's name. */
export type Requirement = { readonly capability: CapabilityName; readonly feature: string; readonly plural?: true };

export const requirements = {
  liveLogs: { capability: "ployz.logs.tail.v1", feature: "Logs", plural: true },
  olderLogs: { capability: "ployz.logs.history.v2", feature: "Older logs", plural: true },
  volumeMove: { capability: "ployz.volume.mirror.declare.v1", feature: "Volume move" },
} as const satisfies Record<string, Requirement>;

/** The requirement whose capability a refusal named; null for a capability Cloud names no feature for. */
export function requirementFor(capability: string): Requirement | null {
  return Object.values(requirements).find((requirement) => requirement.capability === capability) ?? null;
}

/** A Server refused a verb its daemon does not serve; `capability` is the name the refusal carried. */
export class ServerLacks extends Data.TaggedError("ServerLacks")<{
  readonly machineId: string;
  readonly capability: string;
}> {
  readonly publicErrorCategory = "server-lacks" as const;

  override get message() {
    return `${this.machineId} cannot serve ${this.capability}`;
  }
}

/** Splits a log page's per-Server failures into the Servers that lack a capability and every other failure. */
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

/** Whether one Server serves `requirement`; `unknown` until its record arrives or when the record lists nothing. */
export function useServerSupports(organizationSlug: string, machineId: string, requirement: Requirement) {
  const { data: machines = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, useCollectionScope()).machines);
  const machine = machines.find((row) => row.id === machineId);
  return machine === undefined ? "unknown" : serverSupports(machine, requirement);
}
