import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { requireAccess } from "#/modules/organization/access.server";
import { offsiteCopies } from "#/modules/volume-copies/offsite-bucket.server";
import { addVolumeOffsite, readVolumeOffsite, uploadVolumeOffsite } from "#/modules/volume-copies/volume-offsite.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Environment = Schema.Struct({ project: Schema.NullOr(Schema.String), environment: Schema.NullOr(Schema.String) });
const EnvironmentInput = Schema.Struct({ organizationSlug: Schema.NonEmptyString, environment: Environment });
const VolumeInput = Schema.Struct({ ...EnvironmentInput.fields, volumeId: Schema.NonEmptyString });
const Choice = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("existing"), id: Schema.String.check(Schema.isUUID()) }),
  Schema.Struct({
    kind: Schema.Literal("new"),
    endpoint: Schema.String.check(Schema.isMaxLength(256)),
    bucket: Schema.String.check(Schema.isPattern(/^[a-z0-9.-]{3,63}$/)),
    accessKey: Schema.NonEmptyString.check(Schema.isMaxLength(256)),
    secretKey: Schema.NonEmptyString.check(Schema.isMaxLength(256)),
  }),
]);

export const readVolumeOffsiteServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => readVolumeOffsite(organization.id, data.environment, data.volumeId)),
  )));

export const addVolumeOffsiteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeInput.fields, choice: Choice })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => addVolumeOffsite(organization.id, data.environment, data.volumeId, data.choice)),
  )));

export const uploadVolumeOffsiteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ ...VolumeInput.fields, from: Schema.NullOr(Schema.NonEmptyString) })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => uploadVolumeOffsite(organization.id, data.environment, data.volumeId, data.from)),
  )));

/** The Environment's offsite copies for the canvas cards: bucket and schedule, never a key. */
export const listOffsiteCopiesServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(EnvironmentInput))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "operate").pipe(
    Effect.flatMap(({ organization }) => readStore(organization.id, { query: "volumes", environment: data.environment }).pipe(
      Effect.flatMap(({ environment }) => offsiteCopies(organization.id, environment.id)),
    )),
  )));
