import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { hasApprover } from "#/modules/approvals/approvals.server";
import { refusal, type StoreRefusal } from "#/lib/refusal";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { readStore } from "#/modules/config-store/config-store.server";
import { protector, protectorOnly } from "#/modules/config-store/protected.server";
import { environmentProtected } from "#/modules/config-store/store-lookup.server";
import { blocked, isPending, namesReview, versionService } from "#/modules/deploy-approval/deploy-tap";
import { prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { type Caller, callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import type { DeployReview } from "#/modules/proposals/proposal";
import { presentDeployProposal, reviewDeploy } from "#/modules/proposals/proposals.server";
import { AppConfig } from "#/server/config.server";
import { Forbidden, NotFound } from "#/server/public-error";

/** Where a person approves a presented proposal. */
export const approvalUrl = (appUrl: URL, organizationSlug: string, proposalId: string) =>
  new URL(`/cloud/${organizationSlug}/approve/${proposalId}`, appUrl).href;

/** What a Deploy that became a proposal answers: the link a person opens. */
export type DeployLinked = { readonly proposal_id: string; readonly status: "presented"; readonly url: string; readonly message: string };

/** `null`: the Deploy goes on as it always did. A refusal: why it can't ask a person. Linked: it waits for one. */
export type DeployLink = null | { readonly refused: StoreRefusal } | { readonly linked: DeployLinked };

/**
 * The one place a Deploy from an agent or a caller without a key becomes a proposal. A caller that can sign, an
 * agent's or keyless caller's Deploy that nothing holds, and a Deploy the Store or this plan can't take all go on as
 * they were. Everything else waits on one person's passkey: unsigned Services the Servers need a passkey for, the
 * agent gate, a Volume-loss review, a Pending value. Its command is bound to the review it was asked against.
 */
export const linkDeploy = Effect.fn("DeployLink.link")(function* (caller: Caller, command: AdmitDeploy) {
  if (caller.role === null || caller.via === "dashboard" || !(yield* hasApprover(caller.organization.id))) return null;
  const agent = caller.via === "agent";
  const covered = new Set((command.versions ?? []).map(versionService));
  const planned = yield* prepareDeployTap(caller, {
    organizationSlug: caller.organization.slug, environment: command.environment, services: command.services ?? [], classify: true,
  }).pipe(Effect.catchIf((error) => error instanceof Forbidden || error instanceof NotFound, () => Effect.succeed(null)));
  if (planned === null) return null;
  const pending = planned.kind === "refused" && isPending(planned.details);
  if (planned.kind === "refused" && !pending) return null;
  const plan = planned.kind === "plan" ? planned.plan : null;
  const open = (plan?.unsigned ?? []).filter(({ service }) => !covered.has(service));
  const tap = planned.kind === "plan" ? planned.tap : null;
  // Only an agent waits on a person here. Anyone else's Deploy queues as it always did, and each Server checks it.
  if (open.length > 0 && tap?.kind === "refused") return agent ? { refused: refusal("conflict", `${tap.message} Nothing deployed.`) } : null;
  const cannot = tap?.kind === "ready" ? blocked(open) : null;
  if (cannot !== null) return { refused: refusal("conflict", cannot) };
  const signs = tap?.kind === "ready" && open.length > 0;
  const makes = pending && covered.size === 0;
  if (!agent && !signs && !makes) return null;

  const place = yield* readStore(caller.organization.id, { query: "namespace", environment: command.environment }).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(null)),
  );
  if (place === null) return null;
  // A member who can't protect keeps today's card for a protector, unless only this page can sign or fill the Deploy.
  const isProtected = yield* environmentProtected(caller.organization.id, place.environment.project, place.environment.id).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(true)),
  );
  if (isProtected && protectorOnly(caller, place.environment.name) !== null && !signs && !makes) return null;
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
  let bound: AdmitDeploy = {
    ...command, services: [...command.services ?? []], version,
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
  if ("refused" in presented) return presented;
  const { app } = yield* AppConfig;
  const url = approvalUrl(app.url, caller.organization.slug, presented.id);
  return { linked: { proposal_id: presented.id, status: "presented", url, message: `Waiting for approval at ${url}. Nothing deployed yet.` } } satisfies DeployLink;
});
