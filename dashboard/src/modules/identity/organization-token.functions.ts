import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { createOrganizationToken, knownEnvironment, listCredentials, revokeCredential } from "#/modules/identity/organization-token.server";
import { EnvironmentName } from "#/modules/identity/token-grant";
import { retireServerAccess } from "#/modules/machines/server-access.server";
import { callerMiddleware, OrganizationSlugInput, runActor } from "#/server/tanstack";

const Slug = Schema.NonEmptyString;
const Preset = Schema.Union([
  Schema.Struct({ preset: Schema.Literals(["read", "full"]) }),
  Schema.Struct({ preset: Schema.Literal("deploy"), environment: EnvironmentName }),
]);

export const listTokensServerFn = createServerFn({ method: "GET" })
  .middleware([callerMiddleware(OrganizationSlugInput)])
  .handler(({ context }) => runActor(context, listCredentials(context.caller).pipe(
    Effect.map(({ tokens }) => tokens),
  )));

export const createTokenServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(Schema.Struct({
    organizationSlug: Slug,
    name: Schema.NonEmptyString.check(Schema.isMaxLength(64)),
    expiresInDays: Schema.Int.check(Schema.isBetween({ minimum: 1, maximum: 365 })),
    preset: Preset,
  }))])
  .handler(({ context, data }) => runActor(context, knownEnvironment(context.caller, data.preset).pipe(
    Effect.flatMap((preset) => createOrganizationToken(context.caller, { name: data.name, expiresInDays: data.expiresInDays, preset })),
  )));

export const revokeTokenServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(Schema.Struct({ organizationSlug: Slug, id: Schema.NonEmptyString }))])
  .handler(({ context, data }) => runActor(context, revokeCredential(context.caller, data.id).pipe(
    Effect.tap(() => retireServerAccess(data.id)),
  )));
