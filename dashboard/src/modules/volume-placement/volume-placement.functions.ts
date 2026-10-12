import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { requireAccess } from "#/modules/organization/access.server";
import { placementRefusal, stageOffServer, unlessPlacementRefused } from "#/modules/volume-placement/volume-placement.deploy.server";
import { discardPlacement, listPlacements, stagePlacement, stageRestore } from "#/modules/volume-placement/volume-placement.server";
import { planVolumeRun, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import { Conflict } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Organization = Schema.Struct({ organizationSlug: Schema.NonEmptyString });
const Machine = rustMachineIdSchema;
const Placement = Schema.Struct({ primary: Machine, mirrors: Schema.Array(Machine), deletes: Schema.Array(Machine) });
const Environment = Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) });
const VolumeInput = Schema.Struct({ ...Organization.fields, volumeId: Schema.NonEmptyString, environment: Environment });
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

/** `at` is the restore point's time in Unix seconds. */
export const stageRestoreServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeInput.fields, guid: Schema.NonEmptyString, name: Schema.NonEmptyString, at: Schema.Number })))
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

/**
 * Deletes an old copy now, not on Deploy: the user's confirm is the approval. Planned first, so a refusal (the copy is
 * the only one, or a run holds the Volume) returns before any run exists.
 */
export const deleteCopyServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeInput.fields, server: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => unlessPlacementRefused(organization.id, Effect.gen(function* () {
      const run = { volumeId: data.volumeId, environment: data.environment, kind: "delete_mirror", args: { slot: data.server, confirmed_name: null } } as const;
      const planned = yield* planVolumeRun(organization.id, run);
      if (!planned.ok) return { ok: false, refusal: { message: planned.refusal.message } } as const;
      const requested = yield* requestVolumeRun({ userId: context.actor.userId, organizationId: organization.id }, run);
      return requested.ok ? { ok: true } as const : { ok: false, refusal: { message: requested.refusal.message } } as const;
    }))),
  )));
