// The one map from features to Server capabilities. Cloud code names a capability, never a release number.
import type { RuntimeWatchView } from "@ployz/sdk";
import type { CapabilityName } from "@ployz/sdk/capabilities";
import { Data } from "effect";

export type Requirement = { readonly capability: CapabilityName; readonly feature: string; readonly plural?: true };

export const requirements = {
  liveLogs: { capability: "ployz.logs.tail.v1", feature: "Logs", plural: true },
  volumeMove: { capability: "ployz.volume.copy.set.v1", feature: "Volume move" },
  volumeFloor: { capability: "ployz.volume.copy.floor.v1", feature: "Volume move" },
  volumeSize: { capability: "ployz.volume.size.set.v1", feature: "Volume resize" },
  volumeOrigin: { capability: "ployz.volume.origin.v1", feature: "Volume move" },
  volumeStart: { capability: "ployz.volume.start.v1", feature: "Volume move" },
  volumeStartImage: { capability: "ployz.volume.start.image.v1", feature: "Volume move" },
  volumeCutRunning: { capability: "ployz.volume.cut.running.v1", feature: "Volume move" },
  volumeKeepWritten: { capability: "ployz.volume.keep-written.v1", feature: "Volume sync" },
  volumeShrink: { capability: "ployz.volume.size.shrink.v1", feature: "Volume shrink" },
  pruneImages: { capability: "ployz.image.prune.v1", feature: "Image cleanup" },
  buildGrantInspect: { capability: "ployz.build.grant.inspect.v1", feature: "GitHub builds", plural: true },
  forget: { capability: "ployz.machine.forget.v1", feature: "Server removal" },
  ensureContainer: { capability: "ployz.container.ensure.v1", feature: "Deploys", plural: true },
} as const satisfies Record<string, Requirement>;

/**
 * What a Server must list to take a Writer Create, a Move's or a Release's: it starts the Service from its approval's
 * proof, from the image its promote records.
 */
export const writerStart: readonly Requirement[] = [requirements.volumeStart, requirements.volumeStartImage];

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

type WatchedMachine = RuntimeWatchView["machines"][number]["machine"];

/** The Servers a Cluster frame lists, each with the capabilities its record lists (none when it lists none, or with no frame). */
export const frameServers = (frame: RuntimeWatchView | null | undefined, include: (machine: WatchedMachine) => boolean = () => true) =>
  (frame?.machines ?? []).filter(({ machine }) => include(machine))
    .map(({ machine }) => ({ id: machine.id, name: machine.name, capabilities: machine.runtime.capabilities ?? [] }));

/** The Servers whose records say they lack the capability. A Server whose record lists none is not named: the call is attempted. */
export function serversLacking<T extends { readonly name: string; readonly capabilities: readonly string[] }>(machines: readonly T[], requirement: Requirement) {
  return machines.filter((machine) => serverSupports(machine, requirement) === false).map((machine) => machine.name);
}
