/**
 * The statements a person's key signs and a Server verifies, as `ployz_core::authority` writes them, and the automatic
 * deploy rule a passkey answers. Browser code may import this entry: it runs ployz-core as WASM, with no native binding.
 */
export type {
  ApprovedChange,
  AutoDeployRule,
  BuildPolicy,
  JoinApproval,
  Passkey,
  PasskeyJoinDraft,
  PayloadDigest,
  PrivateAccessApproval,
  PrivateAccessTerms,
  ProposalApproval,
  ProposalRef,
  ProposalStep,
  RuleDraft,
  RuleServiceSummary,
  RuleService,
  RuleSource,
  RuleSummary,
  RuleTerms,
  Signed,
  SignerKey,
  StepDigest,
  StepKind,
} from "./generated/payloads";

import type { JoinApproval, Passkey, PasskeyJoinDraft, PrivateAccessTerms, RuleDraft, RuleTerms, RuleView } from "./generated/payloads";

/**
 * The automatic deploy rule a person approves for `view`, the Store's rule view, under `terms`: the canonical body to
 * answer as it is, and what it shows the approver. `services` are the Services it deploys, by default every one on the
 * kept rule's branch, or with no rule every one that builds from GitHub. The kept rule's next generation when it is for
 * the same repository, branch and Environment, else a new rule under `terms.new_rule`. Each Service's digest is taken
 * here, by the ployz-core code the CLI and the Servers run. Throws an Error naming what keeps the Services from one rule.
 */
export function autoDeployRuleBody(view: RuleView, terms: RuleTerms, services?: readonly string[]): RuleDraft;

/** The generation that turns `view`'s kept rule off, signed as `terms.signer` at `terms.issued_at`. Throws when no rule is on. */
export function autoDeployRuleEndBody(view: RuleView, terms: Pick<RuleTerms, "issued_at" | "signer">): RuleDraft;

/**
 * The Private Access approval a passkey answers for `terms`, in canonical form: the auth key sealed to the Cluster in
 * this browser, bound by its digest, which ployz-core takes here as the Servers do. Throws an Error for malformed terms.
 */
export function privateAccessApprovalBody(terms: PrivateAccessTerms): string;

/**
 * The waiting `join` as the one step of a bundle a passkey answers under `operation`, open from `issuedAt` to `expiresAt`
 * (milliseconds since the epoch): the bundle to answer, and the step body a Server is handed with it, each in canonical
 * form, as the Servers check them. Throws an Error for a malformed join.
 */
export function passkeyJoinStep(join: JoinApproval, operation: string, issuedAt: number, expiresAt: number): PasskeyJoinDraft;

/**
 * The key an install line carries for `passkey` (`pk1_…`), as the installer and `ployz key add` read it: a fresh Server
 * trusts it to found or join its Cluster. Throws an Error for a malformed passkey.
 */
export function installKey(passkey: Passkey): string;
