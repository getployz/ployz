import type { MachineRemoveResult } from "#/modules/machines/machine-removal-states";

/** What the toast says once a removal settled. */
export function removedToast(name: string, { release }: MachineRemoveResult) {
  if (release.kind === "kept") return { warning: true, message: `${name} left the cluster. Cloud keeps its hold: ${release.reason}` };
  if (release.kind === "released") {
    return {
      warning: false,
      message: `${name} removed. Add a server to deploy again.`,
    };
  }
  return { warning: false, message: `${name} removed` };
}
