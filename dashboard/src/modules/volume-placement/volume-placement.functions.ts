import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { refusedPublicly } from "#/modules/config-store/store-refusal";
import { rustMachineIdSchema } from "#/lib/schema";
import { requireAccess } from "#/modules/organization/access.server";
import { deleteCopy, stageOffServer } from "#/modules/volume-placement/volume-placement.deploy.server";
import { discardPlacement, listPlacements, stagePlacement } from "#/modules/volume-placement/volume-placement.server";
import { SEED_OFF } from "#/modules/volume-run/plan";
import { DeleteCopyInput, StageRestoreInput } from "#/modules/volume-placement/volume-placement";
import { Conflict } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { VolumeInput } from "#/modules/volume-run/volume-run";

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
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => listPlacements(organization.id)),
  )));

export const stagePlacementServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(StageInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "changes:stage").pipe(
    Effect.flatMap(({ organization }) => stagePlacement(organization.id, data)),
    refusedPublicly,
  )));

export const discardPlacementServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "changes:stage").pipe(
    Effect.flatMap((access) => discardPlacement(access.organization.id, access, data.volumeId, data.environment)),
    refusedPublicly,
  )));

export const stageRestoreServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(StageRestoreInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "changes:stage").pipe(
    Effect.flatMap(() => Effect.fail(new Conflict({ userFacing: true, message: SEED_OFF }))),
    refusedPublicly,
  )));

export const stageOffServerServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...Organization.fields, server: Machine, accepting: Schema.Array(Machine), unrouted: Schema.optional(Schema.Array(Machine)) })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "changes:stage").pipe(
    Effect.flatMap(({ organization }) => stageOffServer(organization.id, data.server, data.accepting, data.unrouted)),
    refusedPublicly,
  )));

export const deleteCopyServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(DeleteCopyInput))
  .handler(({ context, data }) => runActor(context, refusedPublicly(deleteCopy(context.actor, data))));
