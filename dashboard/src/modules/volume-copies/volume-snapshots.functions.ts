import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireAccess } from "#/modules/organization/access.server";
import { POLICY_LIMITS } from "#/modules/volume-copies/snapshot-schedule";
import { readSnapshotPolicy, saveSnapshotPolicy } from "#/modules/volume-copies/snapshot-schedule.server";
import { listVolumeSnapshots, setVolumeSnapshot } from "#/modules/volume-copies/volume-snapshots.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Environment = Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) });
const VolumeInput = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString, environment: Environment });
const SnapshotId = Schema.String.check(Schema.isPattern(/^[a-z0-9-]{1,64}$/));
const Edit = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("take") }),
  Schema.Struct({ kind: Schema.Literal("keep"), id: SnapshotId, kept: Schema.Boolean }),
  Schema.Struct({ kind: Schema.Literal("delete"), id: SnapshotId }),
]);
const count = ([low, high]: readonly [number, number]) => Schema.Int.check(Schema.isBetween({ minimum: low, maximum: high }));
const Policy = Schema.Struct({
  before: Schema.Boolean,
  after: Schema.Boolean,
  keepDeploys: count(POLICY_LIMITS.keepDeploys),
  every: Schema.Literals(["hour", "day", "off"]),
  keep: count(POLICY_LIMITS.keep),
  capGb: count(POLICY_LIMITS.capGb),
});

export const listVolumeSnapshotsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => listVolumeSnapshots(organization.id, data.environment, data.volumeId)),
  )));

export const setVolumeSnapshotServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeInput.fields, expect: Schema.Number, edit: Edit })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => setVolumeSnapshot(organization.id, data.environment, data.volumeId, data.expect, data.edit)),
  )));

export const readSnapshotPolicyServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => readSnapshotPolicy(organization.id, data.volumeId)),
  )));

export const saveSnapshotPolicyServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeInput.fields, policy: Policy })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => saveSnapshotPolicy(organization.id, data.environment, data.volumeId, data.policy)),
  )));
