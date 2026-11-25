import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { hasApprover } from "#/modules/operations/gate.server";
import { refusal, type StoreRefusal } from "#/lib/refusal";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { readStore } from "#/modules/config-store/config-store.server";
import { protector, protectorOnly } from "#/modules/config-store/protected.server";
import { environmentProtected } from "#/modules/config-store/store-lookup.server";
import { admission, isPending, namesReview, versionService } from "#/modules/deploy-approval/deploy-tap";
import { enforcement, prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { type Caller, callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import type { DeployReview } from "#/modules/proposals/proposal";
import { askedDeployProposal, presentDeployProposal, reviewDeploy } from "#/modules/proposals/proposals.server";
import { approvalUrl } from "#/modules/proposals/proposal-link.server";
import { AppConfig } from "#/server/config.server";
import { Forbidden, NotFound } from "#/server/public-error";

/** What a Deploy that became a proposal answers: the link a person opens. */
export type DeployLinked = { readonly proposal_id: string; readonly status: "presented" | "approved"; readonly url: string; readonly message: string };

/** `null`: the Deploy goes on as it always did. A refusal: why it can't ask a person. Linked: it waits for one. */
export type DeployLink = null | { readonly refused: StoreRefusal } | { readonly linked: DeployLinked };

/**
 * The one place a Deploy becomes a proposal: an agent's, one from a caller without a key, and a member's in a Protected
 * Environment they can't protect, from the dashboard too. A caller that can sign, an agent's or keyless caller's Deploy
 * that nothing holds, and a Deploy the Store or this plan can't take all go on as they were. Everything else waits on
 * one person: unsigned Services the Servers need a passkey for, the agent gate, a Volume-loss review, a Pending value,
 * a protector's say. Its command is bound to the review it was asked against.
 */
export const linkDeploy = Effect.fn("DeployLink.link")(function* (caller: Caller, command: AdmitDeploy) {
  if (caller.role === null || !(yield* hasApprover(caller.organization.id))) return null;
  // Only someone who can't protect the Environment may need a protector's say; anyone else's dashboard Deploy just runs.
  const mayGuard = protectorOnly(caller, command.environment.environment ?? "") !== null;
  if (caller.via === "dashboard" && !mayGuard) return null;
  // A retry of a request already asked finds its proposal before anything is planned: once approved, its Deployment
  // holds this id, and a plan or a probe would read the retry as another Deploy.
  const again = yield* askedDeployProposal(caller, command);
  if (again !== null) return again.refused === undefined ? yield* linked(caller, again) : { refused: again.refused };
  const agent = caller.via === "agent";
  const covered = new Set((command.versions ?? []).map(versionService));
  // A Device that signed the rule with this Deploy already signed its Source step, so the rule isn't read for it.
  const keyed = command.rule !== undefined;
  const planned = yield* prepareDeployTap(caller, {
    organizationSlug: caller.organization.slug, environment: command.environment, services: command.services ?? [], classify: true,
    upload: command.upload !== undefined && command.upload !== null, rule: !keyed,
  }).pipe(Effect.catchIf((error) => error instanceof Forbidden || error instanceof NotFound, () => Effect.succeed(null)));
  if (planned === null) return null;
  const waits = planned.kind === "refused" && isPending(planned.details) ? planned.details : null;
  if (planned.kind === "refused" && waits === null) return null;
  const pending = waits !== null;
  const plan = planned.kind === "plan" ? planned.plan : null;
  // A Pending plan reads no tap, so only whether a Server enforces device approval says its uploads won't build.
  const uploads = waits?.uploads ?? [];
  const servers = planned.kind === "plan" ? planned.tap : uploads.length > 0 ? yield* enforcement(caller.organization.id) : { kind: "off" } as const;
  const admits = admission({
    plan: plan ?? { uploads: [...uploads] }, servers, source: planned.kind === "plan" ? planned.source : undefined, covered,
    rule: keyed ? { kind: "device" } : { kind: "none" },
  });
  // Only an agent waits on a person when Cloud couldn't reach the Servers. Anyone else's Deploy queues as it always did,
  // and each Server checks it.
  if (admits.kind === "refused" && admits.unreached) return agent ? { refused: refusal("conflict", `${admits.reason} Nothing deployed.`) } : null;
  if (admits.kind === "refused") return { refused: refusal("conflict", admits.reason) };
  // A Git build no rule covers needs one even when it reaches no data, so the approver's tap signs the rule with it.
  const signs = admits.kind === "ask";
  const makes = pending && covered.size === 0;
  if (!agent && !signs && !makes && !mayGuard) return null;

  const place = yield* readStore(caller.organization.id, { query: "namespace", environment: command.environment }).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(null)),
  );
  if (place === null) return null;
  const guarded = protectorOnly(caller, place.environment.name) !== null && (yield* environmentProtected(caller.organization.id, place.environment.project, place.environment.id).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(true)),
  ));
  if (!agent && !signs && !makes && !guarded) return null;
  let version = command.version ?? null;
  if (plan !== null) {
    if (version !== null && !namesReview(plan.version, version)) return { refused: refusal("conflict", "Your changes moved since you looked. Check them, then deploy again.") };
    version ??= plan.version;
  } else if (version === null) {
    const diff = yield* readStore(caller.organization.id, { query: "diff", environment: command.environment }).pipe(
      Effect.catchTag("StoreRefused", () => Effect.succeed(null)),
    );
    if (diff === null) return null;
    version = diff.version;
  }
  // Only the approver's tap signs the rule the Deploy keeps: the asker's is dropped.
  const { rule: _rule, ...ruleless } = command;
  let bound: AdmitDeploy = {
    ...ruleless, services: [...command.services ?? []], version,
    environment: { project: place.environment.project, environment: place.environment.name },
  };
  // A Pending value has no review yet: the approval page makes it, and the Store's review is read then.
  let review: DeployReview | undefined;
  if (plan !== null) {
    const gate = { permissions: callerPermissions(caller), environments: callerEnvironments(caller), protector: protector(caller) };
    const reviewed = yield* reviewDeploy(caller.organization.id, caller.userId, gate, bound);
    if (reviewed === null) return null;
    if (reviewed.refused !== undefined) return { refused: reviewed.refused };
    ({ command: bound, review } = reviewed);
  }
  const presented = yield* presentDeployProposal(caller, { ...place.environment, namespace: place.namespace }, { asked: command, bound, review });
  return presented.refused === undefined ? yield* linked(caller, presented) : { refused: presented.refused };
});

const linked = Effect.fn("DeployLink.linked")(function* (caller: Caller, proposal: { readonly id: string; readonly status: DeployLinked["status"] }) {
  const { app } = yield* AppConfig;
  const url = approvalUrl(app.url, caller.organization.slug, proposal.id);
  const message = proposal.status === "approved" ? `Approved at ${url}. Follow proposal ${proposal.id} for the deploy.` : `Waiting for approval at ${url}. Nothing deployed yet.`;
  return { linked: { proposal_id: proposal.id, status: proposal.status, url, message } } satisfies DeployLink;
});
