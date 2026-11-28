import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { gate } from "#/modules/deploy-approval/deploy-tap.server";
import { requireAccess } from "#/modules/organization/access.server";
import { readClusterSealing } from "#/modules/sealing/cluster-sealing.server";

/** What each Server that enforces device approval needs to take a preview rule a passkey signed. */
const PREVIEW_RULE_NEEDS = ["ployz.container.deploy-rule.v1", "ployz.container.preview-rule.v1"];

/**
 * What signing Preview settings, or a commit a preview deploys, needs: the Cluster's key the rule names and the person's passkeys every enforcing
 * Server trusts (null when none enforces, so any passkey answers). Anything else is why it can't, in words the sheet
 * shows.
 */
export type PreviewSigning =
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "ready"; readonly cluster: string; readonly passkeys: readonly { readonly credentialId: string; readonly code: string }[] | null };

export const preparePreviewSigning = Effect.fn("PreviewSigning.prepare")(function* (actor: Actor, input: {
  readonly organizationSlug: string;
  /** The start-from's Namespace, whose Containers the Servers list. */
  readonly namespace: string;
}) {
  const access = yield* requireAccess(actor, input.organizationSlug, "changes:deploy");
  if (access.kind !== "member") return { kind: "refused", message: "Ask a member of your organization to approve this." } satisfies PreviewSigning;
  const tap = yield* gate(actor.userId, access.organization.id, [], {
    namespace: input.namespace, parents: Effect.succeed({}), needs: PREVIEW_RULE_NEEDS,
  });
  if (tap.kind === "refused") return tap satisfies PreviewSigning;
  if (tap.kind === "ready") return { kind: "ready", cluster: tap.cluster, passkeys: tap.passkeys } satisfies PreviewSigning;
  // No Server checks approvals: the rule still names the Cluster's key, as Cloud recorded it.
  const sealing = yield* readClusterSealing(access.organization.id);
  if (sealing.state !== "ready") return { kind: "refused", message: "Add a server first." } satisfies PreviewSigning;
  return { kind: "ready", cluster: sealing.publicKey, passkeys: null } satisfies PreviewSigning;
});
