import type { MachineRemoveResult } from "#/modules/machines/machine-removal-states";

/** What the toast says once a removal settled. Ployz never touches the removed Server, so the last one still runs. */
export function removedToast(name: string, { release }: MachineRemoveResult) {
  if (release.kind === "kept") return { warning: true, message: `${name} left the cluster. Cloud keeps its hold: ${release.reason}` };
  if (release.kind === "released") {
    return {
      warning: false,
      message: `${name} removed. It still runs its containers: run sudo ployz-uninstall on it to stop them, or ployz server add --reset to use it again. Add a server to deploy again.`,
    };
  }
  return { warning: false, message: `${name} removed` };
}
