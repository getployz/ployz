import "@tanstack/react-start/server-only";
import type { SetVolumeSizeRequest } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { storeSystem } from "#/modules/config-store/config-store.server";
import type { StoreRefusal, StoreRefused } from "#/modules/config-store/store.contract";
import { ClusterOutOfTime, clusterRead } from "#/modules/runtime/organization-runtime.server";
import { sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { type Bytes, deployingMessage, gb, gbAtLeast, type SizeDirection, type SizeOutcome, type Unheld } from "#/modules/volume-run/resize";
import { type MachineRef, openSession, requireOwner, type RunContext, SwitchAttemptError } from "#/modules/volume-run/volume-run.server";

/**
 * The refusal a `SetVolumeSizeRefused` carries in `RpcError.details`. `reason` is read as a plain string so a reason
 * this Cloud doesn't know stays a refusal, never a transport failure the step would retry forever.
 */
const Shortfall = Schema.Struct({ code: Schema.String, required_growth_bytes: Schema.optional(Schema.Number) });
const SizeRefusedDetails = Schema.Struct({
  details: Schema.Struct({
    reason: Schema.String,
    bound_bytes: Schema.optional(Schema.Number),
    capacity: Schema.optional(Shortfall),
    floor_bytes: Schema.optional(Schema.Number),
  }),
});
const decodeSizeRefused = Schema.decodeUnknownOption(SizeRefusedDetails);

/**
 * What a SetVolumeSize rejection means for a run that wants `machine`'s copy to hold `bytes`, or null when the error
 * carries no refusal, so the ask itself failed. In a grow, `shrink` with a bound at or above `bytes` holds: the copy
 * is already bigger, which is all a grow needs. In a shrink or a settle, `shrink` means the daemon ignored `allow_shrink`.
 * `switching` retries, as a copy step waits on `busy`. Every other reason is final and names the Server.
 */
export function sizeRefusalOf(volumeName: string, machine: MachineRef, bytes: Bytes, direction: SizeDirection, cause: unknown): SizeOutcome | null {
  const decoded = decodeSizeRefused(cause);
  if (Option.isNone(decoded)) return null;
  const { reason, bound_bytes: boundBytes, capacity, floor_bytes: floorBytes } = decoded.value.details;
  const refused = (message: string): SizeOutcome =>
    ({ kind: "refused", reason, message, boundBytes: boundBytes ?? null, capacity: capacity ?? null, floorBytes: floorBytes ?? null });
  switch (reason) {
    case "shrink":
      if (direction !== "grow") return refused(`${machine.name} needs a newer Ployz to shrink ${volumeName}`);
      return boundBytes !== undefined && boundBytes >= bytes ? { kind: "holds", boundBytes } : refused(`${machine.name} refused to resize ${volumeName} (${reason})`);
    case "below_floor":
      return refused(`${machine.name} can't shrink ${volumeName} to ${gb(bytes)}${floorBytes === undefined ? "" : `; its data needs at least ${gbAtLeast(floorBytes)}`}`);
    case "switching":
      return { kind: "retry" };
    case "no_capacity":
      return refused(`${machine.name} has no room to grow ${volumeName} to ${gb(bytes)}`);
    case "not_found":
      return refused(`${machine.name} no longer holds a copy of ${volumeName}`);
    case "not_provisioned":
      return refused(`${volumeName} on ${machine.name} has no size limit to change`);
    default:
      return refused(`${machine.name} refused to resize ${volumeName} (${reason})`);
  }
}

/**
 * One SetVolumeSize to `machine`. An equal size answers Ok with no write, so a copy already at `bytes` costs one
 * round trip and is never skipped on a read of its own. Only a failure to ask fails the Effect, so the step's own
 * retries cover the network. Only a shrink or a settle sends `allow_shrink`, so a grow's request is the one older daemons know.
 */
export const setVolumeSize = Effect.fn("VolumeRun.setVolumeSize")(function* (
  ctx: Pick<RunContext, "organizationId" | "dockerVolume" | "volumeName">,
  machine: MachineRef,
  bytes: Bytes,
  direction: SizeDirection,
) {
  const session = yield* openSession(ctx.organizationId);
  const payload: SetVolumeSizeRequest = { name: ctx.dockerVolume, maximum_bytes: bytes };
  if (direction !== "grow") payload.allow_shrink = true;
  return yield* session.volumeSwitch(machine.id, { command: "set_volume_size", payload }).pipe(
    clusterRead,
    Effect.map((reply): SizeOutcome => ({ kind: "holds", boundBytes: reply.bound_bytes })),
    Effect.catch((error): Effect.Effect<SizeOutcome, ClusterOutOfTime | SwitchAttemptError> => {
      if (error instanceof ClusterOutOfTime) return Effect.fail(error);
      if (error._tag === "ServerLacks") {
        return Effect.succeed<SizeOutcome>({ kind: "refused", reason: "lacks", message: `Volume resize needs a newer Ployz on ${machine.name}`, boundBytes: null, capacity: null, floorBytes: null });
      }
      const outcome = sizeRefusalOf(ctx.volumeName, machine, bytes, direction, "cause" in error ? error.cause : undefined);
      if (outcome === null) return Effect.fail(new SwitchAttemptError(null, `set_volume_size on ${machine.name}: ${sdkFailureMessage(error, [machine])}`));
      return Effect.succeed(outcome);
    }),
  );
}, Effect.scoped);

/** The resize step's own ask: the owner check, then SetVolumeSize, so a run whose row was taken sends nothing. */
export const resizeCopy = Effect.fn("VolumeRun.resizeCopy")(function* (
  ctx: RunContext,
  inngestRunId: string,
  machine: MachineRef,
  bytes: Bytes,
  direction: SizeDirection,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* setVolumeSize(ctx, machine, bytes, direction);
});

/**
 * What the Store said to the record, plain JSON for the step. `changed` and `gone` are final: the Store's size is no
 * longer `from`, or the Volume is no longer deployed.
 */
export type RecordAnswer =
  | { readonly ok: true }
  | { readonly ok: false; readonly reason: "changed" | "gone" | "deployed_since"; readonly message: string };

const Recorded = Schema.Struct({ recorded: Schema.Union([Schema.Struct({ kind: Schema.Literal("provisioned"), maximumBytes: Schema.Number }), Schema.Struct({ kind: Schema.Literal("docker") })]) });
const decodeRecorded = Schema.decodeUnknownOption(Recorded);
const Deploying = Schema.Struct({ reason: Schema.Literal("deploying"), deployment: Schema.Struct({ number: Schema.NullOr(Schema.Number) }), unheld: Schema.optional(Schema.Boolean) });
const decodeDeploying = Schema.decodeUnknownOption(Deploying);

/**
 * What the Store's refusal of a record from `record.from` to `record.to` means for the run, or null when it is no answer
 * the record ends on. A shrink's record waits for a Deployment in flight, so the run names it and asks again after.
 */
export function recordRefusalOf(volumeName: string, record: SizeRecord, refusal: Pick<StoreRefusal, "code" | "details">): RecordAnswer | null {
  if (refusal.code === "not_found") return { ok: false, reason: "gone", message: `${volumeName} is no longer deployed, so its size was not recorded` };
  if (refusal.code !== "conflict") return null;
  const deploying = decodeDeploying(refusal.details);
  if (Option.isSome(deploying) && deploying.value.unheld === true) {
    const { number } = deploying.value.deployment;
    return { ok: false, reason: "deployed_since", message: `${number === null ? "a Deployment" : `Deployment #${number}`} may have created ${volumeName} at ${gb(record.from)}; change its size again in a few minutes` };
  }
  if (Option.isSome(deploying)) return { ok: false, reason: "changed", message: deployingMessage(volumeName, deploying.value.deployment.number, record.to) };
  const recorded = decodeRecorded(refusal.details);
  const holds = Option.isSome(recorded)
    ? recorded.value.recorded.kind === "provisioned" ? `records ${volumeName} at ${gb(recorded.value.recorded.maximumBytes)}, not ${gb(record.from)}` : `records ${volumeName} as a Docker Volume`
    : `no longer records ${volumeName} at ${gb(record.from)}`;
  return { ok: false, reason: "changed", message: `the Store ${holds}; read its size again` };
}

/** One record of a Volume's size: `unheld` when no Server holds a copy, so it sets only the size the next Deploy creates. */
export type SizeRecord = { readonly from: Bytes; readonly to: Bytes; readonly unheld?: Unheld | undefined };

/**
 * Tell the Store every copy holds `to`, or, `unheld`, that no Server holds one: Cloud's own observation, in-process,
 * so no user, token or agent can claim a size no Machine applied. A record the Store already holds answers ok, so a replayed step converges. Only the
 * Store's own answers end the record; a Store that couldn't be asked fails the step, and Inngest asks again.
 */
export const recordVolumeSize = Effect.fn("VolumeRun.recordVolumeSize")(function* (
  ctx: RunContext,
  inngestRunId: string,
  record: SizeRecord,
) {
  yield* requireOwner(ctx.id, inngestRunId);
  const unheld = record.unheld === undefined ? {} : { unheld: record.unheld };
  return yield* storeSystem(ctx.organizationId, { event: "volume_resized", environment: ctx.environmentId, volume: ctx.volumeId, from: record.from, to: record.to, ...unheld }).pipe(
    Effect.map((): RecordAnswer => ({ ok: true })),
    Effect.catchTag("StoreRefused", (refusal): Effect.Effect<RecordAnswer, StoreRefused> => {
      const answer = recordRefusalOf(ctx.volumeName, record, refusal);
      return answer === null ? Effect.fail(refusal) : Effect.succeed(answer);
    }),
  );
});
