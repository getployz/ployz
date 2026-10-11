import type { RuntimeMachineRecord, RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { copyName, parseDockerVolumeName, type VolumeRunView } from "./volume-run";

/** Any run's arguments, each field present only for the kinds that take it. */
function runArgs(run: VolumeRunView): Partial<{ readonly to: string; readonly from: string; readonly full: boolean; readonly slot: string | null; readonly into: { readonly name: string } }> {
  return run.args;
}

/** A run under way, in placement words: "Moving to web-2". */
export function runWords(run: VolumeRunView): string {
  const args = runArgs(run);
  return {
    mirror: `Copying to ${args.to}`,
    sync: "Refreshing mirror",
    delete_mirror: args.slot ? `Deleting the copy on ${args.slot}` : "Deleting a copy",
    move: `Moving to ${args.to}`,
    release: "Resuming the primary",
    restore: `Using mirror on ${args.from}`,
    seed: `Restoring into ${args.into?.name}`,
  }[run.kind];
}

export type TrayCopy = { readonly kind: "moving"; readonly text: string } | { readonly kind: "mirror"; readonly label: string } | null;

/**
 * What a Volume's canvas tray says about its copies: the run under way, a copy switching with no run as a bare
 * "Moving", else the mirror by name.
 */
export function trayCopy(input: {
  readonly volume: { readonly id: string; readonly name: string };
  readonly copies: readonly RuntimeVolumeCopy[];
  readonly machines: readonly Pick<RuntimeMachineRecord, "id" | "name">[];
  readonly active: VolumeRunView | null;
}): TrayCopy {
  const own = input.copies.filter((copy) => parseDockerVolumeName(copy.name)?.volumeId === input.volume.id);
  if (input.active !== null && input.active.kind !== "sync") return { kind: "moving", text: runWords(input.active) };
  if (own.some((copy) => copy.role === "switching")) return { kind: "moving", text: "Moving" };
  const mirror = own.find((copy) => copy.role === "slot");
  if (mirror === undefined) return null;
  const server = input.machines.find((machine) => machine.id === mirror.machineId)?.name ?? mirror.machineId;
  return { kind: "mirror", label: copyName(input.volume.name, server) };
}
