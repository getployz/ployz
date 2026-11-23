import { createServerFn } from "@tanstack/react-start";
import type { Change, PostedVersion } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { askerNames } from "#/modules/approvals/approvals.server";
import { requireAccess } from "#/modules/organization/access.server";
import { decideProposal, fillDeploy, getProposal, pendingProposals, type ProposalView } from "#/modules/proposals/proposals.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

/** A version the passkey signed. The Store checks the rest of it: this only keeps what is not one out. */
const signed = Schema.is(Schema.Struct({ statement: Schema.Struct({ body: Schema.String }) }));
const Posted = Schema.declare<PostedVersion>((value): value is PostedVersion => signed(value));

const Decision = Schema.Union([
  Schema.Struct({ approve: Schema.Struct({ versions: Schema.optional(Schema.Array(Posted).check(Schema.isMaxLength(256))) }) }),
  Schema.Struct({ reject: Schema.Struct({ reason: Schema.optional(Schema.String.check(Schema.isMaxLength(1024))) }) }),
]);

const named = (proposals: readonly ProposalView[]) =>
  Effect.map(askerNames(proposals), (names) => proposals.map((proposal) => ({
    ...proposal,
    asker: proposal.requested_by === null ? null : names.get(proposal.requested_by) ?? null,
  })));

export const listPendingProposalsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
    return yield* named(yield* pendingProposals(access.organization.id));
  })));

export const getProposalServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "org:read");
    const [proposal] = yield* named([yield* getProposal(access.organization.id, data.id)]);
    return proposal ?? null;
  })));

/** The sidebar's answer to one proposal. A refusal, such as a Server that can't take it, comes back as its message. */
export const decideProposalServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String, decision: Decision })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "approvals:answer");
    const decided = yield* decideProposal(access, data.id, data.decision).pipe(
      Effect.catchTag("NotFound", (missing) => Effect.succeed({ ok: false as const, message: missing.message })),
    );
    if (!decided.ok) return decided;
    const [proposal] = yield* named([decided.proposal]);
    return proposal === undefined ? { ok: false as const, message: "No such proposal." } : { ok: true as const, proposal };
  })));

/** A Pending value the approval page made and sealed: set to a sealed secret, and nothing else. */
const isMade = Schema.is(Schema.Struct({ op: Schema.Literal("set"), path: Schema.String, value: Schema.Struct({ secret: Schema.Struct({}) }) }));
const Made = Schema.declare<Change>((value): value is Change => isMade(value));

/** The approval page's one edit: each Pending value its Deploy needs, made and sealed in this browser. */
export const fillDeployServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, id: Schema.String, changes: Schema.Array(Made).check(Schema.isMaxLength(256)) })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const access = yield* requireAccess(context.actor, data.organizationSlug, "changes:deploy");
    return yield* fillDeploy({ ...access, credential: context.actor.credential }, data.id, data.changes);
  })));
