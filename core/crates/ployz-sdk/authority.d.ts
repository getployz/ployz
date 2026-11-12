/**
 * The statements a person's key signs and a Server verifies, as `ployz_core::authority` writes them, the deploy
 * approval a browser key signs, the automatic deploy rule a passkey answers, and the review of a preview a person signs. Browser code may import this entry: it runs ployz-core as WASM, with no native binding.
 */
export type {
  ApprovedChange,
  ApprovedService,
  AutoDeployRule,
  BuildPolicy,
  DeployApproval,
  DeployTerms,
  DraftWorkflow,
  GrantBody,
  PayloadDigest,
  PrivateAccessApproval,
  PrivateAccessTerms,
  Teardown,
  TeardownApproval,
  TeardownScope,
  TeardownTerms,
  ProposalApproval,
  ProposalRef,
  ProposalStep,
  RestartApproval,
  ReviewDraft,
  ReviewTerms,
  RuleDraft,
  RuleServiceSummary,
  RuleService,
  RuleSource,
  RuleSummary,
  RuleTerms,
  SessionGrant,
  Signed,
  SignerKey,
  StepDigest,
  StepKind,
  UpgradeChannel,
  VolumeApproval,
  VolumeDraft,
  VolumeTerms,
  VolumeWorkflow,
} from "./generated/payloads";

import type { ApprovedChange, DeployTerms, PlanView, PreviewReviewView, PrivateAccessTerms, ReviewDraft, ReviewTerms, RuleDraft, RuleTerms, RuleView, TeardownTerms, VolumeDraft, VolumeTerms } from "./generated/payloads";

/**
 * The deploy approval a browser key signs for `plan`, a Store plan read with `target`, under `terms`: the canonical body
 * to sign as it is, what each Service shows the approver, and one line per Service followed by each setting it signs,
 * indented. Each spec's digest is taken here, by the ployz-core code the CLI and the Servers run. Throws an Error for a
 * plan read without `target`, a Service built from source, an image named by a tag, or malformed terms.
 */
export function deployApprovalBody(plan: Pick<PlanView, "namespace" | "target">, terms: DeployTerms): { body: string; changes: ApprovedChange[]; lines: string[] };

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
 * The teardown a passkey answers for `terms`, in canonical form, and the lines a person reads before answering, rendered
 * from that body as the Servers read it. `names.servers` gives each Server the name Cloud knows it by, and `names.volumes`
 * each Docker Volume its Volume's name, shown beside it. Throws an Error for malformed terms.
 */
export function teardownBody(
  terms: TeardownTerms,
  names?: { servers?: Record<string, string>; volumes?: Record<string, string> },
): { body: string; lines: string[] };

/**
 * The review a person signs of `view`, the Store's view of a PR Environment's preview, for its head under `terms`: the
 * canonical body to sign as it is, whether any Service opens a secret (then only a passkey may sign it), and one line per
 * Service followed by each setting its template signs, indented. Each spec's digest is taken here, by the ployz-core code the Servers run. Throws an Error naming what keeps
 * the preview from a review.
 */
export function previewReviewBody(view: PreviewReviewView, terms: Pick<ReviewTerms, "cluster" | "issued_at" | "expires_at">): ReviewDraft;

/**
 * The Volume approval a passkey answers for `terms`: the canonical body to answer as it is, every line the person reads,
 * rendered from that body by the ployz-core code the CLI and the Servers run, and for a Move the spec the target starts.
 * Throws an Error for malformed terms, or a Move whose Service runs an image not pinned by digest.
 */
export function volumeApprovalBody(terms: VolumeTerms): VolumeDraft;
