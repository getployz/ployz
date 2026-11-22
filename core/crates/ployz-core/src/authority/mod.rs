//! Statements a customer's own key signs and a Machine verifies for itself.
//!
//! Cloud carries these but cannot mint them: a Machine checks the signature against the
//! keys it trusts, checks every bound field against the request in hand, and keeps
//! its own record of what it already acted on. Joins, key changes, signed versions and
//! automatic deploy rules each have a statement type here, and [`TrustedKeys::covers`] says
//! which trusted key may sign each. A proposal signs an ordered plan
//! of steps once, and each step travels unsigned with the bundle in its envelope.

pub mod admission;
pub mod birth;
pub mod canonical;
pub mod coverage;
pub mod deploy;
pub mod join;
pub mod key;
pub mod keys;
mod open;
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

pub use admission::{DataReach, needs_signature};
pub use birth::{BirthError, Births, Mounts, PlannedVolume, PlannedVolumes};
pub use canonical::{CanonicalError, Timestamp};
pub use deploy::{ApprovedChange, PayloadDigest};
pub use join::{JoinApproval, Member};
pub use key::{KeyChange, KeyFacts, KeyMember, KeyRef, KeyStep, KeyVerified, NewKey, Prior};
pub use keys::{
    Approver, KeySignature, OwnPrompt, Purpose, Signed, Signer, SignerKey, SigningPublicKey,
    Statement,
};
pub use private_access::{
    AuthKeyDigest, KeyApproval, PrivateAccessApproval, PrivateAccessFacts, PrivateAccessTerms,
    key_approval_challenge,
};
pub use proposal::{ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind};
pub use refusal::AuthorityRefusal;
pub use rule::{
    AttestKey, AutoDeployRule, BuildClaim, BuildPolicy, BuildStage, ClaimRule, CommitAuthority,
    CommitSha, GithubClaims, ImageDigest, PolicyDigest, RecipientKey, RuleEvidence, RuleFacts,
    RuleId, RuleIdentity, RuleProof, RuleService, RuleSource, SealedDigest, VerifiedBuild,
};
pub use rule_draft::{RuleBasis, RuleDraft, RuleDraftError, RuleSummary, RuleTerms};
pub use scope::{
    Ask, Cap, CapSet, Grant, GrantKind, GrantVolume, KeyKind, KeyName, KeyPlace, KeyPlaces, KeyScope,
    PlaceService, Reach, TrustedGrant, ValueReach,
};
pub use trusted_key::{
    Passkey, PasskeyAccount, PasskeyApprover, PasskeyAssertion, PasskeyPublicKey, TrustedKey,
    TrustedKeys, TrustedPasskey,
};
pub use version::{Budget, ServiceVersion, VersionAdmitted, VersionFacts};
