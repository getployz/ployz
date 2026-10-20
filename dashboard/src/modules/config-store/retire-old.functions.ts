import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { readDeploymentCleanup } from "#/modules/config-store/retire-old.server";
import { requireAccess } from "#/modules/organization/access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const deploymentCleanupServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString, deploymentId: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, requireAccess(context.actor, data.organizationSlug, "project:read").pipe(
    Effect.flatMap(({ organization }) => readDeploymentCleanup(organization.id, data.deploymentId)),
  )));
