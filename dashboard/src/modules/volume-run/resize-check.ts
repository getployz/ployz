import { Schema } from "effect";
import { MaximumBytes } from "#/modules/operations/cli-wire";
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
  readonly name: string;
  readonly role: "primary" | "mirror" | "old_copy";
  readonly capabilities: readonly string[] | null;
  readonly committableBytes: number | null;
  readonly boundBytes: number | null;
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
const ROLE_ORDER: readonly Holder["role"][] = ["primary", "mirror", "old_copy"];

/**
 * What the drawer does with a typed size, checked as the run checks it so the input refuses what the run would. The
 * room is a hint the run doesn't read, so an unknown one refuses nothing and the Server's own answer decides.
 */
export function checkResize({ volume, recorded, working, requested, holders }: ResizeAsk): ResizeVerdict {
  if (requested < recorded) return { kind: "refused", message: `${volume} is deployed at ${gb(recorded)}; a deployed volume can't shrink yet.` };
  if (requested === recorded) return working === recorded ? { kind: "settled" } : { kind: "restage" };
  if (holders === null) return { kind: "grow", from: recorded, to: requested, copies: null };
  if (!holders.some((holder) => holder.role === "primary")) {
    return { kind: "refused", message: `No server answers with ${volume}'s primary copy, so it can't grow now.` };
  }

  // The run asks every copy, old ones included. A Server with no record can't be checked, and an empty list claims no verb.
  const unreported = holders.filter((holder) => holder.capabilities === null).map((holder) => holder.name);
  if (unreported.length > 0) {
    const one = unreported.length === 1;
    return { kind: "refused", message: `${listNames(unreported)} ${one ? "isn't" : "aren't"} reporting to Cloud, so growing can't check ${one ? "it" : "them"}.` };
  }
  const lacking = holders.filter((holder) =>
    RESIZE_ORDER.grow.requires.some((requirement) => serverSupports({ capabilities: holder.capabilities ?? [] }, requirement) !== true));
  if (lacking.length > 0) return { kind: "refused", message: `Growing needs a newer Ployz on ${lacking.map((holder) => holder.name).join(", ")}.` };

  // The run grows every slot, an old copy included, so each one's Pool must take what its own bound lacks: a copy an
  // earlier grow left at the size needs nothing.
  const growing = ROLE_ORDER.flatMap((role) => holders.filter((holder) => holder.role === role));
  const [short] = growing.flatMap(({ name, committableBytes, boundBytes }) =>
    committableBytes !== null && Math.max(0, requested - (boundBytes ?? recorded)) > committableBytes ? [{ name, room: committableBytes }] : []);
  if (short !== undefined) {
    const room = Math.floor(short.room / GB) * GB;
    return { kind: "refused", message: room === 0 ? `${short.name} has no room to grow it.` : `${short.name} has room to grow it by ${gb(room)} at most.` };
  }
  return { kind: "grow", from: recorded, to: requested, copies: growing.map(({ name, role }) => ({ name, role })) };
}

/**
 * What the confirm says a grow does. It names the Servers in reading order, the primary's first, then its mirrors' and
 * old copies'; the run itself grows the mirrors first.
 */
export function growWords(volume: string, { from, to, copies }: Extract<ResizeVerdict, { kind: "grow" }>): string {
  const named = (role: Holder["role"]) => (copies ?? []).filter((copy) => copy.role === role).map((copy) => copy.name);
  const [primary] = named("primary");
  const others = [["mirror", named("mirror")], ["old copy", named("old_copy")]] as const;
  const rest = others.flatMap(([noun, names]) => names.length === 0 ? [] : [`its ${noun}${names.length === 1 ? "" : "s"} on ${listNames(names)}`]);
  const where = copies === null || primary === undefined ? "on every server that holds a copy"
    : `on ${listNames([primary, ...rest])}`;
  return `Grow ${volume} from ${gb(from)} to ${gb(to)} ${where}. Its services keep running.`;
}

/** The copies of `volumeId` the Servers report, each with its machine record and its bound. Null until both are known. */
export function holdersOf(volumeId: string, members: readonly Member[] | undefined, machines: readonly RuntimeMachineRecord[] | null,
  copies: readonly RuntimeVolumeCopy[]): readonly Holder[] | null {
  if (members === undefined || machines === null) return null;
  const observed = observePlacement(members);
  const holder = (server: { readonly id: string; readonly name: string }, role: Holder["role"]): Holder => {
    const machine = machines.find((candidate) => candidate.id === server.id);
    const copy = copies.find((candidate) => candidate.machineId === server.id && parseDockerVolumeName(candidate.name)?.volumeId === volumeId);
    return {
      name: server.name, role, capabilities: machine?.capabilities ?? null, committableBytes: machine?.poolCommittableBytes ?? null,
      boundBytes: copy?.boundBytes ?? null,
    };
  };
  return [
    ...(observed.primary === null ? [] : [holder(observed.primary, "primary")]),
    ...observed.copies.map((copy) => holder(copy.server, copy.kind === "slot" ? copy.role : "mirror")),
  ];
}

/** The drawer's confirmed grow; `requestId` is fresh per confirm, so a retried request admits one run. */
export const ResizeVolumeInput = Schema.Struct({
  ...VolumeInput.fields,
  maximumBytes: MaximumBytes,
  requestId: Schema.String.check(Schema.isPattern(/^[A-Za-z0-9_-]{8,64}$/)),
});
