//! The Config Store: all authored configuration and its history, behind one
//! synchronous interface. `read` answers a [`Query`] and `write` applies a
//! [`Command`], each in one transaction; in-process callers pass the payload itself
//! for its typed answer. Runners, GitHub builds and automation have methods of
//! their own. Storage is its only I/O.

mod auto_rule;
mod automation;
mod branch;
mod build;
mod builders;
pub mod catalog;
mod client_sealed;
mod command;
mod config_item;
mod deployment;
mod digest;
pub mod docs;
mod domain;
mod error;
mod git;
mod github;
mod id;
mod names;
mod old_cli;
mod permission;
mod policy;
mod pr_environment;
mod preview_rule;
mod project;
mod pull_request;
mod query;
mod registry;
mod removal;
mod replay;
mod reseal;
mod review;
mod rules;
mod scope;
mod sealing;
mod sealing_targets;
mod service;
mod settings;
pub mod stderr_log;
mod storage;
mod teardown;
pub mod tools;
mod trusted;
mod typed_address;
mod variables;
mod volume;

use std::collections::BTreeSet;

use ployz_core::RpcError;

pub use auto_rule::{ApproveRule, RuleQuery, RuleView};
pub use automation::{
    AutoDeployed, Automated, BranchHead, CheckConclusion, CheckStatus, CheckSuite, Skipped,
    SystemEvent,
};
pub use branch::{
    BranchPlanQuery, BranchPlanView, BranchQuery, BranchView, Branched, CopyNode, CreateBranch,
    KeepBranch, LiveNode, PlannedNode, PlannedRole, SetBranchSetup, SetupCommand, SyncChanges,
    SyncQuery, SyncRow, SyncView, Synced, Take, Taken, UndoSync, Undone,
};
pub use branch::{
    FollowHint, HintSource, Included, IncomingChange, Mark, NamedRow, NeverSync, NeverSynced,
    NeverSyncedRow, ProposalSource, RemoveProposal, Removed, RowRef, SecretRow, SyncChange,
};
pub use build::{
    BuildLogQuery, BuildLogView, BuildReport, BuildStatus, BuildView, GitSource, GithubBuild,
    GithubBuildId, GithubClaims, GithubEnd, GithubGrant, GithubHandOff, GithubInput, GithubReport,
    GithubRule, GithubRun, ImageToken, RunEnd,
};
pub use builders::{BuildOrder, BuildOrderQuery, BuildOrderView, Builder, SetBuildOrder};
pub use client_sealed::SecretValue;
pub use command::*;
pub use deployment::loss::{RemovedVolume, VolumeLoss, docker_volume};
pub use deployment::{
    CiVerdict, Claimed, DeployedNode, DeploymentStatus, DeploymentSummary, DeploymentView,
    DeploymentWarning, FailedAt, Failure, GitPush, IngressAction, IngressFailure, IngressRepair,
    NodeOutcome, NodeStatus, Outcome, RowPhase, RowState, RowTracker, RowTransition, RunEvidence,
    ServerProgress, ServerRow, StoppedBy, Unclaimed, UploadBase, UploadedSource, starts_container,
};
pub use domain::{
    AddDomain, ClusterDomain, ClusterDomainStatus, DnsLookup, DnsRecord, DnsRecordKind, Domain,
    DomainAction, DomainAdded, DomainChange, DomainEdit, DomainEvidence, DomainName, DomainQuery,
    DomainRow, DomainStaged, DomainStatus, DomainView, DomainsQuery, DomainsView,
    PublishedHostname, RemoveDomain, SetGeneratedDomain, TunnelAnswer, TunnelEvidence, TunnelReach,
};
pub use git::{AuthorizedRepository, CreateGitService};
pub use github::{
    GithubFileBody, GithubFileQuery, GithubFileView, GithubTreeQuery, GithubTreeView,
};
pub use id::*;
pub use permission::Permission;
pub use ployz_core::authority::rule_draft::RuleSourceView;
/// A Sync view row's id, as Sync, Take, Never sync and Hold name it.
pub use ployz_core::config::RowId;
pub use preview_rule::{ApprovePreviewRule, PlanPreviewRule, PreviewRuleView};
pub use pull_request::{
    ApprovePullRequest, EnvironmentReview, MergeRow, OnMerge, OnMergeCheck, OnMergeConclusion,
    OnMergeInto, OnMergeQuery, OpenPullRequest, PrEnvironment, PrPlan, PrPlansQuery, PrPlansView,
    PreviewReviewQuery, PreviewReviewView, PullRequest, PullRequestQuery, PullRequestRef,
    PullRequestView, Ride, SetMergeValue, SetPrPlan, Standing, Sweep,
};
pub use query::*;
pub use removal::{
    CancelRemoval, ConsentedQuery, ConsentedView, Due, DueQuery, Fix, Left, LeftStatus, Removal,
    RemovalCancelled, Remove, RemovedQuery, RemovedView, Residual, Waiting,
};
pub use reseal::{LegacyCount, Resealed};
pub use review::history::{HistoryAction, SavedRevision};
pub use review::{
    ApprovalOperation, ApprovalRedeploy, ApprovalRequired, ApprovalSubject, DataEffect,
    DestructiveEffect, DestructiveKind, DiffView, LostAfter, MalformedReviewVersion, NodeChange,
    OperationVerb, ReviewVersion, introduced_source,
};
pub use rules::{BrokenReference, Missing, ReferenceAt};
pub use scope::{EnvironmentRef, EnvironmentSummary};
pub use sealing::SealingKey;
pub use settings::size::memory_size;
pub use settings::{Apply, NodeName, SettingPath};
pub use storage::Converted;
pub use teardown::{
    AppliedVolume, EnvironmentListing, EnvironmentRemoval, EnvironmentsQuery, EnvironmentsView,
    OrganizationRemoved, ProjectListing, ProjectsQuery, ProjectsView, ProtectedView,
    RemovedBecause, SeenProtection, SetDefaultEnvironment, SetEnvironmentProtected,
};
pub use trusted::{
    Approval, Asks, ObservationFailure, ObservationStage, RunningObservation, SealingRecipient,
    Trusted, VolumeObservation,
};
pub use volume::size::{Unheld, VolumeResized};

/// Who is asking, and in which Organization. Every read and write is scoped to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor {
    /// The Organization whose configuration it reads and writes.
    pub organization: OrganizationId,
    /// Who acts, as Cloud authenticated them; none for the Store's own automation
    /// and the hidden local Store.
    pub principal: Option<Principal>,
}

impl Actor {
    /// The Store's own automation in `organization`: nobody in particular.
    #[must_use]
    pub const fn system(organization: OrganizationId) -> Self {
        Self {
            organization,
            principal: None,
        }
    }
}

/// One Config Store over one database.
pub struct ConfigStore {
    storage: storage::Storage,
    sealing: SealingKey,
    converted: Option<Converted>,
}

impl ConfigStore {
    /// Open the Store at `url` (`postgres://…` in Cloud; `sqlite:PATH`, or `sqlite::memory:` for tests),
    /// creating and migrating its tables as needed. Secrets are sealed with `sealing`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for an unsupported URL, or a storage error.
    pub fn open(url: &str, sealing: SealingKey) -> Result<Self, RpcError> {
        let (storage, converted) = storage::Storage::open(url)?;
        Ok(Self {
            storage,
            sealing,
            converted,
        })
    }

    /// What converting Conditional Syncs to offers did, when this open converted them:
    /// the receipt its caller logs. `None` once an earlier open did.
    #[must_use]
    pub const fn converted(&self) -> Option<&Converted> {
        self.converted.as_ref()
    }

    /// Answer `query` — a [`Query`], or one of its payloads for its own view —
    /// from one consistent state.
    ///
    /// # Errors
    /// Returns an RPC error: `not_found`, `ambiguous` or `invalid_argument` for what the
    /// query names, or a storage error.
    pub fn read<Q: Ask>(&self, who: &Actor, query: &Q) -> Result<Q::View, RpcError> {
        self.read_trusted(who, query, &Trusted::default())
    }

    /// [`read`](Self::read) with what Cloud observed itself, such as the certificates
    /// behind a domain's status. Never pass caller-supplied evidence.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn read_trusted<Q: Ask>(
        &self,
        who: &Actor,
        query: &Q,
        trusted: &Trusted,
    ) -> Result<Q::View, RpcError> {
        self.storage.read(|tx| {
            query.answer(&mut Call {
                tx,
                who,
                sealing: &self.sealing,
                trusted,
            })
        })
    }

    /// Apply `command` — a [`Command`], or one of its payloads for its own result —
    /// in one transaction: all of it, or none.
    ///
    /// # Errors
    /// Returns an RPC error: `invalid_argument`, `not_found`, `ambiguous` or `conflict`
    /// for what the command asks, or a storage error.
    pub fn write<C: Tell>(&self, who: &Actor, command: &C) -> Result<C::Written, RpcError> {
        self.write_trusted(who, command, &Trusted::default())
    }

    /// [`write`](Self::write) with evidence Cloud gathered itself, such as which
    /// repositories the Organization may read. Never pass caller-supplied evidence.
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn write_trusted<C: Tell>(
        &self,
        who: &Actor,
        command: &C,
        trusted: &Trusted,
    ) -> Result<C::Written, RpcError> {
        self.with_call(who, trusted, |call| command.apply(call))
    }

    /// Run `work` in one write transaction, with the [`Call`] of caller `who` and the
    /// evidence `trusted`: what the caller holds, and the servers it names.
    fn with_call<T>(
        &self,
        who: &Actor,
        trusted: &Trusted,
        work: impl FnOnce(&mut Call<'_>) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let trusted = trusted.held_by(who);
        self.acting(&trusted, |tx| {
            let trusted = trusted.naming(tx, who)?;
            work(&mut Call {
                tx,
                who,
                sealing: &self.sealing,
                trusted: &trusted,
            })
        })
    }

    /// Run `work` in one write transaction acting for the caller `trusted` describes,
    /// so a write it reaches deep inside still meets [`storage::admit_protect`].
    fn acting<T>(
        &self,
        trusted: &Trusted,
        work: impl FnOnce(&mut dyn storage::Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        self.storage
            .write(|tx| work(&mut storage::Acting { tx, trusted }))
    }

    /// [`write_trusted`](Self::write_trusted) a [`Command`], answering with the pull
    /// requests whose checks it may move, found in the same transaction.
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn commit(
        &self,
        who: &Actor,
        command: &Command,
        trusted: &Trusted,
    ) -> Result<Committed, RpcError> {
        self.with_call(who, trusted, |call| {
            let written = command.apply(call)?;
            let checks = match written.environment() {
                Some(environment) => pull_request::project_checks(call.tx, environment)?,
                None => Vec::new(),
            };
            Ok(Committed { written, checks })
        })
    }

    /// The pull requests whose checks a change in `environment` may move: those of
    /// the open PR Environments in its Project. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn checks(&self, environment: &EnvironmentId) -> Result<Vec<PullRequestRef>, RpcError> {
        self.storage
            .read(|tx| pull_request::project_checks(tx, environment))
    }

    /// Every name under which any Container of `environment`, present or past, may hold
    /// a Sealed value, as of now. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn sealed_names(&self, environment: &EnvironmentId) -> Result<BTreeSet<String>, RpcError> {
        self.storage
            .read(|tx| review::sealed::names(tx, environment.as_str()))
    }

    /// Organizations with a secret the Store sealed itself in some Working State or
    /// Saved head: those a reseal has work in. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn legacy_organizations(&self) -> Result<Vec<OrganizationId>, RpcError> {
        self.storage.read(reseal::legacy_organizations)
    }

    /// How many secrets the Store sealed itself `organization` holds, by whether a
    /// reseal converts them. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn legacy_count(&self, organization: &OrganizationId) -> Result<LegacyCount, RpcError> {
        self.storage
            .read(|tx| reseal::legacy_count(tx, organization))
    }

    /// The Environments of `organization` whose Working State or Saved head holds a
    /// secret the Store sealed itself. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn reseal_environments(
        &self,
        organization: &OrganizationId,
    ) -> Result<Vec<EnvironmentId>, RpcError> {
        self.storage
            .read(|tx| reseal::reseal_environments(tx, organization))
    }

    /// Seal every secret of `environment` the Store sealed itself for `recipient`,
    /// the Cluster's key, in its Working State and Saved head, in one transaction.
    /// Running it again changes nothing. In-process only.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Environment, or a storage error.
    pub fn reseal(
        &self,
        environment: &EnvironmentId,
        recipient: &SealingRecipient,
    ) -> Result<Resealed, RpcError> {
        self.storage
            .write(|tx| reseal::reseal(tx, environment, recipient, &self.sealing))
    }

    /// Bind a queued Deployment to `runner` and return its frozen Deploy Intent, with
    /// its registry credentials opened and its secrets still sealed for the Machine.
    /// In-process only: never exposed over HTTPS. The same runner may claim again
    /// until it records a Deploy Preview; after that its claim leaves the outcome
    /// unknown.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, `conflict` when another runner
    /// owns it, a newer one replaced it, it was cancelled or ended, this runner
    /// already prepared it, or its values can't deploy, which fails it with why; or
    /// a storage error.
    pub fn claim(&self, deployment: &DeploymentId, runner: &RunnerId) -> Result<Claimed, RpcError> {
        self.storage
            .write(|tx| deployment::claim(tx, deployment, runner, &self.sealing))
            .and_then(|claimed| claimed)
    }

    /// Record what `runner` did with the Deployment it claimed; confirmed Node Outcomes
    /// advance Applied State. Recording the same evidence twice changes nothing.
    /// In-process only: never exposed over HTTPS.
    ///
    /// # Errors
    /// Returns `conflict` when another runner owns the Deployment or it already
    /// recorded different evidence, `invalid_argument` for evidence that does not
    /// match it, or a storage error.
    pub fn record(
        &self,
        deployment: &DeploymentId,
        runner: &RunnerId,
        evidence: RunEvidence,
    ) -> Result<DeploymentSummary, RpcError> {
        self.storage
            .write(|tx| deployment::record(tx, deployment, runner, evidence))
    }

    /// Every queued Deployment no runner claimed, admitted before `before` (Unix
    /// seconds), oldest first, across Organizations: Cloud's sweep dispatches each
    /// again, as its first dispatch may have been lost. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn unclaimed(&self, before: i64) -> Result<Vec<Unclaimed>, RpcError> {
        self.storage.read(|tx| deployment::unclaimed(tx, before))
    }

    /// The Git Services Deployment `deployment` builds, each with its pinned commit,
    /// if any: what Cloud reads from GitHub for its runner. In-process only.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    pub fn sources(&self, deployment: &DeploymentId) -> Result<Vec<GitSource>, RpcError> {
        self.storage.read(|tx| build::sources(tx, deployment))
    }

    /// Pin the commit each Git Service of Deployment `deployment` builds, by runtime
    /// Service name, as Cloud resolved it from its branch. A pinned commit never
    /// changes: a pin for a Service already pinned is ignored. Returns every source
    /// with its pin. `trusted` carries the Organization's members as Cloud read them:
    /// a Trusted pull request's commit pins only while its author is one, or when a bot
    /// opened it from the repository itself. In-process only.
    ///
    /// # Errors
    /// Returns `conflict` once the Deployment was replaced, cancelled or ended,
    /// `invalid_argument` for a Service it doesn't build, a malformed commit or one
    /// its pull request doesn't admit, or a storage error.
    pub fn pin(
        &self,
        deployment: &DeploymentId,
        commits: &std::collections::BTreeMap<ployz_core::ServiceName, CommitSha>,
        trusted: &Trusted,
    ) -> Result<Vec<GitSource>, RpcError> {
        // A transaction of its own, before the pin's: a refused pin undoes its writes.
        self.storage.write(|tx| {
            deployment::load(tx, deployment)?;
            let who = Actor::system(build::organization(tx, deployment)?);
            pull_request::withdraw_trust(tx, &who, trusted)
        })?;
        self.storage.write(|tx| {
            build::pin(tx, deployment, commits, trusted)?;
            build::sources(tx, deployment)
        })
    }

    /// Apply what Cloud observed of GitHub: a branch's head or a check suite's result,
    /// admitting the auto-deploys they call for. `trusted` says how many Servers the
    /// Organization has: none skips every auto-deploy. In-process only: never exposed
    /// over HTTPS.
    ///
    /// # Errors
    /// Returns `conflict` when a [`BranchHead`]'s base is no longer the Store's head
    /// (read [`Self::branch_head`] and compare again), `invalid_argument` for a
    /// malformed observation, or a storage error.
    pub fn system(
        &self,
        organization: &OrganizationId,
        event: &SystemEvent,
        trusted: &Trusted,
    ) -> Result<Written, RpcError> {
        self.storage
            .write(|tx| automation::system(tx, organization, event, trusted))
            .map(Written::Automated)
    }

    /// The head of a branch the Store last saw, which Cloud compares a new head from.
    /// In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn branch_head(
        &self,
        organization: &OrganizationId,
        repository_id: RepositoryId,
        branch: &BranchName,
    ) -> Result<Option<CommitSha>, RpcError> {
        self.storage
            .read(|tx| automation::head(tx, organization, repository_id, branch))
    }

    /// The open pull requests into `branch` whose merge would land config: Cloud
    /// reports each that merged before it tells the Store about the push.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn awaiting_merge(
        &self,
        organization: &OrganizationId,
        repository_id: RepositoryId,
        branch: &BranchName,
    ) -> Result<Vec<PullRequestNumber>, RpcError> {
        let who = Actor::system(organization.clone());
        self.storage
            .read(|tx| pull_request::awaiting_merge(tx, &who, repository_id, branch))
    }

    /// Forget an Organization's configuration once it has no Project: what it
    /// created, what Cloud observed of its repositories, and its Build Order. Cloud's
    /// own Organization removal runs it. In-process only: no command reaches it.
    ///
    /// # Errors
    /// Returns `conflict` while the Organization has a Project, or a storage error.
    pub fn remove_organization(&self, who: &Actor) -> Result<OrganizationRemoved, RpcError> {
        self.storage
            .write(|tx| teardown::remove_organization(tx, who))
    }

    /// The Volumes a Deploy put on the Organization's Servers: exactly what
    /// [`SystemEvent::ClusterForgotten`] lets go of. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn applied_volumes(&self, who: &Actor) -> Result<Vec<AppliedVolume>, RpcError> {
        self.storage.read(|tx| teardown::applied_volumes(tx, who))
    }

    /// Hand a pinned Git build that hasn't started to GitHub run `run`. In-process only.
    ///
    /// # Errors
    /// Returns `conflict` once the Deployment no longer wants it or it went to
    /// another run or Builder, `not_found` for an unknown build, or a storage error.
    pub fn github_dispatched(
        &self,
        id: &GithubBuildId,
        github: &GithubRun,
    ) -> Result<(), RpcError> {
        self.storage
            .write(|tx| build::github_dispatched(tx, id, github))
    }

    /// A Git build handed to GitHub. In-process only.
    ///
    /// # Errors
    /// Returns `not_found` unless GitHub holds it, held it, or passed it on, or a
    /// storage error.
    pub fn github_build(&self, id: &GithubBuildId) -> Result<GithubHandOff, RpcError> {
        self.storage.read(|tx| build::github_build(tx, id))
    }

    /// Check a runner's verified OIDC `claims` against GitHub build `id`.
    /// In-process only.
    ///
    /// # Errors
    /// Returns `unauthenticated` for a token of another repository, workflow, branch
    /// or run, or one GitHub didn't dispatch; `not_found` for an unknown build.
    pub fn github_authorize(
        &self,
        id: &GithubBuildId,
        claims: &GithubClaims,
    ) -> Result<GithubBuild, RpcError> {
        self.storage
            .read(|tx| build::github_authorize(tx, id, claims))
    }

    /// The rule GitHub build `id` builds under, automatic deploy or preview, if any: its
    /// run is the push's own, which Cloud follows rather than dispatching one.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    pub fn github_rule(&self, id: &GithubBuildId) -> Result<Option<GithubRule>, RpcError> {
        self.storage.read(|tx| build::github_rule(tx, id))
    }

    /// The review a person signed for GitHub build `id`'s commit, when it builds a
    /// reviewed preview: Cloud dispatches it on the parent's branch rather than following
    /// a push.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    pub fn github_review(
        &self,
        id: &GithubBuildId,
    ) -> Result<Option<ployz_core::authority::Review>, RpcError> {
        self.storage.read(|tx| build::github_review(tx, id))
    }

    /// The approval a person signed for GitHub build `id`'s commit, when it builds under
    /// its Environment's automatic deploy rule: Cloud dispatches it on the rule's branch,
    /// and relays the approval to the grant Machine.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown build, or a storage error.
    pub fn github_approval(
        &self,
        id: &GithubBuildId,
    ) -> Result<
        Option<ployz_core::authority::Signed<ployz_core::authority::CommitApproval>>,
        RpcError,
    > {
        self.storage.read(|tx| build::github_approval(tx, id))
    }

    /// Whether Deployment `id` waits, queued, for a person to approve its commits again
    /// once its builds ended: Cloud holds it until one does.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    pub fn awaiting_approval(&self, id: &DeploymentId) -> Result<bool, RpcError> {
        self.storage
            .read(|tx| deployment::awaiting_approval(tx, id))
    }

    /// The GitHub build run `run_id` holds while it builds, if any.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn github_by_run(&self, run_id: u64) -> Result<Option<GithubBuildId>, RpcError> {
        self.storage.read(|tx| build::github_by_run(tx, run_id))
    }

    /// What GitHub build `id` builds. In-process only: only the authorized runner
    /// receives it.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown build, or a storage error.
    pub fn github_input(&self, id: &GithubBuildId) -> Result<GithubInput, RpcError> {
        self.storage.read(|tx| build::github_input(tx, id))
    }

    /// Run `run_id` of GitHub build `id` checked in with `grant`. In-process only.
    ///
    /// # Errors
    /// Returns `conflict` when it already checked in or is no longer wanted.
    pub fn github_check_in(
        &self,
        id: &GithubBuildId,
        run_id: u64,
        grant: &GithubGrant,
    ) -> Result<(), RpcError> {
        self.storage
            .write(|tx| build::github_check_in(tx, id, run_id, grant))
    }

    /// Take a batch of run `run_id`'s log; returns how many lines it took so far.
    /// In-process only.
    ///
    /// # Errors
    /// Returns `conflict` before check-in, after the final report or the build
    /// ended, or when lines before the batch are missing.
    pub fn github_report(
        &self,
        id: &GithubBuildId,
        run_id: u64,
        report: &GithubReport,
    ) -> Result<u64, RpcError> {
        self.storage
            .write(|tx| build::github_report(tx, id, run_id, report))
    }

    /// End GitHub build `id`, held by run `run_id`, or before any run with none.
    /// In-process only.
    ///
    /// # Errors
    /// Returns `conflict` once the build ended or another run or Builder holds it.
    pub fn github_end(
        &self,
        id: &GithubBuildId,
        run_id: Option<u64>,
        end: &GithubEnd,
    ) -> Result<BuildStatus, RpcError> {
        self.storage
            .write(|tx| build::github_end(tx, id, run_id, end))
    }

    /// Deployment `deployment`'s builds still on GitHub. In-process only.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn github_outstanding(
        &self,
        deployment: &DeploymentId,
    ) -> Result<Vec<GithubBuild>, RpcError> {
        self.storage
            .read(|tx| build::github_outstanding(tx, deployment))
    }
}
