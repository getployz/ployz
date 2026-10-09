import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { listVolumeRuns, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const VolumeRef = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString });
const Server = Schema.NonEmptyString;

/** What the Volume panel can start. Delete Mirror stays in the CLI, where the user types the mirror's name. */
const VolumeRunRequest = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("mirror"), args: Schema.Struct({ to: Server }) }),
  Schema.Struct({ kind: Schema.Literal("sync"), args: Schema.Struct({ full: Schema.Boolean }) }),
  Schema.Struct({ kind: Schema.Literal("move"), args: Schema.Struct({ to: Server }) }),
  Schema.Struct({ kind: Schema.Literal("release"), args: Schema.Struct({}) }),
  Schema.Struct({ kind: Schema.Literal("restore"), args: Schema.Struct({ from: Server }) }),
]);
export type VolumeRunRequest = typeof VolumeRunRequest.Type;

const RequestVolumeRunInput = Schema.Struct({
  ...VolumeRef.fields,
  environment: Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) }),
  run: VolumeRunRequest,
});

export const listVolumeRunsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeRef))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => listVolumeRuns(organization.id, data.volumeId)),
  )));

export const requestVolumeRunServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RequestVolumeRunInput))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => requestVolumeRun(
      { userId: context.actor.userId, organizationId: organization.id },
      { volumeId: data.volumeId, environment: data.environment, ...data.run },
    )),
  )));
