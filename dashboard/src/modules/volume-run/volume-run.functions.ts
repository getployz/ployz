import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { inspectVolumeCopies, listActiveVolumeRuns } from "#/modules/volume-run/volume-run.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const VolumeRef = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString });

const VolumeCopiesInput = Schema.Struct({
  ...VolumeRef.fields,
  environment: Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) }),
});

export const inspectVolumeCopiesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeCopiesInput))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => inspectVolumeCopies(organization.id, data.environment, data.volumeId)),
  )));

export const listActiveVolumeRunsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => listActiveVolumeRuns(organization.id)),
  )));
