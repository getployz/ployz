import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import { requireAccess } from "#/modules/organization/access.server";
import { mirrorStatuses, refreshMirror } from "#/modules/volume-copies/mirror-sync/mirror-sync.refresh.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { VolumeInput, VolumeRef } from "#/modules/volume-run/volume-run";

export const refreshMirrorServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "services:control").pipe(
    Effect.flatMap(({ organization }) => refreshMirror({ userId: context.actor.userId, organizationId: organization.id }, data.environment, data.volumeId)),
  )));

export const mirrorStatusesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(VolumeRef))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => mirrorStatuses(organization.id, data.volumeId)),
  )));
