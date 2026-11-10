//! Statements a customer's own key signs and a Machine verifies for itself.
//!
//! Cloud carries these but cannot mint them: a Machine checks the signature against the
//! keys it trusts, checks every bound field against the request in hand, and keeps
//! its own record of what it already acted on. The first statement is the Service
//! restart approval; joins, deploys and other workflows add their own statement types here. A
//! workflow statement may also be signed by a browser key that a trusted passkey lent a
//! session grant, which travels in the same envelope. A proposal signs an ordered plan of
//! such statements once, and each step travels unsigned with the bundle in its envelope.

pub mod canonical;
pub mod coverage;
pub mod deploy;
pub mod join;
pub mod keys;
pub mod preview;
pub mod private_access;
pub mod proposal;
pub mod refusal;
pub mod restart;
pub mod rule;
pub mod rule_draft;
pub mod session;
pub mod starting_set;
pub mod trusted_key;
pub mod validity;
pub mod volume;

pub use canonical::{CanonicalError, Timestamp};
pub use deploy::{
    ApprovedChange, ApprovedService, BrowserDraft, BrowserDraftError, DeployApproval,
    DeployApproved, DeployFacts, DeployTerms, PayloadDigest,
};
pub use join::JoinApproval;
pub use keys::{Approver, KeySignature, Purpose, Signed, Signer, SigningPublicKey, Statement};
pub use preview::{
    CheckedReview, PreviewCreation, PreviewFacts, PreviewKey, PreviewProof, PreviewReview,
    PreviewRule, PreviewService, Review, ReviewDraft, ReviewDraftError, ReviewSigner, ReviewTerms,
    ReviewedService,
};
pub use private_access::{
    AuthKeyDigest, PrivateAccessApproval, PrivateAccessFacts, PrivateAccessTerms,
};
pub use proposal::{ProposalApproval, ProposalRef, ProposalStep, StepDigest, StepKind};
pub use refusal::AuthorityRefusal;
pub use restart::{
    Approved, OwnPrompt, RestartApproval, RestartFacts, RestartStep, SignerKey, WorkflowKind,
};
pub use rule::{
    AttestKey, AutoDeployRule, BuildClaim, BuildPolicy, BuildStage, ClaimRule, CommitAuthority,
    CommitSha, GithubClaims, ImageDigest, PolicyDigest, RecipientKey, ReviewDigest, RuleFacts,
    RuleId, RuleIdentity, RuleProof, RuleService, RuleSource, SealedDigest,
};
pub use rule_draft::{RuleBasis, RuleDraft, RuleDraftError, RuleSummary, RuleTerms};
pub use session::{ActionFamily, GrantBody, SessionGrant};
pub use starting_set::{Member, StartingSet};
pub use trusted_key::{
    Passkey, PasskeyAccount, PasskeyApprover, PasskeyAssertion, PasskeyPublicKey, TrustedKeys,
    TrustedPasskey,
};
pub use volume::{VolumeApproval, VolumeApproved, VolumeFacts, VolumeStep, VolumeWorkflow};
