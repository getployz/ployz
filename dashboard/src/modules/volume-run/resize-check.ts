import { Schema } from "effect";
import { MaximumBytes } from "#/modules/operations/cli-wire";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { serverSupports } from "#/modules/runtime/daemon-compat";
import { observePlacement, VolumeInput } from "#/modules/volume-placement/volume-placement";
import { type Bytes, gb, RESIZE_ORDER } from "#/modules/volume-run/resize";
import type { Member } from "#/modules/volume-run/volume-run";

const GB = 1_000_000_000;

/** A Server holding a copy, with what its machine record says; null fields are a record Cloud hasn't seen. */
export type Holder = {
  readonly name: string;
  readonly role: "primary" | "mirror" | "old_copy";
  readonly capabilities: readonly string[] | null;
  readonly committableBytes: number | null;
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
  | { readonly kind: "grow"; readonly from: Bytes; readonly to: Bytes; readonly servers: readonly string[] | null };

/**
 * What the drawer does with a typed size, checked as the run checks it so the input refuses what the run would. The
 * room is a hint the run doesn't read, so an unknown one refuses nothing and the Server's own answer decides.
 */
export function checkResize({ volume, recorded, working, requested, holders }: ResizeAsk): ResizeVerdict {
  if (requested < recorded) return { kind: "refused", message: `${volume} is deployed at ${gb(recorded)}; a deployed volume can't shrink yet.` };
  if (requested === recorded) return working === recorded ? { kind: "settled" } : { kind: "restage" };
  if (holders === null) return { kind: "grow", from: recorded, to: requested, servers: null };

  // The run asks every copy, old ones included, and a record listing no capabilities may not serve the verb.
  const lacking = holders.filter((holder) => holder.capabilities === null
    || RESIZE_ORDER.grow.requires.some((requirement) => serverSupports({ capabilities: holder.capabilities ?? [] }, requirement) !== true));
  if (lacking.length > 0) return { kind: "refused", message: `Growing needs a newer Ployz on ${lacking.map((holder) => holder.name).join(", ")}.` };

  const serving = [...holders.filter((holder) => holder.role === "primary"), ...holders.filter((holder) => holder.role === "mirror")];
  const [tightest] = serving.flatMap((holder) => holder.committableBytes === null ? [] : [{ name: holder.name, room: holder.committableBytes }])
    .sort((a, b) => a.room - b.room);
  if (tightest !== undefined && requested - recorded > tightest.room) {
    const room = Math.floor(tightest.room / GB) * GB;
    return { kind: "refused", message: room === 0 ? `${tightest.name} has no room to grow it.` : `${tightest.name} has room to grow it by ${gb(room)} at most.` };
  }
  return { kind: "grow", from: recorded, to: requested, servers: serving.map((holder) => holder.name) };
}

/** The copies the Servers report, each with its machine record. Null until both are known. */
export function holdersOf(members: readonly Member[] | undefined, machines: readonly RuntimeMachineRecord[] | null): readonly Holder[] | null {
  if (members === undefined || machines === null) return null;
  const observed = observePlacement(members);
  const holder = (server: { readonly id: string; readonly name: string }, role: Holder["role"]): Holder => {
    const machine = machines.find((candidate) => candidate.id === server.id);
    return { name: server.name, role, capabilities: machine?.capabilities ?? null, committableBytes: machine?.poolCommittableBytes ?? null };
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
