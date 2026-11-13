import type { MachineId, ResolvedServiceSpec, Signed, VolumeCopy } from "@ployz/sdk";
import type { DraftWorkflow } from "@ployz/sdk/authority";
import { Option, Schema } from "effect";
import type { VolumeChange } from "#/modules/approval-session/volume-approval";
import { MAX_APPROVAL_BYTES, MAX_SERVICE_BYTES } from "#/modules/volume-run/plan";
import { copyName, type Member, type VolumeRunInput } from "#/modules/volume-run/volume-run";
import {
  type MachineRef, observePlacement, type PlacementChange, placementChanges, type PlacementRow, type PlacementVolume,
} from "./volume-placement";

/** The runs a staged change makes that a Server enforcing device approval takes only under an approval. */
export type ApprovedRunKind = "move" | "restore" | "mirror" | "delete_mirror" | "seed";

/**
 * A person's passkey answer to one Volume workflow of a staged placement, kept on the staged row until its Deploy runs
 * it: the run it covers, by kind and the Server it names, and for a Move the spec its target starts. Cloud reads it only
 * to attach it; each Server checks it.
 */
export type PlacementApproval = {
  readonly run: ApprovedRunKind;
  readonly machine: MachineId;
  readonly approval: Signed;
  readonly service?: ResolvedServiceSpec;
};

/** One workflow a staged change asks a passkey for, and the run it is kept for. */
export type PlacementAsk = { readonly run: ApprovedRunKind; readonly machine: MachineId; readonly change: VolumeChange };

/** What a change's workflows read beyond the Servers' answers: for a Move, the spec it runs and the size the Store records. */
export type PlacementTerms = { readonly running: ResolvedServiceSpec | null; readonly recorded: number | null };

const answered = (members: readonly Member[], id: MachineId) => {
  const member = members.find((each) => each.machine.id === id);
  return member?.answered === true ? member : null;
};

/** Whether the slot on `server` holds a snapshot; a slot with none holds no data, so its Server deletes it unasked. */
const holdsData = (members: readonly Member[], server: MachineRef) => {
  const copy = answered(members, server.id)?.view.copy;
  return copy?.kind === "slot" && copy.newest !== null;
};

/**
 * The Mirror removal of the slot on `server`, held as `role`, with the Server that writes the Volume or none. None for a
 * slot that holds no snapshot.
 */
function removal(name: string, members: readonly Member[], server: MachineRef, role: "mirror" | "old_copy"): PlacementAsk[] {
  if (!holdsData(members, server)) return [];
  const writer = observePlacement(members).primary;
  return [{
    run: "delete_mirror",
    machine: server.id,
    change: {
      title: role === "old_copy" ? `Delete ${name}'s old copy on ${server.name}` : `Delete ${name}'s mirror on ${server.name}`,
      action: "Approve delete",
      workflow: { kind: "mirror_removal", mirror: server.id, role, ...(writer !== null && { writer: writer.id }) },
      parties: writer === null ? [server.id] : [server.id, writer.id],
      command: `ployz volume mirror rm ${copyName(name, server.name)}`,
    },
  }];
}

/**
 * The Volume workflows `change` runs, each with the run it is kept for, in the order its Deploy runs them, as the CLI
 * drafts each one. `removing` names the mirrors the same Deploy removes first, which a Move's new writer no longer feeds.
 * Every workflow is drafted from the Servers' answers in `members`, so its approval names what the run will ask.
 */
export function placementAsks(
  name: string, change: PlacementChange, members: readonly Member[], terms: PlacementTerms, removing: readonly MachineId[] = [],
): PlacementAsk[] {
  switch (change.kind) {
    case "delete_copy":
      return removal(name, members, change.server, "old_copy");
    case "remove_mirror":
      return removal(name, members, change.server, "mirror");
    case "add_mirror": {
      const writer = observePlacement(members).primary;
      const clears = change.clears.flatMap((server) => removal(name, members, server, "old_copy"));
      if (writer === null) return clears;
      const workflow: DraftWorkflow = { kind: "mirror", writer: writer.id, mirror: change.server.id, replace: false };
      return [...clears, {
        run: "mirror",
        machine: change.server.id,
        change: {
          title: `Mirror ${name} to ${change.server.name}`,
          action: "Approve mirror",
          workflow,
          parties: [writer.id, change.server.id],
          command: `ployz volume mirror ${name} --to ${change.server.name}`,
        },
      }];
    }
    case "move": {
      const clears = change.clears.flatMap((server) => removal(name, members, server, "old_copy"));
      if (change.how === "restore") return [...clears, ...restore(name, change, members)];
      if (change.from === null || terms.running === null) return clears;
      const mirrors = observePlacement(members).copies
        .filter((copy) => copy.kind === "slot" && copy.role === "mirror" && copy.server.id !== change.to.id && !removing.includes(copy.server.id))
        .map((copy) => copy.server.id);
      return [...clears, {
        run: "move",
        machine: change.to.id,
        change: {
          title: `Move ${name} to ${change.to.name}`,
          action: "Approve move",
          workflow: {
            kind: "move", from: change.from.id, to: change.to.id, mirrors, running: terms.running,
            ...(terms.recorded !== null && { recorded: terms.recorded }),
          },
          parties: [change.from.id, change.to.id, ...mirrors],
          command: `ployz volume move ${name} --to ${change.to.name}`,
        },
      }];
    }
    case "seed":
      return [];
  }
}

/**
 * The Volume workflows a Deploy of `row`'s staged changes runs, in order, as `placementAsks` drafts each. Steps after a
 * Move are drafted over the copies as they stand once it lands, since its Deploy plans them then: the new writer feeds
 * each added mirror, and the copy the Move leaves behind is cleared first.
 */
export function stagedAsks(volume: PlacementVolume, members: readonly Member[], row: PlacementRow, terms: PlacementTerms): PlacementAsk[] {
  const changes = placementChanges(volume, members, row);
  const removing = changes.flatMap((change) => (change.kind === "remove_mirror" ? [change.server.id] : []));
  const move = changes.find((change) => change.kind === "move");
  const first = move === undefined ? changes : changes.filter((change) => change.kind !== "add_mirror");
  const asks = first.flatMap((change) => placementAsks(volume.name, change, members, terms, removing));
  if (move?.kind !== "move") return asks;
  const landed = afterMove(members, changes, move);
  const later = placementChanges(volume, landed, row).filter((change) => change.kind === "add_mirror");
  return [...asks, ...later.flatMap((change) => placementAsks(volume.name, change, landed, terms))];
}

/**
 * The copies once `move` and the removals before it ran: its target writes, the Server it leaves keeps an old copy, and
 * every copy deleted on the way holds nothing. A Move hands off at a final snapshot, so both ends hold one even when the
 * writer had none before; its name and guid are the Servers' to choose, and nothing drafted from them reads either.
 */
function afterMove(members: readonly Member[], changes: readonly PlacementChange[], move: Extract<PlacementChange, { kind: "move" }>): Member[] {
  const deleted = changes.flatMap((change) =>
    change.kind === "delete_copy" || change.kind === "remove_mirror" ? [change.server.id] : change.kind === "move" ? change.clears.map((server) => server.id) : []);
  const epoch = Math.max(0, ...members.map((member) => (member.answered && member.view.copy?.kind === "root" ? member.view.copy.epoch : 0))) + 1;
  const source = move.from === null ? null : answered(members, move.from.id)?.view.copy;
  const final = source?.newest ?? { name: "final", guid: "", created_unix_seconds: 0 };
  return members.map((member): Member => {
    if (!member.answered) return member;
    const copy = member.view.copy;
    const id = member.machine.id;
    if (id === move.to.id) return { ...member, view: { ...member.view, copy: { kind: "root", readonly: false, newest: final, epoch, seed: false } } };
    if (deleted.includes(id)) return { ...member, view: { ...member.view, copy: null } };
    if (id !== move.from?.id || copy === null) return member;
    // The slot's name is the Server's to choose; nothing drafted from it reads it.
    const old: VolumeCopy = { kind: "slot", slot: "old", readonly: true, newest: final, resume_token: null, pull: null, role: "old_copy" };
    return { ...member, view: { ...member.view, copy: old } };
  });
}

/** The Restore a "Use mirror" makes: the copy on its target becomes the writer at its newest snapshot. */
function restore(name: string, change: Extract<PlacementChange, { kind: "move" }>, members: readonly Member[]): PlacementAsk[] {
  const copy = answered(members, change.to.id)?.view.copy;
  if (copy === null || copy === undefined || copy.newest === null) return [];
  const lostAfter = copy.kind === "slot" ? copy.newest.created_unix_seconds * 1000 : null;
  return [{
    run: "restore",
    machine: change.to.id,
    change: {
      title: `Restore ${name} on ${change.to.name}`,
      action: "Approve restore",
      workflow: { kind: "restore", machine: change.to.id, at: copy.newest.guid, ...(lostAfter !== null && { loses_after: lostAfter }) },
      parties: [change.to.id],
      command: `ployz volume restore ${name} --from ${change.to.name}`,
    },
  }];
}

/**
 * The seed a restore into a new Volume runs on its source's primary, `source`: it creates the new Volume there from its
 * copy of the source at the restore point.
 */
export function seedAsk(change: Extract<PlacementChange, { kind: "seed" }>, sourceDocker: string, source: MachineId): PlacementAsk {
  return {
    run: "seed",
    machine: source,
    change: {
      title: `Restore ${change.volume.name} from ${change.source.name}`,
      action: "Approve restore",
      workflow: { kind: "seed", machine: source, source: sourceDocker, at: change.point.guid },
      parties: [source],
      command: "ployz device enforce --off",
    },
  };
}

/** The Server a run names, by the ID it pins or the name it asks for among `members`. */
function runServer(run: VolumeRunInput, members: readonly Member[]): MachineId | null {
  const named = (name: string) => members.find((member) => member.machine.name === name)?.machine.id ?? null;
  switch (run.kind) {
    case "move":
    case "mirror":
      return run.args.machine ?? named(run.args.to);
    case "restore":
      return run.args.machine ?? named(run.args.from);
    case "delete_mirror":
      return run.args.machine ?? (run.args.slot === null ? null : named(run.args.slot));
    default:
      return null;
  }
}

/**
 * `run` carrying the approval kept for it, if one is: the one for its kind and the Server it names, or for a seed the
 * one seed approval. Anything else runs as staged, and a Server that enforces refuses it.
 */
export function withApproval(run: VolumeRunInput, approvals: readonly PlacementApproval[] | undefined, members: readonly Member[]): VolumeRunInput {
  if (approvals === undefined || approvals.length === 0) return run;
  const server = runServer(run, members);
  const kept = approvals.find((each) => each.run === run.kind && (run.kind === "seed" || each.machine === server));
  if (kept === undefined) return run;
  switch (run.kind) {
    case "move":
      return { kind: "move", args: { ...run.args, approval: kept.approval, ...(kept.service !== undefined && { service: kept.service }) } };
    case "mirror":
      return { kind: "mirror", args: { ...run.args, approval: kept.approval } };
    case "restore":
      return { kind: "restore", args: { ...run.args, approval: kept.approval } };
    case "delete_mirror":
      return { kind: "delete_mirror", args: { ...run.args, approval: kept.approval } };
    case "seed":
      return { kind: "seed", args: { ...run.args, approval: kept.approval } };
    default:
      return run;
  }
}

const KeptBody = Schema.fromJsonString(Schema.Struct({
  volume: Schema.String,
  workflow: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("move"), to: Schema.String }),
    Schema.Struct({ kind: Schema.Literal("restore"), machine: Schema.String }),
    Schema.Struct({ kind: Schema.Literal("mirror"), mirror: Schema.String }),
    Schema.Struct({ kind: Schema.Literal("mirror_removal"), mirror: Schema.String }),
    Schema.Struct({ kind: Schema.Literal("seed"), machine: Schema.String }),
  ]),
}));

const RUN_OF = { move: "move", restore: "restore", mirror: "mirror", mirror_removal: "delete_mirror", seed: "seed" } as const;

/**
 * Whether `kept` is shaped for the run it is kept for on Docker Volume `volume`, in at most `MAX_APPROVAL_BYTES` and
 * `MAX_SERVICE_BYTES`: its workflow is that run's, names that Server, and for a Move comes with a spec. Cloud reads it
 * only to refuse a mistake before it keeps it; the Servers check the answer and every term.
 */
export function keptFor(kept: PlacementApproval, volume: string): boolean {
  const bytes = (value: Signed | ResolvedServiceSpec) => new TextEncoder().encode(JSON.stringify(value)).length;
  if (bytes(kept.approval) > MAX_APPROVAL_BYTES || (kept.service !== undefined && bytes(kept.service) > MAX_SERVICE_BYTES)) return false;
  if ((kept.run === "move") !== (kept.service !== undefined)) return false;
  const body = Schema.decodeUnknownOption(KeptBody)(kept.approval.body);
  if (Option.isNone(body) || body.value.volume !== volume) return false;
  const { workflow } = body.value;
  const named = workflow.kind === "move" ? workflow.to : workflow.kind === "restore" || workflow.kind === "seed" ? workflow.machine : workflow.mirror;
  return RUN_OF[workflow.kind] === kept.run && named === kept.machine;
}
