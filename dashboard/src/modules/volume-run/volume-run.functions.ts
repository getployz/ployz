import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { runVolume } from "#/modules/operations/operations.server";
import { requireAccess } from "#/modules/organization/access.server";
import { ResizeVolumeInput } from "#/modules/volume-run/resize";
import { getVolumeRun, isVolumeOwed, listActiveVolumeRuns } from "#/modules/volume-run/volume-run.rows.server";
import { inspectVolumeCopies } from "#/modules/volume-run/volume-run.cluster.server";
import { Conflict } from "#/server/public-error";
import { authenticated, callerMiddleware, OrganizationSlugInput, runActor, strictValidator } from "#/server/tanstack";
import { VolumeInput, VolumeRef } from "#/modules/volume-run/volume-run";

export const inspectVolumeCopiesServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => inspectVolumeCopies(organization.id, data.environment, data.volumeId)),
  )));

export const isVolumeOwedServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(VolumeRef))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => isVolumeOwed(organization.id, data.volumeId)),
  )));

export const listActiveVolumeRunsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationSlugInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => listActiveVolumeRuns(organization.id)),
  )));

export const getVolumeRunServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, id: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "volumes:read").pipe(
    Effect.flatMap(({ organization }) => getVolumeRun(organization.id, data.id)),
  )));

export const resizeVolumeServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(ResizeVolumeInput)])
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const admission = yield* runVolume(
      context.caller,
      { volumeId: data.volumeId, environment: data.environment, kind: "resize", args: { maximum_bytes: data.maximumBytes } },
      { key: data.requestId, approved: null },
    );
    // A grow in a Protected Environment its asker can't protect waits for someone who can.
    if (!admission.ok && admission.refusal.code === "approval_required") return { requested: true as const };
    if (!admission.ok) return yield* new Conflict({ message: admission.refusal.message, userFacing: true });
    return { id: admission.id };
  })));
