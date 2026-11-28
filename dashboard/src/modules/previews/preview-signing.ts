/**
 * Preview settings signed in this browser: the preview rule is drafted with the ployz-core code the Servers check, and
 * the person's passkey answers it with a prompt of its own. Turning previews off signs the rule's ending generation.
 * Cloud only relays the rule view and the answer; each Server checks the rule before it opens a preview.
 */
import type { PreviewRuleView, Signed } from "@ployz/sdk";
import { previewRuleBody, previewRuleEndBody } from "@ployz/sdk/authority";
import type { QueryClient } from "@tanstack/react-query";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { ownerLookup } from "#/modules/deploy-approval/source-step";
import { PasskeyDeclined, signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { preparePreviewSigningServerFn } from "./preview-sign.functions";

/** Why Preview settings weren't signed, in words the sheet shows as they are. */
export class PreviewSignRefused extends Error {}

const DRAFT = "This browser couldn't draft the preview rule. Try again.";

const machineIds = Schema.decodeUnknownSync(Schema.Array(rustMachineIdSchema));

const hex = (bytes: Uint8Array) => [...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("");

async function signing(organizationSlug: string, view: PreviewRuleView) {
  const ready = await preparePreviewSigningServerFn({ data: { organizationSlug, namespace: view.namespace } });
  if (ready.kind === "refused") throw new PreviewSignRefused(ready.message);
  return ready;
}

async function answer(body: string, passkeys: Awaited<ReturnType<typeof signing>>["passkeys"]): Promise<Signed> {
  try {
    return await signWithPasskey(body, "rule", passkeys ?? undefined);
  } catch (error) {
    if (error instanceof PasskeyDeclined) throw new PreviewSignRefused(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new PreviewSignRefused("Your passkey didn't answer. Try again.");
    throw new PreviewSignRefused("Your browser couldn't use a passkey here.");
  }
}

const drafted = <T>(draft: () => T) => {
  try {
    return draft();
  } catch (error) {
    throw new PreviewSignRefused(error instanceof Error && error.message !== "" ? error.message : DRAFT);
  }
};

/** The preview rule for `view` on `servers`, answered by the person's passkey. */
export async function signPreviewRule(input: {
  readonly organizationSlug: string;
  readonly queryClient: QueryClient;
  readonly view: PreviewRuleView;
  readonly repository: string;
  readonly repositoryId: number;
  readonly servers: readonly string[];
}): Promise<Signed> {
  const ready = await signing(input.organizationSlug, input.view);
  const owner = await ownerLookup(input.queryClient, input.organizationSlug)(input.repository, input.repositoryId);
  if ("problem" in owner) throw new PreviewSignRefused(owner.problem);
  const draft = drafted(() => previewRuleBody(input.view, {
    cluster: ready.cluster,
    machines: [...machineIds(input.servers)].sort(),
    owner_id: owner.ownerId,
    new_rule: hex(crypto.getRandomValues(new Uint8Array(16))),
    issued_at: Date.now(),
    signer: "passkey",
  }));
  return answer(draft.body, ready.passkeys);
}

/** The ending generation of the preview rule `view` keeps, answered by the person's passkey. */
export async function signPreviewEnd(organizationSlug: string, view: PreviewRuleView): Promise<Signed> {
  const ready = await signing(organizationSlug, view);
  const draft = drafted(() => previewRuleEndBody(view, { issued_at: Date.now(), signer: "passkey" }));
  return answer(draft.body, ready.passkeys);
}
