import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireAccess } from "#/modules/organization/access.server";
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
