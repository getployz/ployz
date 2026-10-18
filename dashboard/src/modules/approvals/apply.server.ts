import "@tanstack/react-start/server-only";
import type { ConfigCommand, DiffView } from "@ployz/sdk";
import { Effect } from "effect";
import type { ApprovalDecision, ServiceApply } from "#/modules/approvals/approvals";
import { type ApprovalView, type Decider, decideApproval, onceFor, type OperationDigest, readAsker, undoApproval } from "#/modules/approvals/approvals.server";
import { callStore, writePermission } from "#/modules/config-store/config-store.server";
import { findService } from "#/modules/config-store/store-lookup.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { bodyDigest } from "#/modules/operations/keyed.server";
import { OPERATIONS } from "#/modules/operations/operations";
import { actService } from "#/modules/operations/operations.server";
import { type Access, requireAccessById } from "#/modules/organization/access.server";

/** Why an approved ask didn't run; `moved` when the plan it reviewed is gone, so it must be asked again. */
type NotRun = { readonly message: string; readonly moved: boolean };

const notRun = (refusal: StoreRefusal): NotRun =>
  ({ message: refusal.message, moved: refusal.code === "approval_required" || refusal.code === "conflict" });

const askerCaller = (access: Access, credentialId: string): Caller => ({
  userId: access.userId,
  organization: access.organization,
  credential: { kind: "session", id: credentialId },
  role: access.kind === "member" ? access.role : null,
  support: access.kind === "support" ? access.shows : null,
  via: "dashboard",
});

const applyStore = Effect.fn("Approvals.applyStore")(function* (organizationId: string, userId: string, asked: ConfigCommand, approval: ApprovalView, reviewed: DiffView) {
  const command = onceFor(asked, approval.id, reviewed);
  const access = yield* requireAccessById({ userId }, organizationId, writePermission(command), "store");
  const result = yield* callStore(organizationId, access.userId, { operation: "write", command }, { source: "dashboard" }, {
    approval: { approved: approval.digest },
    permissions: access.permissions,
    environments: access.environments,
  });
  return result.ok ? null : notRun(result.refusal);
});

const applyService = Effect.fn("Approvals.applyService")(function* (
  organizationId: string,
  asker: { userId: string; credentialId: string },
  approval: ApprovalView,
  { environmentId, service, verb }: ServiceApply,
) {
  const access = yield* requireAccessById({ userId: asker.userId }, organizationId, OPERATIONS[`service.${verb}`].requires);
  const found = yield* findService(organizationId, environmentId, service);
  if (found === null) return { message: "The service is gone.", moved: true } satisfies NotRun;
  const body = { environmentId, service, only: null };
  const admission = yield* actService(
    askerCaller(access, asker.credentialId),
    { ...body, ...found, verb, digest: bodyDigest(`service_${verb}`, body) },
    { key: null, approvalId: approval.id },
  );
  return admission.ok ? null : notRun(admission.refusal);
});

/**
 * Runs what a dashboard ask into a Protected Environment waited on, as its asker with what they hold now and the
 * approval it was given. An ask from `ployz` or an agent has no `apply`: it runs when it asks again with the approval.
 */
const applyApproved = Effect.fn("Approvals.applyApproved")(function* (organizationId: string, approval: ApprovalView) {
  const asker = yield* readAsker(organizationId, approval.id);
  if (asker === undefined || asker.review.apply === undefined) return null;
  if (asker.requestedByUserId === null) return { message: "Whoever asked is no longer in the Organization.", moved: true } satisfies NotRun;
  return "diff" in asker.review
    ? yield* applyStore(organizationId, asker.requestedByUserId, asker.review.apply, approval, asker.review.diff)
    : yield* applyService(organizationId, { userId: asker.requestedByUserId, credentialId: asker.credentialId }, approval, asker.review.apply);
});

/**
 * `decideApproval`, and once this decision approves a dashboard ask into a Protected Environment, what it waited on
 * runs. When it doesn't run, the approval is handed back, to pending or superseded when its plan moved, and the
 * decider is told which. Only the decision that approved the row runs it, so approving again never runs it twice.
 */
export const decideAndApply = <R = never>(
  decider: Decider,
  id: string,
  decision: ApprovalDecision,
  operationDigest?: OperationDigest<R>,
) => Effect.gen(function* () {
  const decided = yield* decideApproval(decider, id, decision, operationDigest);
  if (!decided.ok || decided.decided !== true || !("approve" in decision)) return decided;
  const failed = yield* applyApproved(decider.organization.id, decided.approval).pipe(
    Effect.catchTags({
      Forbidden: (error) => Effect.succeed({ message: error.message, moved: false } satisfies NotRun),
      NotFound: (error) => Effect.succeed({ message: error.message, moved: true } satisfies NotRun),
    }),
  );
  if (failed === null) return decided;
  yield* undoApproval(decider.organization.id, id, failed.moved);
  const then = failed.moved ? "The plan changed, so it has to be asked again." : "It's still waiting for approval.";
  const refusal: StoreRefusal = { code: "conflict", message: `Approved, but it didn't run: ${failed.message} ${then}`, details: null };
  return { ok: false as const, refusal };
}).pipe(Effect.withSpan("Approvals.decideAndApply"));
