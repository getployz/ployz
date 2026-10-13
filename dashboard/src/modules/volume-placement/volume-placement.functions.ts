import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { requireAccess } from "#/modules/organization/access.server";
import { placementRefusal, stageOffServer, unlessPlacementRefused } from "#/modules/volume-placement/volume-placement.deploy.server";
import { deleteOldCopy, discardPlacement, listPlacements, stagePlacement, stageRestore } from "#/modules/volume-placement/volume-placement.server";
import { DeleteCopyInput, StageRestoreInput, VolumeInput } from "#/modules/volume-placement/volume-placement";
import { Conflict } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Organization = Schema.Struct({ organizationSlug: Schema.NonEmptyString });
const Machine = rustMachineIdSchema;
const Placement = Schema.Struct({ primary: Machine, mirrors: Schema.Array(Machine), deletes: Schema.Array(Machine) });
const StageInput = Schema.Struct({
  ...VolumeInput.fields,
  staged: Schema.NullOr(Placement),
  mirrors: Schema.Array(Machine),
});

export const listPlacementsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Organization))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => listPlacements(organization.id)),
  )));

export const stagePlacementServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(StageInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    // Clearing a staged placement starts no run, so it stays open while the gate is closed.
    Effect.flatMap(({ organization }) => data.staged === null
      ? stagePlacement(organization.id, data)
      : unlessPlacementRefused(organization.id, stagePlacement(organization.id, data))),
  )));

export const discardPlacementServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => discardPlacement(organization.id, context.actor.userId, data.volumeId, data.environment)),
  )));

export const stageRestoreServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(StageRestoreInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => unlessPlacementRefused(organization.id, stageRestore(organization.id, context.actor.userId, data))),
  )));

export const stageOffServerServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...Organization.fields, server: Machine, accepting: Schema.Array(Machine) })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => placementRefusal(organization.id).pipe(Effect.flatMap((refusal) => refusal === null
      ? stageOffServer(organization.id, data.server, data.accepting)
      : Effect.fail(new Conflict({ userFacing: true, message: refusal.message }))))),
  )));

export const deleteCopyServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(DeleteCopyInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => unlessPlacementRefused(organization.id, deleteOldCopy(organization.id, context.actor.userId, data))),
  )));
