import type { MachineId } from "@ployz/sdk";
import { Schema } from "effect";
import { gigabytes } from "#/modules/config-store/store-volumes";
import { MaximumBytes, VolumeApproval } from "#/modules/operations/cli-wire";
import { listNames } from "#/lib/plural";
import type { RuntimeMachineRecord, RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { serverSupports } from "#/modules/runtime/daemon-compat";
import { observePlacement, VolumeInput } from "#/modules/volume-placement/volume-placement";
import { type Bytes, gb, RESIZE_ORDER } from "#/modules/volume-run/resize";
import { type Member, parseDockerVolumeName } from "#/modules/volume-run/volume-run";

const GB = 1_000_000_000;

/**
 * A Server holding a copy, with what its machine record says and the bound its copy holds there; null fields are a
 * record or a bound Cloud hasn't seen.
 */
export type Holder = {
  readonly id: MachineId;
  readonly name: string;
  readonly role: "primary" | "mirror" | "old_copy";
  readonly capabilities: readonly string[] | null;
  readonly committableBytes: number | null;
  readonly boundBytes: number | null;
  /** Whether the Server takes a passkey's answer to a resize; null when it didn't answer. */
  readonly passkey: boolean | null;
};

/** A size typed for a deployed Volume. `holders` is null until the Servers have answered and their records are in. */
export type ResizeAsk = {
  readonly volume: string;
  readonly recorded: Bytes;
  readonly working: Bytes;
  readonly requested: Bytes;
  readonly holders: readonly Holder[] | null;
};

export type ResizeVerdict =
  | { readonly kind: "settled" }
  /** The size the Servers hold, over a restored revision that asks for less: staged back, not resized. */
  | { readonly kind: "restage" }
  | { readonly kind: "refused"; readonly message: string }
  /** `copies` is every copy the run grows, primary first, or null before the Servers answer. */
  | { readonly kind: "grow"; readonly from: Bytes; readonly to: Bytes; readonly copies: readonly Copy[] | null };

type Copy = Pick<Holder, "name" | "role">;
const READING_ORDER: readonly Holder["role"][] = ["primary", "mirror", "old_copy"];

export function checkResize({ volume, recorded, working, requested, holders }: ResizeAsk): ResizeVerdict {
  if (requested < recorded) return { kind: "refused", message: `${volume} is deployed at ${gb(recorded)}; this panel can't shrink it. Use ployz volume set ${volume} --size ${gigabytes(requested)}.` };
  if (requested === recorded) return working === recorded ? { kind: "settled" } : { kind: "restage" };
  if (holders === null) return { kind: "grow", from: recorded, to: requested, copies: null };
  if (!holders.some((holder) => holder.role === "primary")) {
    return { kind: "refused", message: `No server answers with ${volume}'s primary copy, so it can't grow now.` };
  }

  const unreported = holders.filter((holder) => holder.capabilities === null).map((holder) => holder.name);
  if (unreported.length > 0) {
    const one = unreported.length === 1;
    return { kind: "refused", message: `${listNames(unreported)} ${one ? "isn't" : "aren't"} reporting to Cloud, so growing can't check ${one ? "it" : "them"}.` };
  }
  const lacking = holders.filter((holder) =>
    RESIZE_ORDER.grow.requires.some((requirement) => serverSupports({ capabilities: holder.capabilities ?? [] }, requirement) !== true));
  if (lacking.length > 0) return { kind: "refused", message: `Growing needs a newer Ployz on ${lacking.map((holder) => holder.name).join(", ")}.` };

  const growing = READING_ORDER.flatMap((role) => holders.filter((holder) => holder.role === role));
  const lacksFromItsBound = (holder: Holder) => Math.max(0, requested - (holder.boundBytes ?? recorded));
  const [short] = growing.flatMap((holder) =>
    holder.committableBytes !== null && lacksFromItsBound(holder) > holder.committableBytes ? [{ name: holder.name, room: holder.committableBytes }] : []);
  if (short !== undefined) {
    const room = Math.floor(short.room / GB) * GB;
    return { kind: "refused", message: room === 0 ? `${short.name} has no room to grow it.` : `${short.name} has room to grow it by ${gb(room)} at most.` };
  }
  return { kind: "grow", from: recorded, to: requested, copies: growing.map(({ name, role }) => ({ name, role })) };
}

/** The copies of `volumeId` the Servers report, each with its machine record and its bound. Null until both are known. */
export function holdersOf(volumeId: string, members: readonly Member[] | undefined, machines: readonly RuntimeMachineRecord[] | null,
  copies: readonly RuntimeVolumeCopy[]): readonly Holder[] | null {
  if (members === undefined || machines === null) return null;
  const observed = observePlacement(members);
  const holder = (server: { readonly id: MachineId; readonly name: string }, role: Holder["role"]): Holder => {
    const machine = machines.find((candidate) => candidate.id === server.id);
    const copy = copies.find((candidate) => candidate.machineId === server.id && parseDockerVolumeName(candidate.name)?.volumeId === volumeId);
    const member = members.find((candidate) => candidate.machine.id === server.id);
    const answered = member?.answered === true ? member : null;
    return {
      id: server.id, name: server.name, role, capabilities: machine?.capabilities ?? null, committableBytes: machine?.poolCommittableBytes ?? null,
      boundBytes: copy?.boundBytes ?? null,
      passkey: answered === null ? null : answered.passkey === true,
    };
  };
  return [
    ...(observed.primary === null ? [] : [holder(observed.primary, "primary")]),
    ...observed.copies.map((copy) => holder(copy.server, copy.kind === "slot" ? copy.role : "mirror")),
  ];
}

/**
 * The drawer's confirmed grow; `requestId` is fresh per confirm, so a retried request admits one run. `approval` is the
 * person's passkey answer.
 */
export const ResizeVolumeInput = Schema.Struct({
  ...VolumeInput.fields,
  maximumBytes: MaximumBytes,
  requestId: Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{8,64}$/)),
  approval: Schema.optional(VolumeApproval),
});
