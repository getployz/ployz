/**
 * An Environment's automatic deploy rule as the browser approves it: every push to one branch deploys with the settings
 * shown, with no prompt. The browser drafts the rule's canonical body with the
 * ployz-core code the CLI and the Servers run, and the person's passkey answers it with a prompt of its own. No session
 * signs a rule: it stands for every future push, so each Server takes only a passkey's own answer or a Device's
 * signature. Cloud keeps and relays the answered rule and can change none of it.
 */
import type { AutoDeployRule, RuleDraft, RuleTerms, RuleView, Signed } from "@ployz/sdk";
import { autoDeployRuleBody, autoDeployRuleEndBody } from "@ployz/sdk/authority";
import { type ApprovalScope, hex } from "#/modules/approval-session/approval-session";
import { PasskeyRefused, signWithPasskey } from "#/modules/approval-session/passkey-statement";

/** Why a rule couldn't be approved, in words the dialog shows as they are. */
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

/**
 * The rule a person approves now in `scope`: the kept rule's next generation when it is for the same repository and
 * branch, else a new rule. `ownerId` is the repository owner's GitHub ID, which each token the rule takes must name.
 */
export function draftRule(view: RuleView, scope: ApprovalScope, ownerId: number, now = Date.now()): RuleDraft {
  const terms = {
    cluster: scope.cluster,
    machines: [...scope.machines],
    owner_id: ownerId,
    new_rule: hex(crypto.getRandomValues(new Uint8Array(16))),
    issued_at: now,
    signer: "passkey" as const,
  };
  try {
    // SAFETY: the Servers report these IDs and the Cluster's key, and `autoDeployRuleBody` parses them as ployz-core's `RuleTerms`.
    return autoDeployRuleBody(view, terms as RuleTerms);
  } catch (error) {
    throw drafting(error instanceof Error ? error.message : null);
  }
}

/** The repository `draft` signs, read from its body: the summary is text to show, quoted when the name is unusual. */
export function draftedRepository(draft: RuleDraft): string {
  // SAFETY: ployz-core drafted the body from an `AutoDeployRule`.
  return (JSON.parse(draft.body) as AutoDeployRule).source.repository;
}

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
    if (error instanceof PasskeyRefused) throw new RuleRefused(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new RuleRefused("Your passkey didn't answer. Try again.");
    throw new RuleRefused("Your browser couldn't use a passkey here.");
  }
}
