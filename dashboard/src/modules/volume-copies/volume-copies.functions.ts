import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { RefreshMirrorInput } from "#/modules/volume-copies/volume-copies";
import { requireAccess } from "#/modules/organization/access.server";
import { mirrorStatuses, refreshMirror } from "#/modules/volume-copies/volume-copies.refresh.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const VolumeRef = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString });

export const refreshMirrorServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RefreshMirrorInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => refreshMirror({ userId: context.actor.userId, organizationId: organization.id }, data.environment, data.volumeId)),
  )));

export const mirrorStatusesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeRef))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => mirrorStatuses(organization.id, data.volumeId)),
  )));
