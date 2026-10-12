import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { freshApproval, freshOperationDigest, freshPendingApprovals } from "#/modules/machines/server-operations.server";
import { requireAccess } from "#/modules/organization/access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const listPendingApprovalsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization: { id } } = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return yield* freshPendingApprovals(id);
  })));

export const getApprovalServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization: { id } } = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return yield* freshApproval(id, data.id);
  })));

/**
 * The sidebar's answer to one approval, in the Organization its URL names, so support decides where they stand. A
 * refusal, such as a plan that changed since the card drew, comes back as its message.
 */
export const decideApprovalServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String, decision: ApprovalDecision })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return yield* decideApproval(access, data.id, data.decision, freshOperationDigest).pipe(
      Effect.map((decided) => decided.ok
        ? { ok: true as const, approval: decided.approval }
        : { ok: false as const, message: decided.refusal.message }),
      Effect.catchTag("NotFound", (missing) => Effect.succeed({ ok: false as const, message: missing.message })),
    );
  })));
