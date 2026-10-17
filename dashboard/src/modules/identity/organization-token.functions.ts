import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { createOrganizationToken, listCredentials, revokeCredential } from "#/modules/identity/organization-token.server";
import { retireServerAccess } from "#/modules/machines/server-access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Slug = Schema.NonEmptyString;
const Preset = Schema.Union([
  Schema.Struct({ preset: Schema.Literals(["read", "full"]) }),
  Schema.Struct({ preset: Schema.Literal("deploy"), environment: Schema.NonEmptyString }),
]);

export const listTokensServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug })))
  .handler(({ context, data }) => runActor(context, dashboardCaller(getRequest().headers, data.organizationSlug).pipe(
    Effect.flatMap(listCredentials),
    Effect.map(({ tokens }) => tokens),
  )));

export const createTokenServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({
    organizationSlug: Slug,
    name: Schema.NonEmptyString.check(Schema.isMaxLength(64)),
    expiresInDays: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 365 })),
    preset: Preset,
  })))
  .handler(({ context, data }) => runActor(context, dashboardCaller(getRequest().headers, data.organizationSlug).pipe(
    Effect.flatMap((caller) => createOrganizationToken(caller, { name: data.name, expiresInDays: data.expiresInDays, preset: data.preset })),
  )));

export const revokeTokenServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, id: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, dashboardCaller(getRequest().headers, data.organizationSlug).pipe(
    Effect.flatMap((caller) => revokeCredential(caller, data.id)),
    Effect.tap(() => retireServerAccess(data.id)),
  )));
