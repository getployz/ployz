import type { MachineName } from "@ployz/sdk";
import { listNames } from "#/lib/plural";
import { gigabytes } from "#/modules/config-store/store-volumes";
import { type Requirement, requirements } from "#/modules/runtime/daemon-compat";
import type { Refusal } from "#/modules/volume-run/plan";
import type { AnsweredMember, VolumeRunArgs } from "#/modules/volume-run/volume-run";

/** Whole bytes, as `ProvisionedVolumeMaximumBytes` crosses the SDK. */
export type Bytes = number;

export const gb = (bytes: Bytes) => `${gigabytes(bytes)} GB`;

/** A copy's floor in whole GB, rounded up, so the size it names is one the copy accepts. */
export const gbAtLeast = (bytes: Bytes) => `${Math.ceil(bytes / 1_000_000_000)} GB`;

/** Which way a resize moves the Volume's bound; a settle asks every copy for the size the Store already records. */
export type SizeDirection = "grow" | "shrink" | "settle";

/**
 * How one direction keeps the Store's size at or below every copy's while the Machines change: `holders` orders
 * the copies the run sets, `record` places the one Store record before the first Machine, after the last, or nowhere, and
 * `requires` is what every holder must serve, or the run is refused before it asks any.
 */
type Order = {
  readonly holders: (writer: AnsweredMember, mirrors: readonly AnsweredMember[]) => readonly AnsweredMember[];
  readonly record: "first" | "last" | "none";
  readonly requires: readonly Requirement[];
};

export const RESIZE_ORDER = {
  // Mirrors first, the primary last, then the record: until every copy holds the size, the Store still says the old one.
  grow: { holders: (writer, mirrors) => [...mirrors, writer], record: "last", requires: [requirements.volumeSize] },
  // The record first, then the primary, whose floor decides, then the same size on each mirror. A mirror's floor reads
  // its own copy, which can lag the writer's, so a mirror never shrinks on its own.
  shrink: { holders: (writer, mirrors) => [writer, ...mirrors], record: "first", requires: [requirements.volumeSize, requirements.volumeShrink] },
  // The Store already records the size, so nothing is recorded. A settle only lowers a copy above it, a shrink, so the
  // primary goes first and every holder must serve the shrink.
  settle: { holders: (writer, mirrors) => [writer, ...mirrors], record: "none", requires: [requirements.volumeSize, requirements.volumeShrink] },
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
  /** Set when no Server holds a copy: the run only records the size the next Deploy creates the Volume at. */
  readonly unheld?: Unheld;
};

/** What a run that found no copy saw before it asked the Servers, which the Store's record checks: see `VolumeResized`. */
export type Unheld = { readonly latest_deployment: number | null };

/**
 * The Environment's Deployments as a resize read them before asking the Servers: the newest numbered one, and the
 * one in flight. Only a resize that finds no copy reads them.
 */
export type DeploymentsSeen = { readonly latest: number | null; readonly inFlight: { readonly number: number | null } | null };

const refuse = (code: Refusal["code"], message: string): Refusal => ({ code, message });

/** The direction from the recorded size to the asked one. */
export const sizeDirection = (from: Bytes, to: Bytes): SizeDirection => (to > from ? "grow" : to < from ? "shrink" : "settle");

/**
 * Why a shrink waits for a Deployment in flight: it claims its Volumes at the recorded size, and a copy lowered beneath
 * that claim fails its planner. The Store refuses the shrink's record in these words; the admission says them first.
 */
export const deployingMessage = (volumeName: string, number: number | null, to: Bytes) =>
  `${number === null ? "A Deployment" : `Deployment #${number}`} is in flight; run ployz volume set ${volumeName} --size ${gigabytes(to)} again when it ends`;

/** What every Server holding a copy must serve for a resize from `from` to `to`, or the run is refused before it asks any. */
export function resizeRequirements(from: Bytes, to: Bytes): readonly Requirement[] {
  return RESIZE_ORDER[sizeDirection(from, to)].requires;
}

/**
 * The size a Volume's copies hold, as the Store records it: `recorded` is 0 until a Deploy fixed one, and `working`
 * is 0 for a Docker Volume, which has no bound to change.
 */
export type RecordedSize = { readonly recorded: Bytes; readonly working: Bytes };

/**
 * The resize, read from one writer and its slots. The planner's shared checks ran first (every Server answered or
 * named what it lacks, one writer, no cut in progress), so here only the size decides. A settle only lowers: it reads
 * each holder's bound from the Runtime Watch frame and refuses when one sits below the recorded size, which a stale
 * replay of a grow's record leaves. The frame is a reading, not a lock, so a copy lowered below the record after it
 * would be grown by its daemon, under that daemon's own capacity check.
 */
export function planResize(
  volumeName: string,
  args: VolumeRunArgs["resize"],
  size: RecordedSize,
  writer: AnsweredMember,
  mirrors: readonly AnsweredMember[],
): { readonly ok: true; readonly phase: ResizePhase } | { readonly ok: false; readonly refusal: Refusal } {
  const unsized = unsizedRefusal(volumeName, size);
  if (unsized !== null) return { ok: false, refusal: unsized };
  const direction = sizeDirection(size.recorded, args.maximum_bytes);
  const order: Order = RESIZE_ORDER[direction];
  const holders = order.holders(writer, mirrors);
  if (direction === "settle") {
    const unobserved = holders.find((member) => member.boundBytes === undefined || member.boundBytes === null);
    if (unobserved !== undefined) {
      return { ok: false, refusal: refuse("unobserved", `Cloud has no fresh reading of ${volumeName} on ${unobserved.machine.name}; try again in a moment`) };
    }
    const lowest = Math.min(...holders.map((member) => member.boundBytes ?? size.recorded));
    if (lowest < size.recorded) {
      const below = holders.filter((member) => member.boundBytes === lowest).map((member) => member.machine.name);
      return { ok: false, refusal: refuse("held_below", `${listNames(below)} ${below.length === 1 ? "holds" : "hold"} ${volumeName} at ${gb(lowest)}, below its recorded ${gb(size.recorded)}. `
        + `Run ployz volume set ${volumeName} --size ${gigabytes(lowest)} to record the size it holds, then ployz volume set ${volumeName} --size ${gigabytes(size.recorded)} to grow it`) };
    }
  }
  const sets = holders.map((member): ResizeStep => ({ kind: "set", member, role: member === writer ? "primary" : "mirror" }));
  const record = { kind: "record" } as const;
  const steps = order.record === "first" ? [record, ...sets] : order.record === "last" ? [...sets, record] : sets;
  return { ok: true, phase: { kind: "resize", direction, from: size.recorded, to: args.maximum_bytes, steps } };
}

function unsizedRefusal(volumeName: string, size: RecordedSize): Refusal | null {
  if (size.recorded !== 0) return null;
  return size.working === 0
    ? refuse("invalid", `${volumeName} uses Docker storage, which has no size to change; add a Managed Volume for a sized one`)
    : refuse("invalid", `${volumeName} has no deployed size to change yet; deploy it first`);
}

/**
 * The resize of a Volume no Server holds: a Deploy fixed its size and then failed before creating it. Every managed
 * Server answered without a copy, so the run records the size alone and the next Deploy creates it at that size. A
 * Server that reports no managed storage might still hold a copy its daemon can't see, as when `zpool` is missing
 * while the pool is imported, so it refuses. So does a Deployment in flight, which may be creating the copy now.
 */
export function planUnheld(
  volumeName: string,
  args: VolumeRunArgs["resize"],
  size: RecordedSize,
  hidden: readonly MachineName[],
  deployments: DeploymentsSeen | undefined,
): { readonly ok: true; readonly phase: ResizePhase } | { readonly ok: false; readonly refusal: Refusal } {
  const unsized = unsizedRefusal(volumeName, size);
  if (unsized !== null) return { ok: false, refusal: unsized };
  if (hidden.length > 0) {
    return { ok: false, refusal: refuse("unanswered", `${listNames([...hidden])} ${hidden.length === 1 ? "reports" : "report"} no managed storage, so whether `
      + `${hidden.length === 1 ? "it holds" : "they hold"} ${volumeName} is unknown; change its size once ${listNames([...hidden])} ${hidden.length === 1 ? "reports its" : "report their"} storage, or after the next deploy creates it`) };
  }
  if (deployments === undefined) return { ok: false, refusal: refuse("unobserved", `Cloud couldn't read ${volumeName}'s Deployments; try again in a moment`) };
  if (deployments.inFlight !== null) return { ok: false, refusal: refuse("deploying", deployingMessage(volumeName, deployments.inFlight.number, args.maximum_bytes)) };
  const direction = sizeDirection(size.recorded, args.maximum_bytes);
  const steps: readonly ResizeStep[] = direction === "settle" ? [] : [{ kind: "record" }];
  return { ok: true, phase: { kind: "resize", direction, from: size.recorded, to: args.maximum_bytes, steps, unheld: { latest_deployment: deployments.latest } } };
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
