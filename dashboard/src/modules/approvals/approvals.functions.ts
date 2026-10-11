import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { freshApproval, freshPendingApprovals } from "#/modules/machines/server-operations.server";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const listPendingApprovalsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { id } = yield* requireInfrastructureOrganization(context.actor, data.organizationSlug);
    return yield* freshPendingApprovals(id);
  })));

export const getApprovalServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { id } = yield* requireInfrastructureOrganization(context.actor, data.organizationSlug);
    return yield* freshApproval(id, data.id);
  })));
