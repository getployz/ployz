import type { MachineId } from "@ployz/sdk";
import type { VolumeChange } from "#/modules/approval-session/volume-approval";
import { gigabytes } from "#/modules/config-store/store-volumes";
import { observePlacement } from "#/modules/volume-placement/volume-placement";
import { copyName, type Member, type VolumeRunInput } from "#/modules/volume-run/volume-run";
import type { PlacementTerms } from "#/modules/volume-placement/placement-approvals";

/** Why a run's workflow can't be drafted here, in words shown as they are. */
export class UndraftedRun extends Error {}

/**
 * The Server a Move off the Volume's writer leaves, which the browser reads the running spec from before it drafts the
 * Move. Null for any other run.
 */
export function movesFrom(run: VolumeRunInput, members: readonly Member[]): MachineId | null {
  return run.kind === "move" ? observePlacement(members).primary?.id ?? null : null;
}

/**
 * The workflow `run` asks the Servers of Volume `name` to take, drafted as the CLI drafts it over the copies in
 * `members`, so its approval names what the run will ask. Throws `UndraftedRun` for a run the dashboard can't draft.
 */
export function runChange(name: string, run: VolumeRunInput, members: readonly Member[], terms: PlacementTerms): VolumeChange {
  const observed = observePlacement(members);
  const writer = observed.primary;
  const answered = (id: MachineId) => {
    const member = members.find((each) => each.machine.id === id);
    return member?.answered === true ? member : null;
  };
  const server = (to: string, machine: MachineId | undefined) => {
    const found = members.find((member) => (machine === undefined ? member.machine.name === to : member.machine.id === machine));
    if (found === undefined) throw new UndraftedRun(`No server named ${to} holds ${name}.`);
    return found.machine;
  };
  const needsWriter = () => {
    if (writer === null) throw new UndraftedRun(`No server reports ${name}'s primary copy. Try again once one does.`);
    return writer;
  };
  switch (run.kind) {
    case "move": {
      const from = needsWriter();
      const to = server(run.args.to, run.args.machine);
      if (terms.running === null) throw new UndraftedRun(`No service on ${from.name} uses ${name}, so there's nothing to move.`);
      const mirrors = observed.copies.filter((copy) => copy.kind === "slot" && copy.role === "mirror" && copy.server.id !== to.id).map((copy) => copy.server.id);
      return {
        title: `Move ${name} to ${to.name}`,
        action: "Approve move",
        workflow: { kind: "move", from: from.id, to: to.id, mirrors, running: terms.running, ...(terms.recorded !== null && { recorded: terms.recorded }) },
        parties: [from.id, to.id, ...mirrors],
        command: `ployz volume move ${name} --to ${to.name}`,
      };
    }
    case "mirror": {
      const from = needsWriter();
      const to = server(run.args.to, run.args.machine);
      return {
        title: `Mirror ${name} to ${to.name}`,
        action: "Approve mirror",
        workflow: { kind: "mirror", writer: from.id, mirror: to.id, replace: false },
        parties: [from.id, to.id],
        command: `ployz volume mirror ${name} --to ${to.name}`,
      };
    }
    case "sync": {
      const from = needsWriter();
      const mirror = observed.copies.find((copy) => copy.kind === "slot" && copy.role === "mirror");
      if (!run.args.full || mirror === undefined) throw new UndraftedRun(`${name} has no mirror to sync in full.`);
      return {
        title: `Sync ${name} to ${mirror.server.name} in full`,
        action: "Approve sync",
        workflow: { kind: "mirror", writer: from.id, mirror: mirror.server.id, replace: true },
        parties: [from.id, mirror.server.id],
        command: `ployz volume sync ${name} --full`,
      };
    }
    case "restore": {
      const from = server(run.args.from, run.args.machine);
      const copy = answered(from.id)?.view.copy;
      if (copy === null || copy === undefined || copy.newest === null) throw new UndraftedRun(`${copyName(name, from.name)} holds no snapshot to restore.`);
      const lostAfter = copy.kind === "slot" ? copy.newest.created_unix_seconds * 1000 : null;
      return {
        title: `Restore ${name} on ${from.name}`,
        action: "Approve restore",
        workflow: { kind: "restore", machine: from.id, at: copy.newest.guid, ...(lostAfter !== null && { loses_after: lostAfter }) },
        parties: [from.id],
        command: `ployz volume restore ${name} --from ${from.name}`,
      };
    }
    case "delete_mirror": {
      if (run.args.slot === null && run.args.machine === undefined) throw new UndraftedRun(`This doesn't name which copy of ${name} to delete.`);
      const slot = server(run.args.slot ?? "", run.args.machine);
      const copy = answered(slot.id)?.view.copy;
      if (copy?.kind !== "slot" || copy.role === undefined) throw new UndraftedRun(`${slot.name} doesn't report what ${copyName(name, slot.name)} holds.`);
      return {
        title: copy.role === "old_copy" ? `Delete ${name}'s old copy on ${slot.name}` : `Delete ${name}'s mirror on ${slot.name}`,
        action: "Approve delete",
        workflow: { kind: "mirror_removal", mirror: slot.id, role: copy.role, ...(writer !== null && { writer: writer.id }) },
        parties: writer === null ? [slot.id] : [slot.id, writer.id],
        command: `ployz volume mirror rm ${copyName(name, slot.name)}`,
      };
    }
    case "resize": {
      const holding = members.flatMap((member) => (member.answered && member.view.copy !== null ? [member.machine.id] : []));
      const shrink = terms.recorded !== null && run.args.maximum_bytes < terms.recorded;
      return {
        title: `${shrink ? "Shrink" : "Grow"} ${name}`,
        action: `Approve ${shrink ? "shrink" : "grow"}`,
        workflow: { kind: "resize", machines: holding, maximum_bytes: run.args.maximum_bytes, shrink },
        parties: holding,
        command: `ployz volume set ${name} --size ${gigabytes(run.args.maximum_bytes)}`,
      };
    }
    case "release":
    case "seed":
      throw new UndraftedRun("The dashboard can't approve this. Run it from an approved device.");
  }
}
