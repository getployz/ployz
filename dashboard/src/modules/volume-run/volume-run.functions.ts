import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { runVolume } from "#/modules/operations/operations.server";
import { requireAccess } from "#/modules/organization/access.server";
import { ResizeVolumeInput } from "#/modules/volume-run/resize-check";
import { getVolumeRun, inspectVolumeCopies, isVolumeOwed, listActiveVolumeRuns } from "#/modules/volume-run/volume-run.server";
import { Conflict } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const VolumeRef = Schema.Struct({ organizationSlug: Schema.NonEmptyString, volumeId: Schema.NonEmptyString });

const VolumeCopiesInput = Schema.Struct({
  ...VolumeRef.fields,
  environment: Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) }),
});

export const inspectVolumeCopiesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeCopiesInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => inspectVolumeCopies(organization.id, data.environment, data.volumeId)),
  )));

export const isVolumeOwedServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeRef))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => isVolumeOwed(organization.id, data.volumeId)),
  )));

export const listActiveVolumeRunsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => listActiveVolumeRuns(organization.id)),
  )));

export const getVolumeRunServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, id: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => getVolumeRun(organization.id, data.id)),
  )));

export const resizeVolumeServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ResizeVolumeInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const caller = yield* dashboardCaller(getRequest().headers, data.organizationSlug);
    const admission = yield* runVolume(
      caller,
      { volumeId: data.volumeId, environment: data.environment, kind: "resize", args: { maximum_bytes: data.maximumBytes } },
      { key: data.requestId, approvalId: null },
    );
    if (!admission.ok) return yield* new Conflict({ message: admission.refusal.message, userFacing: true });
    return { id: admission.id };
  })));
