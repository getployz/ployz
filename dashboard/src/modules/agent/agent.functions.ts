import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { recentThreads } from "#/modules/agent/persistence.server";
import { requireAccess } from "#/modules/organization/access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

/** The signed-in member's latest sidebar chats in the Organization, for chat history. */
export const listAgentThreadsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return yield* recentThreads({ organizationId: access.organization.id, userId: access.userId });
  })));
