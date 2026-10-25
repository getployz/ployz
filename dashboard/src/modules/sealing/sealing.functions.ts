import { setResponseHeader } from "@tanstack/react-start/server";
import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireAccess } from "#/modules/organization/access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { readClusterSealingToSeal } from "./cluster-sealing.server";

/** Whether the browser seals the Organization's Sealed values, and for which Organization id. */
export const readSealingFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(async ({ context, data }) => {
    setResponseHeader("cache-control", "private, no-store");
    return await runActor(context, Effect.gen(function* () {
      const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
      return { organizationId: access.organization.id, sealing: yield* readClusterSealingToSeal(access.organization.id) };
    }));
  });
