//! Why a Machine refuses a signed statement. Each reason names what the caller can fix.

use thiserror::Error;

use super::admission::DataReach;
use super::canonical::Timestamp;
use super::keys::{Purpose, SigningPublicKey};
use super::scope::KeyKind;
use crate::{DeviceCode, DockerVolumeName};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AuthorityRefusal {
    #[error("the {purpose} approval carries no signature")]
    Unsigned { purpose: Purpose },
    #[error("the {purpose} approval is malformed: {problem}")]
    Malformed { purpose: Purpose, problem: String },
    #[error("key {} is not trusted on this Machine", .key.code())]
    UnknownKey { key: SigningPublicKey },
    #[error("the signature by device {} does not verify", .key.code())]
    BadSignature { key: SigningPublicKey },
    #[error("the passkey is not approved on this Machine")]
    UnknownPasskey,
    /// The key is trusted here, but its kind or scope doesn't reach what it signed.
    #[error("the {kind} key {code} may not approve {what} here")]
    OutOfScope {
        code: DeviceCode,
        kind: KeyKind,
        what: &'static str,
    },
    /// The key is a CI key made before every key named its Environments, so it reaches
    /// none.
    #[error(
        "the CI key {code} names no environment, so it may approve nothing: make a new one with ployz key add --ci --env"
    )]
    Unscoped { code: DeviceCode },
    /// The key is a grant whose Volume list names no entry for a Volume the spec mounts.
    #[error("the {kind} key {code} may not mount Volume {volume} here")]
    VolumeNotGranted {
        code: DeviceCode,
        kind: KeyKind,
        volume: DockerVolumeName,
    },
    #[error("the spec {reach}, and nothing signed it")]
    NeedsSignature { reach: DataReach },
    /// No Device or rule signed the spec, but its Service has run signed in this Cluster,
    /// and such a name answers only for a signed Container.
    #[error("{service} has run signed, and nothing signed this spec")]
    SignedName { service: crate::QualifiedService },
    #[error("the approval names {field} this request does not match")]
    Mismatch { field: MismatchField },
    #[error(
        "the rule pins getployz/build {sha}, which installs a ployz no Ployz release built; approve again under Automatic deploys"
    )]
    RetiredBuilder { sha: String },
    #[error("the approval expired")]
    Expired,
    #[error("this Machine holds no Sealing Key, so it cannot tell which Cluster was approved")]
    NoClusterIdentity,
    #[error("this GitHub token already minted a Build Grant; a build asks for a new one")]
    ProofReplayed,
    #[error("the request carries more signatures than this Machine checks for one request")]
    Overspent,
    /// A forget reached this Machine for the Service, signed at `floor`, after the approval.
    #[error("the approval was signed before {}, when this Service's older versions were forgotten", .floor.utc())]
    Forgotten { floor: Timestamp },
}

/// The first of `checks` that does not hold, as a [`AuthorityRefusal::Mismatch`] naming its field.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] for the first `false` check, in order.
pub(super) fn first_mismatch<const N: usize>(
    checks: [(MismatchField, bool); N],
) -> Result<(), AuthorityRefusal> {
    match checks.into_iter().find(|(_, holds)| !holds) {
        Some((field, _)) => Err(AuthorityRefusal::Mismatch { field }),
        None => Ok(()),
    }
}

/// The part of a request a signed statement names differently. Its text completes
/// "the approval names … this request does not match".
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MismatchField {
    Cluster,
    Environment,
    /// A forget names the system Namespace, whose Containers the daemon itself keeps.
    SystemNamespace,
    /// Signed more than ten minutes past this Machine's clock.
    SigningTime,
    /// A Container a CI or agent key admitted before this Machine kept what it reaches.
    KeptReach,
    /// A kept Container whose every admitting key was rotated away or removed here.
    KeptKey,
    /// A CI or agent key's version names no parent, or none of the statements presented
    /// with that digest is a parentless version a person or full CLI signed for the same
    /// Service.
    Parent,
    /// A version that edits a parent also asks to forget older ones.
    ForgetParent,
    Signer,
    OneAdmittingMachine,
    JoiningMachine,
    ApprovedDevices,
    ApprovedGrants,
    /// A Device or passkey added with a prior, which only a CI or agent key's grant has.
    Grant,
    SealingRecipient,
    MachineAllocation,
    AdmittingMachine,
    ApprovedPasskeys,
    Proposal,
    KeyScope,
    ThisMachine,
    Passkey,
    AuthKey,
    Step,
    Payload,
    Service,
    VolumesEnvironment,
    Hook,
    Image,
    AttestKey,
    Stage,
    Commit,
    /// A person's approval of one commit for one preview.
    CommitApproval,
    Policy,
    Generation,
    Rule,
    EndedRule,
    Machine,
    ClaimRunnerEnvironment,
    ClaimJobWorkflowRef,
    ClaimJobWorkflowSha,
    ClaimWorkflowRef,
    ClaimEventName,
    ClaimRef,
    ClaimRefType,
    ClaimRepositoryOwnerId,
    ClaimRepositoryId,
    /// A push by a GitHub user a preview rule doesn't name.
    ClaimActor,
    /// A preview's Ingress hostname that doesn't name the preview.
    Hostname,
    PasskeySignature,
    UserVerification,
    RelyingParty,
    Assertion,
    Origin,
    Challenge,
    AssertionType,
    IssueTime,
    Lifetime,
    Forget,
    HookRun,
    SourceServer,
    KeptAdmission,
    Admission,
    VolumeStart,
    /// A Volume start whose spec doesn't mount the Volume it names.
    StartedVolume,
    UnusedSubnet,
    JoinConfirmation,
    VerifiedBuild,
    Audience,
    GitHubClient,
    ApprovalRecord,
    SealedValues,
    Holder,
    StartTime,
    Namespace,
    Branch,
    Repository,
    /// A rule of the other source: a GitHub rule for a registry image, or a registry rule
    /// for a build.
    RuleSource,
    /// The digest a registry answered for the image a registry rule's Container names.
    RegistryDigest,
    PendingJoinCapacity,
    BootstrapPeers,
    FirstKey,
    JoinRestrictions,
    SealedTrust,
    /// A Copy step's source Volume.
    CopySource,
    /// A Copy step's restore point.
    RestorePoint,
    /// A Copy step's new Volume.
    CopyDestination,
}

impl MismatchField {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Cluster => "a Cluster",
            Self::Environment => "an Environment",
            Self::SystemNamespace => "the system Namespace",
            Self::SigningTime => "a signing time",
            Self::KeptReach => "a kept reach",
            Self::KeptKey => "a kept key",
            Self::Parent => "a parent version",
            Self::ForgetParent => "a forget on an edit",
            Self::Signer => "the signer",
            Self::OneAdmittingMachine => "one admitting Machine",
            Self::JoiningMachine => "a joining Machine",
            Self::ApprovedDevices => "approved Devices",
            Self::ApprovedGrants => "approved grants",
            Self::Grant => "a grant",
            Self::SealingRecipient => "a sealing recipient",
            Self::MachineAllocation => "a Machine allocation",
            Self::AdmittingMachine => "an admitting Machine",
            Self::ApprovedPasskeys => "approved passkeys",
            Self::Proposal => "a proposal",
            Self::KeyScope => "a key scope",
            Self::ThisMachine => "this Machine",
            Self::Passkey => "a passkey",
            Self::AuthKey => "an auth key",
            Self::Step => "a step",
            Self::Payload => "a payload",
            Self::Service => "a Service",
            Self::VolumesEnvironment => "a Volume's Environment",
            Self::Hook => "a hook",
            Self::Image => "an image",
            Self::AttestKey => "the attest key",
            Self::Stage => "a stage",
            Self::Commit => "a commit",
            Self::CommitApproval => "a commit approval",
            Self::Policy => "a policy",
            Self::Generation => "a generation",
            Self::Rule => "a rule",
            Self::EndedRule => "an ended rule",
            Self::Machine => "a Machine",
            Self::ClaimRunnerEnvironment => "runner_environment",
            Self::ClaimJobWorkflowRef => "job_workflow_ref",
            Self::ClaimJobWorkflowSha => "job_workflow_sha",
            Self::ClaimWorkflowRef => "workflow_ref",
            Self::ClaimEventName => "event_name",
            Self::ClaimRef => "ref",
            Self::ClaimRefType => "ref_type",
            Self::ClaimRepositoryOwnerId => "repository_owner_id",
            Self::ClaimRepositoryId => "repository_id",
            Self::ClaimActor => "actor_id",
            Self::Hostname => "a hostname",
            Self::PasskeySignature => "a passkey signature",
            Self::UserVerification => "user verification",
            Self::RelyingParty => "a relying party",
            Self::Assertion => "an assertion",
            Self::Origin => "an origin",
            Self::Challenge => "a challenge",
            Self::AssertionType => "an assertion type",
            Self::IssueTime => "an issue time",
            Self::Lifetime => "a lifetime",
            Self::Forget => "a forget",
            Self::HookRun => "a hook run",
            Self::SourceServer => "the source Server",
            Self::KeptAdmission => "a kept admission",
            Self::Admission => "an admission",
            Self::VolumeStart => "a Volume start",
            Self::StartedVolume => "the Volume to start on",
            Self::UnusedSubnet => "an unused subnet",
            Self::JoinConfirmation => "a join confirmation",
            Self::VerifiedBuild => "a verified build",
            Self::Audience => "an audience",
            Self::GitHubClient => "a GitHub client",
            Self::ApprovalRecord => "the approval record",
            Self::SealedValues => "the Sealed values",
            Self::Holder => "a holder",
            Self::StartTime => "a start time",
            Self::Namespace => "a Namespace",
            Self::Branch => "a branch",
            Self::Repository => "a repository",
            Self::RuleSource => "a rule source",
            Self::RegistryDigest => "a registry digest",
            Self::PendingJoinCapacity => "pending join capacity",
            Self::BootstrapPeers => "bootstrap peers",
            Self::FirstKey => "the joiner's First Key",
            Self::JoinRestrictions => "join restrictions",
            Self::SealedTrust => "sealed trust",
            Self::CopySource => "the Volume to copy",
            Self::RestorePoint => "a restore point",
            Self::CopyDestination => "the new Volume",
        }
    }

    /// What a refused deploy says to the person who ran it, when this field has its own words.
    #[must_use]
    pub const fn deploy_refusal(self) -> Option<&'static str> {
        Some(match self {
            Self::Payload => "This service's settings differ from what the device approved.",
            Self::Image => "This server runs only images pinned by digest.",
            Self::Service => "The device didn't approve this service.",
            Self::Hook => "The device approved no pre-deploy command for this service.",
            Self::HookRun => "This deploy already ran its pre-deploy command here.",
            Self::VolumeStart => {
                "This server starts a service on a moved or restored Volume only from a deploy an approved device signed."
            }
            Self::StartedVolume => {
                "The approved settings of this service don't mount the volume this start names."
            }
            Self::SourceServer => {
                "This server copies a volume's data only from another server of its cluster."
            }
            Self::Admission => {
                "This server kept no record of who approved this container. Deploy it again."
            }
            Self::KeptAdmission => {
                "This container was made before this server's upgrade, so the server starts it only from a deploy an approved device signed. Deploy the service again with a current CLI."
            }
            Self::Generation => {
                "A newer approval of the automatic deploy rule replaced the one this container ran under."
            }
            Self::SigningTime => {
                "This deploy was signed more than 10 minutes ahead of this server's clock. Check the clock of the device that signed it, and this server's."
            }
            Self::SystemNamespace => "Versions in the system namespace can't be forgotten.",
            Self::Environment => "The device approved a deploy to another environment.",
            Self::Cluster => "The device approved a deploy to another cluster.",
            Self::KeptReach => {
                "A CI or agent key approved this container before this server kept what it reaches, so the server no longer starts it. Deploy it again."
            }
            Self::KeptKey => "The key that approved this was rotated or removed; deploy again.",
            Self::Parent => {
                "This deploy edits an earlier version a person approved, but that version didn't arrive with it or isn't one this server trusts. Deploy the service once from a terminal."
            }
            Self::ForgetParent => {
                "A deploy that only changes the last version a person approved can't forget older versions. Run --forget-older from a terminal."
            }
            Self::Signer
            | Self::OneAdmittingMachine
            | Self::JoiningMachine
            | Self::ApprovedDevices
            | Self::ApprovedGrants
            | Self::Grant
            | Self::SealingRecipient
            | Self::MachineAllocation
            | Self::AdmittingMachine
            | Self::ApprovedPasskeys
            | Self::Proposal
            | Self::KeyScope
            | Self::ThisMachine
            | Self::Passkey
            | Self::AuthKey
            | Self::Step
            | Self::VolumesEnvironment
            | Self::AttestKey
            | Self::Stage
            | Self::Commit
            | Self::CommitApproval
            | Self::Policy
            | Self::Rule
            | Self::EndedRule
            | Self::Machine
            | Self::ClaimRunnerEnvironment
            | Self::ClaimJobWorkflowRef
            | Self::ClaimJobWorkflowSha
            | Self::ClaimWorkflowRef
            | Self::ClaimEventName
            | Self::ClaimRef
            | Self::ClaimRefType
            | Self::ClaimRepositoryOwnerId
            | Self::ClaimRepositoryId
            | Self::ClaimActor
            | Self::Hostname
            | Self::PasskeySignature
            | Self::UserVerification
            | Self::RelyingParty
            | Self::Assertion
            | Self::Origin
            | Self::Challenge
            | Self::AssertionType
            | Self::IssueTime
            | Self::Lifetime
            | Self::Forget
            | Self::UnusedSubnet
            | Self::JoinConfirmation
            | Self::VerifiedBuild
            | Self::Audience
            | Self::GitHubClient
            | Self::ApprovalRecord
            | Self::SealedValues
            | Self::Holder
            | Self::StartTime
            | Self::Namespace
            | Self::Branch
            | Self::Repository
            | Self::RegistryDigest
            | Self::RuleSource
            | Self::PendingJoinCapacity
            | Self::BootstrapPeers
            | Self::FirstKey
            | Self::JoinRestrictions
            | Self::SealedTrust
            | Self::CopySource
            | Self::RestorePoint
            | Self::CopyDestination => return None,
        })
    }

    /// What a refused rule deploy says to the person who reads the Deployment, when this
    /// field has its own words.
    #[must_use]
    pub const fn rule_deploy_refusal(self) -> Option<&'static str> {
        Some(match self {
            Self::Payload => {
                "This service's settings differ from what the automatic deploy rule approves. Approve again under Automatic deploys."
            }
            Self::Image => {
                "This server runs only the image GitHub built for this commit under the rule."
            }
            Self::Audience => {
                "This push was built under another approval of the automatic deploy rule. Push again."
            }
            Self::Commit => "This commit is no longer the head of its branch.",
            Self::CommitApproval => {
                "This commit was approved for another preview, or not at all. Approve it for this preview."
            }
            Self::VerifiedBuild => {
                "No server of this cluster checked this build with GitHub. Push again."
            }
            Self::Generation => "A newer approval of the automatic deploy rule replaced this one.",
            Self::AttestKey => "This image wasn't built with this cluster's build secrets.",
            Self::EndedRule => "Automatic deploys for this environment are off.",
            Self::Service => "The automatic deploy rule doesn't cover this service.",
            Self::Machine => {
                "The automatic deploy rule was approved before this server joined. Approve again under Automatic deploys."
            }
            Self::Environment => "The automatic deploy rule is for another environment.",
            Self::Cluster => "The automatic deploy rule is for another cluster.",
            Self::HookRun => "This push already ran its pre-deploy command here.",
            Self::ClaimActor => "The preview rule doesn't deploy pushes by this GitHub user.",
            Self::Hostname => {
                "Each of a preview's domains names the preview, so no two previews share one."
            }
            Self::Repository => {
                "This image isn't in the repository the automatic deploy rule approves."
            }
            Self::RuleSource => {
                "This environment's automatic deploy rule doesn't deploy this kind of source. Approve again under Automatic deploys."
            }
            Self::RegistryDigest => "The registry serves something else under this image's digest.",
            Self::Signer | Self::Passkey => {
                "An automatic deploy rule needs your passkey or an approved device. Approve again under Automatic deploys."
            }
            Self::SigningTime => {
                "The automatic deploy rule was approved more than 10 minutes ahead of this server's clock. Check the clock of the device that approved it, and this server's."
            }
            Self::SystemNamespace
            | Self::KeptReach
            | Self::KeptKey
            | Self::Parent
            | Self::ForgetParent
            | Self::OneAdmittingMachine
            | Self::JoiningMachine
            | Self::ApprovedDevices
            | Self::ApprovedGrants
            | Self::Grant
            | Self::SealingRecipient
            | Self::MachineAllocation
            | Self::AdmittingMachine
            | Self::ApprovedPasskeys
            | Self::Proposal
            | Self::KeyScope
            | Self::ThisMachine
            | Self::AuthKey
            | Self::Step
            | Self::VolumesEnvironment
            | Self::Hook
            | Self::Stage
            | Self::Policy
            | Self::Rule
            | Self::ClaimRunnerEnvironment
            | Self::ClaimJobWorkflowRef
            | Self::ClaimJobWorkflowSha
            | Self::ClaimWorkflowRef
            | Self::ClaimEventName
            | Self::ClaimRef
            | Self::ClaimRefType
            | Self::ClaimRepositoryOwnerId
            | Self::ClaimRepositoryId
            | Self::PasskeySignature
            | Self::UserVerification
            | Self::RelyingParty
            | Self::Assertion
            | Self::Origin
            | Self::Challenge
            | Self::AssertionType
            | Self::IssueTime
            | Self::Lifetime
            | Self::Forget
            | Self::SourceServer
            | Self::KeptAdmission
            | Self::Admission
            | Self::VolumeStart
            | Self::StartedVolume
            | Self::UnusedSubnet
            | Self::JoinConfirmation
            | Self::GitHubClient
            | Self::ApprovalRecord
            | Self::SealedValues
            | Self::Holder
            | Self::StartTime
            | Self::Namespace
            | Self::Branch
            | Self::PendingJoinCapacity
            | Self::BootstrapPeers
            | Self::FirstKey
            | Self::JoinRestrictions
            | Self::SealedTrust
            | Self::CopySource
            | Self::RestorePoint
            | Self::CopyDestination => return None,
        })
    }
}

impl std::fmt::Display for MismatchField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}
