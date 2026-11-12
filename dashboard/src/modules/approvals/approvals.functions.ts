import { createServerFn } from "@tanstack/react-start";
import type { Signed } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { MoveService, VolumeApproval } from "#/modules/operations/cli-wire";
import { decideAndApply } from "#/modules/approvals/apply.server";
import { askerNames, type ApprovalView, readAsker } from "#/modules/approvals/approvals.server";
import { ApprovalsInput } from "#/modules/approval-session/approvals-input";
import { findService } from "#/modules/config-store/store-lookup.server";
import { actTarget } from "#/modules/service-act/service-act.server";
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
  .validator(strictValidator(Schema.Struct({
    organizationSlug: Schema.String,
    id: Schema.String,
    decision: ApprovalDecision,
    /** For a restart or stop on Servers that enforce: one approval per container, from the approver's passkey session. */
    approvals: Schema.optional(ApprovalsInput),
    /** For a Volume run on Servers that enforce: the approver's passkey answer to its workflow, and a Move's spec. */
    volume: Schema.optional(Schema.Struct({ approval: VolumeApproval, service: Schema.optional(MoveService) })),
  })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    // SAFETY: the schema bounds each field's shape; Servers parse and verify the rest.
    const approvals = data.approvals as Readonly<Record<string, Signed>> | undefined;
    const decided = yield* decideAndApply({ ...access, via: "dashboard" }, data.id, data.decision, freshOperationDigest, approvals, data.volume).pipe(
      Effect.catchTag("NotFound", (missing) => Effect.succeed({ ok: false as const, refusal: { message: missing.message } })),
    );
    return decided.ok
      ? { ok: true as const, approval: yield* shownOne(access, decided.approval) }
      : { ok: false as const, message: decided.refusal.message };
  })));

/**
 * The restart or stop a dashboard ask runs once approved, as the approver's browser needs it to make approvals: the
 * Service's copies and the Servers. Null for anything else, or when Cloud can't read the Servers.
 */
export const approvalActTargetServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    const asker = yield* readAsker(access.organization.id, data.id);
    if (asker === undefined || "diff" in asker.review) return null;
    const apply = asker.review.apply;
    if (apply === undefined || !("service" in apply)) return null;
    const found = yield* findService(access.organization.id, apply.environmentId, apply.service);
    if (found === null) return null;
    const target = yield* actTarget(access.organization.id, found);
    return target === null ? null : { verb: apply.verb, environmentId: apply.environmentId, service: apply.service, project: found.project, environmentName: found.environment, namespace: found.namespace, ...target };
  })));
