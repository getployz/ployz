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
  PrivateAccessTerms,
  RequestedServiceSpec,
  RuleDraft,
  RuleTerms,
  RuleView,
  VersionsDraft,
} from "./generated/payloads";

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
 * One version of each of `asked.services` a person's passkey signs in one prompt: the bundle to answer, each step body
 * with the births and image the Store records beside it, and the lines the person reads, each by the ployz-core code the
 * Servers run. `pins` maps each tag image to the `tag@sha256:…` it runs as, which `pinImages` looks up. `seed` is 16
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
}): VersionsDraft;

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
