import "@tanstack/react-start/server-only";
import type { MachineId, SlotId, Snapshot } from "@ployz/sdk";
import { Effect } from "effect";
import { parseDockerVolumeName } from "#/modules/runtime/naming";
import { confirmedCopy, leftoverRm, mayHoldOnlyWrites } from "#/modules/volume-run/plan";
import type { ConfirmedCopy, Member, VolumeRunView } from "#/modules/volume-run/volume-run";
import { observeVolumeMembers } from "#/modules/volume-run/volume-run.cluster.server";
import { configVolumeNames, type CopyObserver, type NameInspector, observeOrganizationCopies, orphanSlotNames } from "#/modules/volume-run/volume-run.orphans.server";
import { insertLeftoverDelete } from "#/modules/volume-run/volume-run.rows.server";
import type { StoreRefusal } from "#/lib/refusal";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";

/**
 * A copy a deleted Volume left on a Server that the orphan sweep keeps, because its Server may hold writes no other
 * Server has or can't say. Only a person deletes one, after seeing it.
 */
export type LeftoverCopy = {
  readonly docker_volume: string;
  readonly server: string;
  readonly machine_id: MachineId;
  readonly slot: SlotId;
  readonly newest: Snapshot;
  /** Null when the Server's Ployz doesn't say. */
  readonly written_here: boolean | null;
  /** The most the copy holds, when the Server reports it. */
  readonly bound_bytes: number | null;
  /** What a delete names, so it deletes this copy as shown, and nothing replaced or written there since. */
  readonly confirm: string;
};

const WRITTEN = { true: "written", false: "copied", null: "unknown" } as const;

function leftover(dockerVolume: string, member: Member): LeftoverCopy | null {
  if (!member.answered || !mayHoldOnlyWrites(member) || member.view.copy?.kind !== "slot" || member.view.copy.newest === null) return null;
  const copy = confirmedCopy(member);
  if (copy === null) return null;
  return {
    docker_volume: dockerVolume,
    server: member.machine.name,
    machine_id: member.machine.id,
    slot: copy.slot,
    newest: member.view.copy.newest,
    written_here: copy.written_here,
    bound_bytes: member.boundBytes ?? null,
    confirm: `${copy.slot}-${copy.newest}-${WRITTEN[`${copy.written_here}`]}`,
  };
}

/** Every copy of a deleted Volume the orphan sweep keeps, by Docker Volume and Server. */
export const listLeftovers = Effect.fn("VolumeRun.listLeftovers")(function* (
  organizationId: string,
  observe: CopyObserver = observeOrganizationCopies,
  inspect: NameInspector = observeVolumeMembers,
) {
  const copies = yield* observe(organizationId);
  if (copies.length === 0) return [];
  const names = orphanSlotNames(copies, yield* configVolumeNames(organizationId));
  const found = yield* Effect.forEach(names, (name) => inspect(organizationId, name).pipe(
    Effect.map((members) => members.flatMap((member) => leftover(name, member) ?? [])),
    Effect.catch((error) => Effect.logWarning("Leftover inspect failed.", { organizationId, name, error }).pipe(Effect.as([]))),
  ), { concurrency: 4 });
  return found.flat();
});

const gb = (bytes: number) => `${Math.round(bytes / 1e8) / 10} GB`;

/** The copy as a person confirms it: where it is, its newest snapshot, its size and whether it may hold unique writes. */
export function describeLeftover(copy: LeftoverCopy) {
  const when = new Date(copy.newest.created_unix_seconds * 1000).toISOString().slice(0, 16).replace("T", " ");
  const size = copy.bound_bytes === null ? "" : `, up to ${gb(copy.bound_bytes)}`;
  const writes = copy.written_here === true ? "may hold writes no other Server has"
    : copy.written_here === false ? "holds writes copied from another Server" : "unknown whether it holds writes no other Server has";
  return `${copy.docker_volume} on ${copy.server}: slot ${copy.slot}, last snapshot ${copy.newest.name} (${when} UTC)${size}; ${writes}`;
}

type Deleted = { readonly ok: true; readonly run: VolumeRunView } | { readonly ok: false; readonly refusal: StoreRefusal };

const refused = (code: StoreRefusal["code"], message: string, details: StoreRefusal["details"] = null): Deleted =>
  ({ ok: false, refusal: { code, message, details } });

/**
 * Deletes the copy `dockerVolume` left on `server`, once `confirm` names it as its Server reports it now. Without it, or
 * once the copy changed, nothing is deleted and the refusal shows the copy and the command that deletes it.
 */
export const deleteLeftover = Effect.fn("VolumeRun.deleteLeftover")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly dockerVolume: string; readonly server: string; readonly confirm: string | null },
  inspect: NameInspector = observeVolumeMembers,
) {
  const { organizationId } = caller;
  const { dockerVolume, server } = input;
  const parsed = parseDockerVolumeName(dockerVolume);
  if (parsed === null) return refused("not_found", `Expected a Docker Volume name, as ployz volume leftovers lists it`);
  if ((yield* configVolumeNames(organizationId)).has(dockerVolume)) {
    return refused("invalid", `${dockerVolume} belongs to a Volume that still exists. Delete its copies with ployz volume mirror rm`);
  }
  const members = yield* inspect(organizationId, dockerVolume);
  const member = members.find((each) => each.machine.name === server);
  const found = member === undefined ? null : leftover(dockerVolume, member);
  if (member === undefined || found === null) {
    return refused("not_found", `${server} keeps no copy of ${dockerVolume}. ployz volume leftovers lists the kept copies`);
  }
  if (input.confirm !== found.confirm) {
    const next = `${leftoverRm(dockerVolume, server)} --confirm ${found.confirm}`;
    const changed = input.confirm === null ? "" : "It changed since you looked. ";
    return refused("confirmation_required", `${changed}${describeLeftover(found)}. Run ${next} to delete it`, { next, copy: found });
  }
  const machine = { id: found.machine_id, name: server };
  const copy: ConfirmedCopy = { machine: found.machine_id, slot: found.slot, newest: found.newest.guid, written_here: found.written_here };
  const confirmed = { by: "person" as const, copies: [copy] };
  const run = yield* insertLeftoverDelete({ ...caller, confirmed }, { environmentId: parsed.namespace, volumeId: parsed.volumeId, dockerVolume }, machine);
  if (run === null) return refused("conflict", `A run on ${dockerVolume} is under way. Try again once it ends`);
  return { ok: true, run } satisfies Deleted;
});

/** The copies deleted Volumes left on one Server that only a person deletes, for its page. */
export const listServerLeftovers = Effect.fn("VolumeRun.listServerLeftovers")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly machineId: MachineId },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const leftovers = yield* listLeftovers(organization.id).pipe(
    Effect.catch((error) => Effect.logWarning("Leftover list failed.", { organizationId: organization.id, error }).pipe(Effect.as([]))),
  );
  return leftovers.filter((copy) => copy.machine_id === input.machineId);
});
