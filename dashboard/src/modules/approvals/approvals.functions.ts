import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { decideAndApply } from "#/modules/approvals/apply.server";
import { askerNames, type ApprovalView } from "#/modules/approvals/approvals.server";
import { freshApproval, freshOperationDigest, freshPendingApprovals } from "#/modules/machines/server-operations.server";
import { type Access, requireAccess } from "#/modules/organization/access.server";
import { approvalForSupport } from "#/modules/support-access/support-view";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

/**
 * Approvals as `access` may read them: support sees each review under the Organization's rules, `asker` names who asked,
 * and `yours` marks one they asked for.
 */
const shown = (access: Access, names: ReadonlyMap<string, string>) => (approval: ApprovalView) => ({
  ...(access.kind === "support" ? approvalForSupport(approval, access.shows) : approval),
  asker: approval.requested_by === null ? null : names.get(approval.requested_by) ?? null,
  yours: approval.requested_by === access.userId,
});
const shownTo = (access: Access, approvals: readonly ApprovalView[]) =>
  Effect.map(askerNames(approvals), (names) => approvals.map(shown(access, names)));
const shownOne = (access: Access, approval: ApprovalView) =>
  Effect.map(askerNames([approval]), (names) => shown(access, names)(approval));

export const listPendingApprovalsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
    return yield* shownTo(access, yield* freshPendingApprovals(access.organization.id));
  })));

export const getApprovalServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
    return yield* shownOne(access, yield* freshApproval(access.organization.id, data.id));
  })));

/**
 * The sidebar's answer to one approval, in the Organization its URL names, so support decides where they stand. A
 * refusal, such as a plan that changed since the card drew, comes back as its message.
 */
export const decideApprovalServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String, decision: ApprovalDecision })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    const decided = yield* decideAndApply({ ...access, via: "dashboard" }, data.id, data.decision, freshOperationDigest).pipe(
      Effect.catchTag("NotFound", (missing) => Effect.succeed({ ok: false as const, refusal: { message: missing.message } })),
    );
    return decided.ok
      ? { ok: true as const, approval: yield* shownOne(access, decided.approval) }
      : { ok: false as const, message: decided.refusal.message };
  })));
