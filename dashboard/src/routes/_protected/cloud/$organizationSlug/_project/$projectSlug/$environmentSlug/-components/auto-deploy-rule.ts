/**
 * An Environment's automatic deploy rule as the browser reads it, and turning it off: the ending generation is drafted
 * with the ployz-core code the CLI and the Servers run, and the person's passkey answers it with a prompt of its own.
 * Turning the rule on is a Save that signs it (see `save-tap.ts`). Cloud keeps and relays the answered rule and can
 * change none of it.
 */
import type { AutoDeployRule, RuleDraft, RuleView, Signed } from "@ployz/sdk";
import { autoDeployRuleEndBody } from "@ployz/sdk/authority";
import { PasskeyDeclined, signWithPasskey } from "#/modules/passkeys/passkey-statement";

/** Why a rule couldn't be turned off, in words the dialog shows as they are. */
export class RuleRefused extends Error {}

/** The rule the Environment keeps, ended or not, or null before the first approval. */
export function keptRule(view: Pick<RuleView, "rule">): AutoDeployRule | null {
  if (view.rule === null || view.rule === undefined) return null;
  try {
    // SAFETY: the Store keeps only a rule body it parsed as an `AutoDeployRule`.
    return JSON.parse(view.rule.body) as AutoDeployRule;
  } catch {
    return null;
  }
}

/** Whether pushes deploy under the kept rule now. */
export const ruleOn = (view: Pick<RuleView, "rule">) => {
  const kept = keptRule(view);
  return kept !== null && !kept.ended;
};

/** What ployz-core said keeps the rule from being drafted, as the dialog shows it. */
const drafting = (message: string | null) => new RuleRefused(message || "This browser couldn't draft the rule. Try again.");

/** The generation that turns the kept rule off. */
export function draftEnd(view: RuleView, now = Date.now()): RuleDraft {
  try {
    return autoDeployRuleEndBody(view, { issued_at: now, signer: "passkey" });
  } catch (error) {
    throw drafting(error instanceof Error ? error.message : null);
  }
}

/** `draft`, answered by the person's passkey with a prompt of its own, ready to keep. */
export async function answerRule(draft: RuleDraft): Promise<Signed> {
  try {
    return await signWithPasskey(draft.body, "rule");
  } catch (error) {
    if (error instanceof PasskeyDeclined) throw new RuleRefused(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new RuleRefused("Your passkey didn't answer. Try again.");
    throw new RuleRefused("Your browser couldn't use a passkey here.");
  }
}
