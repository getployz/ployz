/**
 * An Environment's automatic deploy rule as the browser approves it: every push to one branch deploys with the settings
 * shown, with no prompt. The browser drafts the rule's canonical body with the
 * ployz-core code the CLI and the Servers run, and the person's passkey answers it with a prompt of its own. No session
 * signs a rule: it stands for every future push, so each Server takes only a passkey's own answer or a Device's
 * signature. Cloud keeps and relays the answered rule and can change none of it.
 */
import type { AutoDeployRule, RuleDraft, RuleTerms, RuleView, Signed } from "@ployz/sdk";
import { autoDeployRuleBody, autoDeployRuleEndBody } from "@ployz/sdk/authority";
import { hex } from "#/lib/bytes";
import { PasskeyDeclined, signWithPasskey } from "#/modules/passkeys/passkey-statement";

/** Where a rule stands: the Cluster, by its Sealing public key as Servers report it, and every Server in it. */
export type RuleScope = { readonly cluster: string; readonly machines: readonly string[] };

type ScopeMachine = {
  readonly id: string;
  readonly name: string;
  readonly sealing: { readonly kind: string; readonly public_key?: string } | null;
  readonly capabilities: readonly string[];
};

const RULE_CAPABILITIES = ["ployz.container.deploy-rule.v1", "ployz.rule.passkey.v1"];

const heldKey = (machine: ScopeMachine) => machine.sealing?.kind === "held" && machine.sealing.public_key ? machine.sealing.public_key : undefined;

/**
 * The scope a rule names, and the first Server that can't take one from a browser. The Cluster is the one the Servers
 * `near` (those running the Environment) hold the Sealing Key of, or the only one; the scope names every Server in it,
 * since a Deploy may place on any of them. Null when Cloud can't tell which Cluster it is.
 */
export function ruleScope(near: readonly string[], machines: readonly ScopeMachine[]) {
  const running = new Set(near);
  const keys = new Set(machines.map(heldKey).filter((key) => key !== undefined));
  const cluster = machines.filter((machine) => running.has(machine.id)).map(heldKey).find((key) => key !== undefined)
    ?? (keys.size === 1 ? [...keys][0] : undefined);
  const members = cluster === undefined ? machines : machines.filter((machine) => heldKey(machine) === cluster);
  return {
    scope: cluster === undefined ? null : { cluster, machines: members.map((machine) => machine.id).sort() },
    older: members.find((machine) => !RULE_CAPABILITIES.every((capability) => machine.capabilities.includes(capability)))?.name ?? null,
  };
}

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
export function draftRule(view: RuleView, scope: RuleScope, ownerId: number, now = Date.now()): RuleDraft {
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
    if (error instanceof PasskeyDeclined) throw new RuleRefused(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new RuleRefused("Your passkey didn't answer. Try again.");
    throw new RuleRefused("Your browser couldn't use a passkey here.");
  }
}
