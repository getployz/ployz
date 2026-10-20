import { gigabytes } from "#/modules/config-store/store-volumes";
import { type Requirement, requirements } from "#/modules/runtime/daemon-compat";
import type { Refusal } from "#/modules/volume-run/plan";
import type { AnsweredMember, VolumeRunArgs } from "#/modules/volume-run/volume-run";

/** Whole bytes, as `ProvisionedVolumeMaximumBytes` crosses the SDK. */
export type Bytes = number;

export const gb = (bytes: Bytes) => `${gigabytes(bytes)} GB`;

/** Which way a resize moves the Volume's bound. */
export type SizeDirection = "grow" | "shrink";

/**
 * How one direction keeps the Store's size at or below every copy's while the Machines change: `holders` orders
 * the copies the run sets, `record` places the one Store record before the first Machine or after the last, and
 * `requires` is what every holder must serve, or the run is refused before it asks any.
 */
type Order = {
  readonly holders: (writer: AnsweredMember, mirrors: readonly AnsweredMember[]) => readonly AnsweredMember[];
  readonly record: "first" | "last";
  readonly requires: readonly Requirement[];
};

export const RESIZE_ORDER = {
  // Mirrors first, the primary last, then the record: until every copy holds the size, the Store still says the old one.
  grow: { holders: (writer, mirrors) => [...mirrors, writer], record: "last", requires: [requirements.volumeSize] },
  // The record first, then the primary, whose floor decides, then the same size on each mirror. A mirror's floor reads
  // its own copy, which can lag the writer's, so a mirror never shrinks on its own.
  shrink: { holders: (writer, mirrors) => [writer, ...mirrors], record: "first", requires: [requirements.volumeSize, requirements.volumeShrink] },
} as const satisfies Record<SizeDirection, Order>;

/** One thing the run does, in order: ask one Machine for the size, or tell the Store every copy holds it. */
export type ResizeStep =
  | { readonly kind: "set"; readonly member: AnsweredMember; readonly role: "mirror" | "primary" }
  | { readonly kind: "record" };

/** Plain JSON: the run replays it from `planFromCopies` on every attempt. */
export type ResizePhase = {
  readonly kind: "resize";
  readonly direction: SizeDirection;
  /** The size the Store recorded when the run was requested, which the record compares against. */
  readonly from: Bytes;
  readonly to: Bytes;
  readonly steps: readonly ResizeStep[];
};

const refuse = (code: Refusal["code"], message: string): Refusal => ({ code, message });

/** The direction from the recorded size to the asked one, or null for an equal size, which is settled. */
export const sizeDirection = (from: Bytes, to: Bytes): SizeDirection | null => (to > from ? "grow" : to < from ? "shrink" : null);

/** What every Server holding a copy must serve for a resize from `from` to `to`, or the run is refused before it asks any. */
export function resizeRequirements(from: Bytes, to: Bytes): readonly Requirement[] {
  const direction = sizeDirection(from, to);
  return direction === null ? [] : RESIZE_ORDER[direction].requires;
}

/**
 * The size a Volume's copies hold, as the Store records it: `recorded` is 0 until a Deploy fixed one, and `working`
 * is 0 for a Docker Volume, which has no bound to change.
 */
export type RecordedSize = { readonly recorded: Bytes; readonly working: Bytes };

/**
 * The resize, read from one writer and its slots. The planner's shared checks ran first (every Server answered or
 * named what it lacks, one writer, no cut in progress), so here only the size decides.
 */
export function planResize(
  volumeName: string,
  args: VolumeRunArgs["resize"],
  size: RecordedSize,
  writer: AnsweredMember,
  mirrors: readonly AnsweredMember[],
): { readonly ok: true; readonly phase: ResizePhase } | { readonly ok: false; readonly refusal: Refusal } {
  if (size.recorded === 0) {
    return { ok: false, refusal: size.working === 0
      ? refuse("invalid", `${volumeName} uses Docker storage, which has no size to change; add a Managed Volume for a sized one`)
      : refuse("invalid", `${volumeName} has no deployed size to change yet; deploy it first`) };
  }
  const direction = sizeDirection(size.recorded, args.maximum_bytes);
  if (direction === null) return { ok: false, refusal: refuse("already_sized", `${volumeName} already has ${gb(args.maximum_bytes)}`) };
  const order: Order = RESIZE_ORDER[direction];
  const sets = order.holders(writer, mirrors).map((member): ResizeStep => ({ kind: "set", member, role: member === writer ? "primary" : "mirror" }));
  const steps = order.record === "first" ? [{ kind: "record" } as const, ...sets] : [...sets, { kind: "record" } as const];
  return { ok: true, phase: { kind: "resize", direction, from: size.recorded, to: args.maximum_bytes, steps } };
}

/**
 * The reasons a Machine refuses SetVolumeSize with, plus the run's own: `lacks` for a daemon without the verb and
 * `read_only` for one that kept the copy switching past the run's patience. A reason this Cloud doesn't know is kept
 * as the Machine sent it, so a newer daemon's refusal is final rather than retried.
 */
export type SizeRefusalReason = "switching" | "no_capacity" | "not_found" | "not_provisioned" | "shrink" | "below_floor" | "lacks" | "read_only" | (string & {});

/**
 * What one Machine's answer means for the run. `holds` lets the walk go on; `retry` asks again with the same request;
 * `refused` ends the run with `message`, which names the Server.
 */
export type SizeOutcome =
  | { readonly kind: "holds"; readonly boundBytes: Bytes }
  | { readonly kind: "retry" }
  | {
    readonly kind: "refused";
    readonly reason: SizeRefusalReason;
    readonly message: string;
    readonly boundBytes: Bytes | null;
    readonly capacity: PoolShortfall | null;
    /** The smallest size the copy accepts, on a `below_floor` refusal. */
    readonly floorBytes: Bytes | null;
  };

/** What a Pool lacked on a `no_capacity` refusal, as `StorageCapacityError` names it; only an `insufficient_storage` one sizes it. */
export type PoolShortfall = { readonly code: string; readonly required_growth_bytes?: number };

/**
 * Whether a refusal undoes a record already made: only while no copy has answered `holds`. Once one has, some copy
 * may hold the new size and none holds less, so the Store keeps it. A grow records last, so it never undoes.
 */
export function undoOnRefusal(stepsDone: readonly ("recorded" | "held")[]): boolean {
  return stepsDone.includes("recorded") && !stepsDone.includes("held");
}
