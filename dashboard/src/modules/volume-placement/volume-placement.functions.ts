import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { stageOffServer } from "#/modules/volume-placement/volume-placement.deploy.server";
import { discardPlacement, listPlacements, stagePlacement } from "#/modules/volume-placement/volume-placement.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Organization = Schema.Struct({ organizationSlug: Schema.NonEmptyString });
const Machine = rustMachineIdSchema;
const Placement = Schema.Struct({ primary: Machine, mirrors: Schema.Array(Machine), deletes: Schema.Array(Machine) });
const StageInput = Schema.Struct({
  ...Organization.fields,
  volumeId: Schema.NonEmptyString,
  environment: Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) }),
  staged: Schema.NullOr(Placement),
  mirrors: Schema.Array(Machine),
});

export const listPlacementsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Organization))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => listPlacements(organization.id)),
  )));

export const stagePlacementServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(StageInput))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => stagePlacement(organization.id, data)),
  )));

export const discardPlacementServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...Organization.fields, volumeId: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => discardPlacement(organization.id, data.volumeId)),
  )));

export const stageOffServerServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...Organization.fields, server: Machine, accepting: Schema.Array(Machine) })))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => stageOffServer(organization.id, data.server, data.accepting)),
  )));
