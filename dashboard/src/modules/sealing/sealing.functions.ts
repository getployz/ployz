import { setResponseHeader } from "@tanstack/react-start/server";
import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireAccess } from "#/modules/organization/access.server";
import { Forbidden } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { readClusterSealing } from "./cluster-sealing.server";

/** Whether the browser seals the Organization's Sealed values, and for which Organization id. Members only. */
export const readSealingFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(async ({ context, data }) => {
    setResponseHeader("cache-control", "private, no-store");
    return await runActor(context, Effect.gen(function* () {
      const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
      if (access.kind === "support") return yield* new Forbidden({ message: "Only members set sealed values." });
      return { organizationId: access.organization.id, sealing: yield* readClusterSealing(access.organization.id) };
    }));
  });
