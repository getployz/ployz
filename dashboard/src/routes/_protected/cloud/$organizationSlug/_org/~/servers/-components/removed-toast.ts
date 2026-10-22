import type { MachineRemoveResult } from "#/modules/machines/machine-removal";

/**
 * What the toast says once a removal settled. Cloud keeps its hold only when it says so; a Server the Cluster had
 * already forgotten loses Cloud's hold, and only its reset is in doubt.
 */
export function removedToast(name: string, { resetWarning, release }: MachineRemoveResult) {
  if (release.kind === "kept") return { warning: true, message: `${name} left the cluster. Cloud keeps its hold: ${release.reason}` };
  if (resetWarning !== null) return { warning: true, message: `${name} removed, but ${resetWarning}.` };
  if (release.kind === "released") return { warning: false, message: `${name} removed. Add a server to deploy again.` };
  return { warning: false, message: `${name} removed` };
}
