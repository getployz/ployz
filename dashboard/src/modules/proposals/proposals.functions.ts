import { createServerFn } from "@tanstack/react-start";
import type { Signed } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { ProposalApprovalsInput, ProposalBundleInput } from "#/modules/approval-session/proposals-input";
import { askerNames } from "#/modules/approvals/approvals.server";
import { requireAccess } from "#/modules/organization/access.server";
import type { ProposalApprovals } from "#/modules/proposals/proposal";
import { decideProposal, getProposal, pendingProposals, type ProposalView, readProposalTarget } from "#/modules/proposals/proposals.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Decision = Schema.Union([
  Schema.Struct({ approve: Schema.Struct({ approvals: ProposalApprovalsInput, bundle: ProposalBundleInput }) }),
  Schema.Struct({ reject: Schema.Struct({ reason: Schema.optional(Schema.String.check(Schema.isMaxLength(1024))) }) }),
]);

const named = (proposals: readonly ProposalView[]) =>
  Effect.map(askerNames(proposals), (names) => proposals.map((proposal) => ({
    ...proposal,
    asker: proposal.requested_by === null ? null : names.get(proposal.requested_by) ?? null,
  })));

export const listPendingProposalsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
    return yield* named(yield* pendingProposals(access.organization.id));
  })));

export const getProposalServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
    const [proposal] = yield* named([yield* getProposal(access.organization.id, data.id)]);
    return proposal ?? null;
  })));

/** Each step as the approver's browser signs it: the Service's copies and the Servers. Null when Cloud can't read them. */
export const proposalTargetServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    return yield* readProposalTarget(access.organization.id, data.id);
  })));

/** The sidebar's answer to one proposal. A refusal, such as a Server that can't take it, comes back as its message. */
export const decideProposalServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String, decision: Decision })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    // SAFETY: the schema bounds each field's shape; Servers parse and verify the rest.
    const decision = "approve" in data.decision
      ? { approve: { approvals: data.decision.approve.approvals as ProposalApprovals, bundle: data.decision.approve.bundle as Signed } }
      : data.decision;
    const decided = yield* decideProposal(access, data.id, decision).pipe(
      Effect.catchTag("NotFound", (missing) => Effect.succeed({ ok: false as const, message: missing.message })),
    );
    if (!decided.ok) return decided;
    const [proposal] = yield* named([decided.proposal]);
    return proposal === undefined ? { ok: false as const, message: "No such proposal." } : { ok: true as const, proposal };
  })));
