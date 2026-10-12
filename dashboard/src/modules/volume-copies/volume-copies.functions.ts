import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { lastMirrorRefresh, refreshMirror } from "#/modules/volume-copies/volume-copies.refresh.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const VolumeRef = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString });
const Environment = Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) });

export const refreshMirrorServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeRef.fields, environment: Environment })))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => refreshMirror({ userId: context.actor.userId, organizationId: organization.id }, data.environment, data.volumeId)),
  )));

export const lastMirrorRefreshServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeRef))
  .handler(({ context, data }) => runActor(context, requireInfrastructureOrganization(context.actor, data.organizationSlug).pipe(
    Effect.flatMap((organization) => lastMirrorRefresh(organization.id, data.volumeId)),
  )));
