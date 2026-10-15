import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { type ApprovalView, decideApproval } from "#/modules/approvals/approvals.server";
import { freshApproval, freshOperationDigest, freshPendingApprovals } from "#/modules/machines/server-operations.server";
import { type Access, requireAccess } from "#/modules/organization/access.server";
import { approvalForSupport } from "#/modules/support-access/support-view";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

/** An approval as `access` may read it: support sees its review under the Organization's rules. */
const shownTo = (access: Access) => <T extends ApprovalView | null>(approval: T): T =>
  access.kind === "support" && approval !== null ? approvalForSupport(approval, access.shows) : approval;

export const listPendingApprovalsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return (yield* freshPendingApprovals(access.organization.id)).map(shownTo(access));
  })));

export const getApprovalServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "operate");
    return shownTo(access)(yield* freshApproval(access.organization.id, data.id));
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
    return yield* decideApproval({ ...access, via: "dashboard" }, data.id, data.decision, freshOperationDigest).pipe(
      Effect.map((decided) => decided.ok
        ? { ok: true as const, approval: shownTo(access)(decided.approval) }
        : { ok: false as const, message: decided.refusal.message }),
      Effect.catchTag("NotFound", (missing) => Effect.succeed({ ok: false as const, message: missing.message })),
    );
  })));
