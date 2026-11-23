import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { hasApprover } from "#/modules/approvals/approvals.server";
import { refusal, type StoreRefusal } from "#/lib/refusal";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { callStore, readStore } from "#/modules/config-store/config-store.server";
import { protector, protectorOnly } from "#/modules/config-store/protected.server";
import { environmentProtected } from "#/modules/config-store/store-lookup.server";
import { BLOCKED, isPending, namesReview, versionService } from "#/modules/deploy-approval/deploy-tap";
import { prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { type Caller, callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { presentDeployProposal } from "#/modules/proposals/proposals.server";
import { AppConfig } from "#/server/config.server";
import { Forbidden, NotFound } from "#/server/public-error";

/** Where a person approves a presented proposal. */
export const approvalUrl = (appUrl: URL, organizationSlug: string, proposalId: string) =>
  new URL(`/cloud/${organizationSlug}/approve/${proposalId}`, appUrl).href;

/** What a Deploy that became a proposal answers: the link a person opens. */
export type DeployLinked = { readonly proposal_id: string; readonly status: "presented"; readonly url: string; readonly message: string };

/** `null`: the Deploy goes on as it always did. A refusal: why it can't ask a person. Linked: it waits for one. */
export type DeployLink = null | { readonly refused: StoreRefusal } | { readonly linked: DeployLinked };

const linkedLink = (linked: DeployLinked): DeployLink => ({ linked });
const refusedLink = (refused: StoreRefusal): DeployLink => ({ refused });

const LossAsked = Schema.Struct({ accept: Schema.Array(Schema.String), version: Schema.String });

/**
 * The one place a Deploy from an agent or a caller without a key becomes a proposal. A caller that can sign, an
 * agent's or keyless caller's Deploy that nothing holds, and a Deploy the Store or this plan can't take all go on as
 * they were. Everything else waits on one person's passkey: unsigned Services the Servers need a passkey for, the
 * agent gate, a Volume-loss review, a Pending value. Its command is bound to the review it was asked against.
 */
export const linkDeploy = Effect.fn("DeployLink.link")(function* (caller: Caller, command: AdmitDeploy) {
  if (caller.role === null || caller.via === "dashboard" || !(yield* hasApprover(caller.organization.id))) return null;
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
  if (open.length > 0 && tap?.kind === "refused") return refusedLink(refusal("conflict", `${tap.message} Nothing deployed.`));
  const blocked = tap?.kind === "ready" ? open.find((entry) => entry.needs !== "version") : undefined;
  if (blocked !== undefined && blocked.needs !== "version") return refusedLink(refusal("conflict", BLOCKED[blocked.needs](blocked.service)));
  const needsPerson = caller.via === "agent" || (tap?.kind === "ready" && open.length > 0) || (pending && covered.size === 0);
  if (!needsPerson) return null;

  const place = yield* readStore(caller.organization.id, { query: "namespace", environment: command.environment }).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(null)),
  );
  if (place === null) return null;
  // A member who can't protect keeps today's ask: a protector approves it, as the Store asks.
  const isProtected = yield* environmentProtected(caller.organization.id, place.environment.project, place.environment.id).pipe(
    Effect.catchTag("StoreRefused", () => Effect.succeed(true)),
  );
  if (isProtected && protectorOnly(caller, place.environment.name) !== null) return null;

  let version = command.version ?? null;
  if (plan !== null) {
    if (version !== null && !namesReview(plan.version, version)) return refusedLink(refusal("conflict", "Your changes moved since you looked. Check them, then deploy again."));
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
  // Nothing ran yet: the Store asks before it admits, so this read of its answer admits nothing.
  const probe = () => callStore(caller.organization.id, caller.userId, { operation: "write", command: bound }, {
    approval: "required", asks: "every_deploy", permissions: callerPermissions(caller), environments: callerEnvironments(caller), protector: protector(caller),
  });
  if (plan !== null) {
    let asked = yield* probe();
    const loss = asked.ok ? Option.none() : asked.refusal.code === "confirmation_required" ? Schema.decodeUnknownOption(LossAsked)(asked.refusal.details) : Option.none();
    if (Option.isSome(loss)) {
      bound = { ...bound, version: loss.value.version, accept_volume_loss: [...loss.value.accept] };
      asked = yield* probe();
    }
    if (asked.ok) return null;
    if (asked.refusal.code !== "approval_required") return refusedLink(asked.refusal);
  }
  const { id } = yield* presentDeployProposal(caller, { ...place.environment, namespace: place.namespace }, bound);
  const { app } = yield* AppConfig;
  const url = approvalUrl(app.url, caller.organization.slug, id);
  return linkedLink({ proposal_id: id, status: "presented", url, message: `Waiting for approval at ${url}. Nothing deployed yet.` });
});
