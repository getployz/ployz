import type { RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { parseDockerVolumeName, type VolumeRunView } from "./volume-run";

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

/**
 * What a Volume's canvas tray says while its copies change: the run under way, or a copy switching with no run as a
 * bare "Moving". Its mirrors are nodes of their own under the tray.
 */
export function trayMoving(input: {
  readonly volume: { readonly id: string };
  readonly copies: readonly RuntimeVolumeCopy[];
  readonly active: VolumeRunView | null;
}): string | null {
  const own = input.copies.filter((copy) => parseDockerVolumeName(copy.name)?.volumeId === input.volume.id);
  if (input.active !== null && input.active.kind !== "sync") return runWords(input.active);
  return own.some((copy) => copy.role === "switching") ? "Moving" : null;
}
