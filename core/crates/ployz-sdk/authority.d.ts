/**
 * The statements a person's key signs and a Server verifies, as `ployz_core::authority` writes them, and the automatic
 * deploy rule a passkey answers. Browser code may import this entry: it runs ployz-core as WASM, with no native binding.
 */
export type {
  ApprovedChange,
  AutoDeployRule,
  BuildPolicy,
  GithubSource,
  JoinApproval,
  Passkey,
  PasskeyJoinDraft,
  PayloadDigest,
  PreviewSummary,
  PrivateAccessApproval,
  PrivateAccessTerms,
  ProposalApproval,
  ProposalRef,
  ProposalStep,
  RegistrySource,
  RuleDraft,
  RuleImageView,
  RuleServiceSummary,
  RuleService,
  RuleSource,
  RuleSummary,
  RuleTerms,
  Signed,
  SignerKey,
  SourceNeed,
  SourceSummary,
  StepDigest,
  StepKind,
  TrashAction,
  TrashApproval,
  TrashTerms,
  VersionsDraft,
  VersionStep,
} from "./generated/payloads";

import type {
  HeldContainer,
  JoinApproval,
  MachineDetails,
  Parent,
  Passkey,
  PasskeyJoinDraft,
  PlanView,
  PreviewRuleView,
  PrivateAccessTerms,
  RequestedServiceSpec,
  RegistrySource,
  RuleDraft,
  RuleImageView,
  RuleSourceView,
  RuleTerms,
  RuleView,
  Signed,
  SourceNeed,
  TrashTerms,
  VersionsDraft,
} from "./generated/payloads";

/** The generation that turns `view`'s kept rule off, signed as `terms.signer` at `terms.issued_at`. Throws when no rule is on. */
export function autoDeployRuleEndBody(view: RuleView, terms: Pick<RuleTerms, "issued_at" | "signer">): RuleDraft;

/**
 * The preview rule a passkey answers for the Store's `preview_rule` view: every Git Service of the repository builds under
 * it, every Service that runs an image runs it pinned by digest, and each preview opens the start-from's Sealed values.
 * The kept rule's next generation when it is for the same repository and start-from. `terms.machines` are the Servers
 * previews run on; `terms.new_rule` is 16 random bytes from this browser. Throws an Error naming a Service no preview
 * rule can cover, such as an image that names no digest.
 */
export function previewRuleBody(view: PreviewRuleView, terms: RuleTerms): RuleDraft;

/** The generation that turns `view`'s kept preview rule off. Throws when previews are off. */
export function previewRuleEndBody(view: PreviewRuleView, terms: Pick<RuleTerms, "issued_at" | "signer">): RuleDraft;

/**
 * The Private Access approval a passkey answers for `terms`, in canonical form: the auth key sealed to the Cluster in
 * this browser, bound by its digest, which ployz-core takes here as the Servers do. Throws an Error for malformed terms.
 */
export function privateAccessApprovalBody(terms: PrivateAccessTerms): string;

/**
 * The Trash approval a passkey answers for `terms`, in canonical form: the one Server and the exact entries it restores or
 * empties, sorted and each once, by the ployz-core code the Servers run. Throws an Error for malformed terms.
 */
export function trashApprovalBody(terms: TrashTerms): string;

/**
 * The waiting `join` as the one step of a bundle a passkey answers under `operation`, open from `issuedAt` to `expiresAt`
 * (milliseconds since the epoch): the bundle to answer, and the step body a Server is handed with it, each in canonical
 * form, as the Servers check them. Throws an Error for a malformed join.
 */
export function passkeyJoinStep(join: JoinApproval, operation: string, issuedAt: number, expiresAt: number): PasskeyJoinDraft;

/**
 * One version of each of `asked.services` a person's passkey signs in one prompt: the bundle to answer, each step body
 * with the births and image the Store records beside it, and the lines the person reads, each by the ployz-core code the
 * Servers run. With `source`, the bundle's last step is the Environment's automatic deploy rule for `source.target`,
 * unless the kept rule already approves it or was ended; null when that leaves nothing to sign. `pins` maps each tag image to the `tag@sha256:…` it runs as, which `pinImages` looks up. `seed` is 16
 * random bytes from this browser. `against` is what each mount is compared with, with `parents` as the Store's parent
 * query returns them. `confirm` lists the mounts the person's answer says yes to. Throws an
 * Error, before any bundle, for a tag with no pin, a pin that isn't the tag itself by digest, a Volume from another
 * environment, or data from before births that nothing vouches for.
 */
export function versionSteps(asked: {
  cluster: string;
  namespace: string;
  volumes?: PlanView["volumes"];
  services: readonly RequestedServiceSpec[];
  pins?: Readonly<Record<string, string>>;
  operation: string;
  issued_at: number;
  expires_at: number;
  seed: readonly number[];
  against: Against;
  source?: SourceAsked;
}): VersionsDraft | null;

/**
 * What a Save of the Store's staged rule `view` needs for its Source step, read from the view alone before any Server or
 * passkey is reached: `"none"` when the rule was ended, no pushed Service builds from one branch, or the kept rule
 * approves what a push deploys wherever it may place it; `"servers"` when the kept rule approves the settings and only
 * the Servers it names may be out of date; `"rule"` otherwise. Throws an Error for a malformed view.
 */
export function sourceNeed(view: RuleView): SourceNeed;

/**
 * What a Source step drafts the rule from, as the Store's rule view gives it: the kept rule, where each Service builds
 * from, the settings a push deploys, every Server of the Cluster by ID, and the repository owner's GitHub ID. The builder
 * is the kept rule's when absent, else Ployz's own release.
 */
export type SourceAsked = {
  rule: Signed | null;
  sources: readonly RuleSourceView[];
  /** The Saved Services that run a registry's image, which a registry rule deploys. */
  images?: readonly RuleImageView[];
  target: readonly RequestedServiceSpec[];
  machines: readonly string[];
  owner_id: number;
  builder?: { ref: string; sha: string };
  /**
   * The branch, or registry repository, the person turns automatic deploys on for: a rule they ended starts again, and
   * it follows this one.
   */
  on?: PushBranch | PushImages;
};

/** A branch a rule follows: the repository's GitHub ID and the branch's name. */
export type PushBranch = { repository_id: number; branch: string };

/** A registry repository whose every pushed image a rule deploys. */
export type PushImages = { registry: RegistrySource };

/**
 * What each mount is compared with: each Service's last approved version, the keys one enforcing Server trusts, the
 * mounts of the Namespace's Containers as the Servers that answered list them, and each Server that didn't answer, by name.
 */
export type Against = {
  parents?: Readonly<Record<string, Parent>>;
  trusted: Pick<MachineDetails, "devices" | "passkeys" | "grants">;
  containers: readonly HeldContainer[];
  unanswered?: readonly string[];
};

/**
 * The key an install line carries for `passkey` (`pk1_…`), as the installer and `ployz key add` read it: a fresh Server
 * trusts it to found or join its Cluster. Throws an Error for a malformed passkey.
 */
export function installKey(passkey: Passkey): string;
