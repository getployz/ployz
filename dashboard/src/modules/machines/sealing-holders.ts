import type { MachineId, RuntimeWatchView } from "@ployz/sdk";

export function sealingHolders(frame: Pick<RuntimeWatchView, "machines">) {
  const held: MachineId[] = [];
  const lacking: MachineId[] = [];
  for (const { machine } of frame.machines) {
    const kind = machine.runtime.sealing?.kind;
    if (kind === "held") held.push(machine.id);
    else if (kind === "needs_access" || kind === "awaits_recovery") lacking.push(machine.id);
  }
  return { held, lacking };
}
