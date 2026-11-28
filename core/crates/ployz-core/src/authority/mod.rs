//! Statements a customer's own key signs and a Machine verifies for itself.
//!
//! Cloud carries these but cannot mint them: a Machine checks the signature against the
//! keys it trusts, checks every bound field against the request in hand, and keeps
//! its own record of what it already acted on. Joins, key changes, signed versions,
//! automatic deploy rules and copies of a Volume each have a statement type here, and
//! [`TrustedKeys::covers`] says which trusted key may sign each. A proposal signs an ordered plan
//! of steps once, and each step travels unsigned with the bundle in its envelope.

pub mod admission;
pub mod birth;
pub mod canonical;
pub mod copy;
pub mod coverage;
pub mod deploy;
pub mod join;
pub mod key;
pub mod keys;
mod open;
pub mod preview_draft;
pub mod private_access;
pub mod proposal;
pub mod refusal;
pub mod rule;
pub mod rule_draft;
pub mod scope;
pub mod shown;
pub mod trusted_key;
pub mod validity;
pub mod version;
pub mod version_draft;

pub use admission::{DataReach, needs_signature, reach};
pub use birth::{
    Against, Approved, Before, BirthError, Births, Held, HeldContainer, MountLines, Mounts,
    PlannedVolume, PlannedVolumes, Unknown,
};
pub use canonical::{CanonicalError, Timestamp};
pub use copy::{CopyFacts, CopyStep, CopyVerified, CopyWindow};
pub use deploy::{ApprovedChange, PayloadDigest};
pub use join::{JoinApproval, Member, PasskeyJoinDraft};
pub use key::{
    GRANT_MOVED, KeyChange, KeyFacts, KeyMember, KeyRef, KeyStep, KeyVerified, NewKey, Prior,
};
pub use keys::{
    Approver, KeySignature, OwnPrompt, Purpose, Signed, Signer, SignerKey, SigningPublicKey,
    Statement,
};
pub use preview_draft::{PreviewBasis, PreviewSummary};
pub use private_access::{
    AuthKeyDigest, KeyApproval, PrivateAccessApproval, PrivateAccessFacts, PrivateAccessTerms,
    key_approval_challenge,
};
pub use proposal::{ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind};
pub use refusal::{AuthorityRefusal, MismatchField};
pub use rule::{
    AttestKey, AutoDeployRule, BuildClaim, BuildPolicy, BuildStage, BuiltImageDigest,
    ChargedAuthority, CommitApproval, CommitAuthority, CommitSha, GithubClaims, GithubSource,
    KeptRule, PREVIEW_NAMESPACE, PinnedService, PolicyDigest, PreviewTerms, RecipientKey,
    RegistryPin, RegistrySource, RuleEvidence, RuleFacts, RuleId, RuleIdentity, RuleProof,
    RuleService, RuleSource, SealedDigest, VerifiedBuild, preview_template,
};
pub use rule_draft::{
    RuleBasis, RuleDraft, RuleDraftError, RuleImageView, RuleSummary, RuleTerms, SourceSummary,
};
pub use scope::{
    Ask, Cap, CapSet, Grant, GrantKind, GrantVolume, KeyKind, KeyName, KeyPlace, KeyPlaces,
    KeyScope, PlaceService, Reach, TrustedGrant, ValueReach,
};
pub use trusted_key::{
    Passkey, PasskeyAccount, PasskeyApprover, PasskeyAssertion, PasskeyPublicKey, TrustedKey,
    TrustedKeys, TrustedPasskey,
};
pub use version::{Budget, Charged, ServiceVersion, VersionAdmitted, VersionFacts};
