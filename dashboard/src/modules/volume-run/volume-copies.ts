import type { VolumeRunView } from "./volume-run";

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
