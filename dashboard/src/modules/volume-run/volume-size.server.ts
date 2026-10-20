import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { storeSystem } from "#/modules/config-store/config-store.server";
import { ClusterOutOfTime, clusterRead } from "#/modules/runtime/organization-runtime.server";
import { sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { type Bytes, gb, type PoolShortfall, type SizeDirection, type SizeOutcome } from "#/modules/volume-run/resize";
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
  }),
});
const decodeSizeRefused = Schema.decodeUnknownOption(SizeRefusedDetails);

const lacked = (capacity: PoolShortfall | undefined) =>
  capacity?.required_growth_bytes === undefined ? "" : ` (it lacks ${gb(capacity.required_growth_bytes)})`;

/**
 * What a SetVolumeSize rejection means for a run that wants `machine`'s copy to hold at least `bytes`, or null when
 * the error carries no refusal, so the ask itself failed. `shrink` with a bound at or above `bytes` holds: the copy is
 * already bigger, which is all a grow needs. `switching` retries, as a copy step waits on `busy`. Every other reason
 * is final and names the Server.
 */
export function sizeRefusalOf(volumeName: string, machine: MachineRef, bytes: Bytes, cause: unknown): SizeOutcome | null {
  const decoded = decodeSizeRefused(cause);
  if (Option.isNone(decoded)) return null;
  const { reason, bound_bytes: boundBytes, capacity } = decoded.value.details;
  const refused = (message: string): SizeOutcome => ({ kind: "refused", reason, message, boundBytes: boundBytes ?? null, capacity: capacity ?? null });
  switch (reason) {
    case "shrink":
      return boundBytes !== undefined && boundBytes >= bytes ? { kind: "holds", boundBytes } : refused(`${machine.name} refused to resize ${volumeName} (${reason})`);
    case "switching":
      return { kind: "retry" };
    case "no_capacity":
      return refused(`${machine.name} has no room to grow ${volumeName} to ${gb(bytes)}${lacked(capacity)}`);
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
 * retries cover the network. `direction` is what a shrink will read `holds` by; a grow sends nothing for it.
 */
export const setVolumeSize = Effect.fn("VolumeRun.setVolumeSize")(function* (
  ctx: Pick<RunContext, "organizationId" | "dockerVolume" | "volumeName">,
  machine: MachineRef,
  bytes: Bytes,
  _direction: SizeDirection,
) {
  const session = yield* openSession(ctx.organizationId);
  return yield* session.volumeSwitch(machine.id, { command: "set_volume_size", payload: { name: ctx.dockerVolume, maximum_bytes: bytes } }).pipe(
    clusterRead,
    Effect.map((reply): SizeOutcome => ({ kind: "holds", boundBytes: reply.bound_bytes })),
    Effect.catch((error): Effect.Effect<SizeOutcome, ClusterOutOfTime | SwitchAttemptError> => {
      if (error instanceof ClusterOutOfTime) return Effect.fail(error);
      if (error._tag === "ServerLacks") {
        return Effect.succeed<SizeOutcome>({ kind: "refused", reason: "lacks", message: `Volume resize needs a newer Ployz on ${machine.name}`, boundBytes: null, capacity: null });
      }
      const outcome = sizeRefusalOf(ctx.volumeName, machine, bytes, "cause" in error ? error.cause : undefined);
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
  | { readonly ok: false; readonly reason: "changed" | "gone"; readonly message: string };

const Recorded = Schema.Struct({ recorded: Schema.Union([Schema.Struct({ kind: Schema.Literal("provisioned"), maximum_bytes: Schema.Number }), Schema.Struct({ kind: Schema.Literal("docker") })]) });
const decodeRecorded = Schema.decodeUnknownOption(Recorded);

/**
 * Tell the Store every copy holds `to`: Cloud's own observation, in-process, so no user, token or agent can claim a
 * size no Machine applied. A record the Store already holds answers ok, so a replayed step converges.
 */
export const recordVolumeSize = Effect.fn("VolumeRun.recordVolumeSize")(function* (
  ctx: RunContext,
  inngestRunId: string,
  record: { readonly from: Bytes; readonly to: Bytes },
) {
  yield* requireOwner(ctx.id, inngestRunId);
  return yield* storeSystem(ctx.organizationId, { event: "volume_resized", environment: ctx.environmentId, volume: ctx.volumeId, from: record.from, to: record.to }).pipe(
    Effect.map((): RecordAnswer => ({ ok: true })),
    Effect.catchTag("StoreRefused", (refusal): Effect.Effect<RecordAnswer> => {
      if (refusal.code === "not_found") return Effect.succeed({ ok: false, reason: "gone", message: `${ctx.volumeName} is no longer deployed, so its size was not recorded` });
      const recorded = decodeRecorded(refusal.details);
      const holds = Option.isSome(recorded)
        ? recorded.value.recorded.kind === "provisioned" ? `records ${ctx.volumeName} at ${gb(recorded.value.recorded.maximum_bytes)}, not ${gb(record.from)}` : `records ${ctx.volumeName} as a Docker Volume`
        : `no longer records ${ctx.volumeName} at ${gb(record.from)}`;
      return Effect.succeed({ ok: false, reason: "changed", message: `the Store ${holds}; read its size again` });
    }),
  );
});
