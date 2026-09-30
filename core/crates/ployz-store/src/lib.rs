//! The Config Store: all authored configuration and its history, behind one
//! synchronous interface. `read` answers a [`Query`] and `write` applies a
//! [`Command`], each in one transaction; in-process callers use the typed method
//! for each, which runs the same code. Storage is its only I/O.

mod automation;
mod branch;
mod build;
mod builders;
pub mod catalog;
mod command;
mod conditional_save;
mod deployment;
mod domain;
mod error;
mod git;
mod id;
mod policy;
mod pull_request;
mod query;
mod registry;
mod removal;
mod review;
mod scope;
mod sealing;
mod settings;
mod storage;
mod teardown;
mod trusted;
mod variables;

use ployz_core::RpcError;

pub use automation::{AutoDeployed, Automated, BranchHead, CheckSuite, Skipped, SystemEvent};
pub use branch::{
    BranchPlanQuery, BranchPlanView, BranchQuery, BranchView, Branched, CopyNode, CreateBranch,
    KeepBranch, LiveNode, Move, MoveChoice, MovePick, MoveQuery, MoveRow, MoveView, Moved,
    PickChoice, PlannedNode, PlannedRole, Save, SetupCommand, Take, Update, When,
};
pub use build::{
    BuildLogQuery, BuildLogView, BuildReport, BuildStatus, BuildView, GitSource, GithubBuild,
    GithubBuildId, GithubClaims, GithubEnd, GithubGrant, GithubReport, GithubRun,
};
pub use builders::{BuildOrder, BuildOrderQuery, BuildOrderView, Builder, SetBuildOrder};
pub use command::*;
pub use conditional_save::{ConditionalSave, Landed, PendingSaves, PullRequestHint, SaveState};
pub use deployment::{
    Claimed, DeploymentStatus, DeploymentSummary, DeploymentView, NodeOutcome, NodeStatus, Outcome,
    RunEvidence, Unclaimed, UploadBase, UploadedSource,
};
pub use domain::{
    AddDomain, ClusterDomain, ClusterDomainStatus, DnsLookup, DnsRecord, Domain, DomainAction,
    DomainEvidence, DomainName, DomainQuery, DomainRow, DomainStaged, DomainStatus, DomainView,
    DomainsQuery, DomainsView, RemoveDomain,
};
pub use git::{AuthorizedRepository, CreateGitService};
pub use id::*;
pub use pull_request::{
    Destination, DestinationSave, OpenPullRequest, PrEnvironment, PrPlan, PrPlansQuery,
    PrPlansView, PullRequest, PullRequestQuery, PullRequestRef, PullRequestView, SetPrPlan, Sweep,
};
pub use query::*;
pub use removal::{RemovedVolume, VolumeLoss};
pub use review::{DataEffect, DiffView, NodeChange};
pub use scope::{EnvironmentRef, EnvironmentSummary};
pub use sealing::SealingKey;
pub use settings::{Apply, SettingPath};
pub use teardown::{
    EnvironmentListing, EnvironmentRemoved, EnvironmentsQuery, EnvironmentsView,
    OrganizationRemoved, ProjectListing, ProjectRemoved, ProjectsQuery, ProjectsView,
    RemoveEnvironment, RemoveProject, SetDefaultEnvironment,
};
pub use trusted::{Trusted, VolumeObservation};

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
}

impl ConfigStore {
    /// Open the Store at `url` (`postgres://…` in Cloud; `sqlite:PATH`, or `sqlite::memory:` for tests),
    /// creating and migrating its tables as needed. Secrets are sealed with `sealing`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for an unsupported URL, or a storage error.
    pub fn open(url: &str, sealing: SealingKey) -> Result<Self, RpcError> {
        Ok(Self {
            storage: storage::Storage::open(url)?,
            sealing,
        })
    }

    /// Answer `query` from one consistent state.
    ///
    /// # Errors
    /// Returns an RPC error: `not_found`, `ambiguous` or `invalid_argument` for what the
    /// query names, or a storage error.
    pub fn read(&self, who: &Actor, query: &Query) -> Result<View, RpcError> {
        self.read_trusted(who, query, &Trusted::default())
    }

    /// [`read`](Self::read) with what Cloud observed itself, such as the certificates
    /// behind a domain's status. Never pass caller-supplied evidence.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn read_trusted(
        &self,
        who: &Actor,
        query: &Query,
        trusted: &Trusted,
    ) -> Result<View, RpcError> {
        self.storage.read(|tx| query::run(tx, who, query, trusted))
    }

    /// Apply `command` in one transaction: all of it, or none.
    ///
    /// # Errors
    /// Returns an RPC error: `invalid_argument`, `not_found`, `ambiguous` or `conflict`
    /// for what the command asks, or a storage error.
    pub fn write(&self, who: &Actor, command: &Command) -> Result<Written, RpcError> {
        self.write_trusted(who, command, &Trusted::default())
    }

    /// [`write`](Self::write) with evidence Cloud gathered itself, such as which
    /// repositories the Organization may read. Never pass caller-supplied evidence.
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn write_trusted(
        &self,
        who: &Actor,
        command: &Command,
        trusted: &Trusted,
    ) -> Result<Written, RpcError> {
        self.storage
            .write(|tx| command::run(tx, who, &self.sealing, command, trusted))
    }

    /// [`Query::Environment`]: an Environment's Settings.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn environment(
        &self,
        who: &Actor,
        query: &EnvironmentQuery,
    ) -> Result<EnvironmentView, RpcError> {
        self.storage.read(|tx| query::environment(tx, who, query))
    }

    /// [`Command::CreateProject`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_project(
        &self,
        who: &Actor,
        create: &CreateProject,
    ) -> Result<ProjectCreated, RpcError> {
        self.storage
            .write(|tx| command::create_project(tx, who, create))
    }

    /// [`Command::CreateEnvironment`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_environment(
        &self,
        who: &Actor,
        create: &CreateEnvironment,
    ) -> Result<EnvironmentCreated, RpcError> {
        self.storage
            .write(|tx| command::create_environment(tx, who, create))
    }

    /// [`Command::CreateService`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_service(
        &self,
        who: &Actor,
        create: &CreateService,
    ) -> Result<ServiceStaged, RpcError> {
        self.storage
            .write(|tx| command::create_service(tx, who, create))
    }

    /// [`Command::RenameService`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn rename_service(
        &self,
        who: &Actor,
        rename: &RenameService,
    ) -> Result<ServiceStaged, RpcError> {
        self.storage
            .write(|tx| command::rename_service(tx, who, rename))
    }

    /// [`Command::RemoveService`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn remove_service(
        &self,
        who: &Actor,
        remove: &RemoveService,
    ) -> Result<ServiceStaged, RpcError> {
        self.storage
            .write(|tx| command::remove_service(tx, who, remove))
    }

    /// [`Query::Services`]: an Environment's Services.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn services(&self, who: &Actor, query: &ServicesQuery) -> Result<ServicesView, RpcError> {
        self.storage.read(|tx| query::services(tx, who, query))
    }

    /// [`Query::Service`]: one Service.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn service(&self, who: &Actor, query: &ServiceQuery) -> Result<ServiceView, RpcError> {
        self.storage.read(|tx| query::service(tx, who, query))
    }

    /// [`Command::Edit`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn edit(&self, who: &Actor, edit: &Edit) -> Result<Edited, RpcError> {
        self.storage
            .write(|tx| command::edit(tx, who, &self.sealing, edit, &Trusted::default()))
    }

    /// [`Command::CreateGitService`], with the repository evidence Cloud gathered.
    ///
    /// # Errors
    /// As [`write`](Self::write); `not_found` when `trusted` doesn't vouch for the
    /// repository or branch.
    pub fn create_git_service(
        &self,
        who: &Actor,
        create: &CreateGitService,
        trusted: &Trusted,
    ) -> Result<ServiceStaged, RpcError> {
        self.storage
            .write(|tx| git::create_git_service(tx, who, create, trusted))
    }

    /// [`Query::Diff`]: an Environment's changes and the version to act on them by.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn diff(&self, who: &Actor, query: &DiffQuery) -> Result<DiffView, RpcError> {
        self.storage.read(|tx| query::diff(tx, who, query))
    }

    /// [`Command::Publish`]. `trusted` carries what the Servers hold when it
    /// publishes the removal of deployed Volumes; never pass caller-supplied evidence.
    ///
    /// # Errors
    /// As [`write`](Self::write); `confirmation_required` when it publishes the loss of
    /// data not accepted, `unavailable` when the evidence of that data is missing.
    pub fn publish(
        &self,
        who: &Actor,
        publish: &Publish,
        trusted: &Trusted,
    ) -> Result<Published, RpcError> {
        self.storage
            .write(|tx| command::publish(tx, who, publish, trusted))
    }

    /// [`Command::Discard`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn discard(&self, who: &Actor, discard: &Discard) -> Result<Discarded, RpcError> {
        self.storage.write(|tx| command::discard(tx, who, discard))
    }

    /// [`Command::Admit`]: publish if needed, then freeze and queue a Deployment.
    /// `trusted` carries what the Servers hold when the Deploy removes deployed
    /// Volumes; never pass caller-supplied evidence.
    ///
    /// # Errors
    /// As [`write`](Self::write); `confirmation_required` when it deletes data not
    /// accepted by name, `unavailable` when the evidence of that data is missing.
    pub fn admit(
        &self,
        who: &Actor,
        admit: &Admit,
        trusted: &Trusted,
    ) -> Result<DeploymentSummary, RpcError> {
        self.storage
            .write(|tx| command::admit(tx, who, admit, trusted))
    }

    /// [`Command::AddDomain`], with Cloud's evidence of the custom-domain capability.
    ///
    /// # Errors
    /// As [`write`](Self::write); `unsupported` for a custom domain without the capability.
    pub fn add_domain(
        &self,
        who: &Actor,
        add: &AddDomain,
        trusted: &Trusted,
    ) -> Result<DomainStaged, RpcError> {
        self.storage
            .write(|tx| domain::add_domain(tx, who, add, trusted))
    }

    /// [`Command::RemoveDomain`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn remove_domain(
        &self,
        who: &Actor,
        remove: &RemoveDomain,
        trusted: &Trusted,
    ) -> Result<DomainStaged, RpcError> {
        self.storage
            .write(|tx| domain::remove_domain(tx, who, remove, trusted))
    }

    /// [`Query::Domains`]: an Environment's domains, each with its status.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn domains(
        &self,
        who: &Actor,
        query: &DomainsQuery,
        trusted: &Trusted,
    ) -> Result<DomainsView, RpcError> {
        self.storage
            .read(|tx| domain::domains(tx, who, query, trusted))
    }

    /// [`Query::Domain`]: one domain, with its status.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn domain(
        &self,
        who: &Actor,
        query: &DomainQuery,
        trusted: &Trusted,
    ) -> Result<DomainView, RpcError> {
        self.storage
            .read(|tx| domain::domain(tx, who, query, trusted))
    }

    /// [`Command::Start`]: check a queued Deployment can still go to a runner.
    ///
    /// # Errors
    /// As [`write`](Self::write); `conflict` unless the Deployment is queued.
    pub fn start(&self, who: &Actor, start: &Start) -> Result<DeploymentSummary, RpcError> {
        self.storage.write(|tx| command::start(tx, who, start))
    }

    /// [`Command::CreateVolume`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_volume(
        &self,
        who: &Actor,
        create: &CreateVolume,
    ) -> Result<VolumeStaged, RpcError> {
        self.storage
            .write(|tx| command::create_volume(tx, who, create))
    }

    /// [`Command::RemoveVolume`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn remove_volume(
        &self,
        who: &Actor,
        remove: &RemoveVolume,
    ) -> Result<VolumeStaged, RpcError> {
        self.storage
            .write(|tx| command::remove_volume(tx, who, remove))
    }

    /// Change a draft Volume's storage; a deployment request fixes it.
    ///
    /// # Errors
    /// As [`write`](Self::write), including conflict when storage is locked.
    pub fn set_volume_storage(
        &self,
        who: &Actor,
        set: &SetVolumeStorage,
    ) -> Result<VolumeStaged, RpcError> {
        self.storage.write(|tx| command::set_storage(tx, who, set))
    }

    /// [`Query::Volumes`]: an Environment's Volumes.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn volumes(&self, who: &Actor, query: &VolumesQuery) -> Result<VolumesView, RpcError> {
        self.storage.read(|tx| query::volumes(tx, who, query))
    }

    /// [`Query::Volume`]: one Volume.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn volume(&self, who: &Actor, query: &VolumeQuery) -> Result<VolumeView, RpcError> {
        self.storage.read(|tx| query::volume(tx, who, query))
    }

    /// [`Query::Removals`]: the deployed Volumes a full Deploy would remove, so the
    /// caller knows which Docker Volumes to observe before admitting it.
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn removals(&self, who: &Actor, query: &RemovalsQuery) -> Result<RemovalsView, RpcError> {
        self.storage.read(|tx| query::removals(tx, who, query))
    }

    /// [`Command::Cancel`].
    ///
    /// # Errors
    /// As [`write`](Self::write); `conflict` when the Deployment already ended.
    pub fn cancel(&self, who: &Actor, cancel: &Cancel) -> Result<DeploymentSummary, RpcError> {
        self.storage.write(|tx| command::cancel(tx, who, cancel))
    }

    /// What deploying would ship, from authored state alone.
    ///
    /// # Errors
    /// As [`Self::read`], plus `invalid_argument` when the Environment can't deploy.
    pub fn plan(&self, who: &Actor, query: &PlanQuery) -> Result<PlanView, RpcError> {
        self.storage
            .read(|tx| query::deployment::plan(tx, who, query))
    }

    /// The Namespace an Environment's containers carry on the Servers.
    ///
    /// # Errors
    /// As [`Self::read`].
    pub fn namespace(
        &self,
        who: &Actor,
        query: &NamespaceQuery,
    ) -> Result<NamespaceView, RpcError> {
        self.storage
            .read(|tx| query::deployment::namespace(tx, who, query))
    }

    /// One page of an Environment's Deployments, newest first.
    ///
    /// # Errors
    /// As [`Self::read`], plus `invalid_argument` for a bad limit or cursor.
    pub fn deployments(
        &self,
        who: &Actor,
        query: &DeploymentsQuery,
    ) -> Result<DeploymentsView, RpcError> {
        self.storage
            .read(|tx| query::deployment::page(tx, who, query))
    }

    /// One Deployment with its recorded Deploy Preview and Node Outcomes.
    ///
    /// # Errors
    /// As [`Self::read`]; `not_found` for a Deployment of another Organization.
    pub fn deployment(&self, who: &Actor, id: &DeploymentId) -> Result<DeploymentView, RpcError> {
        self.storage.read(|tx| deployment::view(tx, who, id))
    }

    /// Bind a queued Deployment to `runner` and return its frozen Deploy Intent, with
    /// its secrets unsealed: the only way plaintext leaves the Store. In-process only:
    /// never exposed over HTTPS. The same runner may claim again until it records a
    /// Deploy Preview; after that its claim leaves the outcome unknown.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, `conflict` when another runner
    /// owns it, a newer one replaced it, it was cancelled or ended, or this runner
    /// already prepared it, or a storage error.
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
    /// with its pin. In-process only.
    ///
    /// # Errors
    /// Returns `conflict` once the Deployment was replaced, cancelled or ended,
    /// `invalid_argument` for a Service it doesn't build or a malformed commit, or a
    /// storage error.
    pub fn pin(
        &self,
        deployment: &DeploymentId,
        commits: &std::collections::BTreeMap<ployz_core::ServiceName, String>,
    ) -> Result<Vec<GitSource>, RpcError> {
        self.storage.write(|tx| build::pin(tx, deployment, commits))
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
        repository_id: u64,
        branch: &str,
    ) -> Result<Option<String>, RpcError> {
        self.storage
            .read(|tx| automation::head(tx, organization, repository_id, branch))
    }

    /// The Conditional Saves a push to `branch` may freeze or carry: Cloud reports
    /// the pull requests that merged, then which merge commits the head contains.
    ///
    /// # Errors
    ///
    /// `invalid_argument` for an ID out of range; `internal` on storage failure.
    pub fn pending_saves(
        &self,
        organization: &OrganizationId,
        repository_id: u64,
        branch: &str,
    ) -> Result<PendingSaves, RpcError> {
        let who = Actor::system(organization.clone());
        self.storage
            .read(|tx| conditional_save::pending(tx, &who, repository_id, branch))
    }

    /// [`Command::CreateBranch`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn create_branch(&self, who: &Actor, create: &CreateBranch) -> Result<Branched, RpcError> {
        self.storage
            .write(|tx| branch::create_branch(tx, who, create))
    }

    /// [`Command::Move`].
    ///
    /// # Errors
    /// As [`write`](Self::write); `invalid_argument` unless the sides are a Branch
    /// and its Parent, or for a secret that wants a fresh value; `conflict` for a
    /// stale version, nothing to move, a Branch being removed, or an Update while
    /// the Branch doesn't run its Working State.
    pub fn move_changes(&self, who: &Actor, request: &Move) -> Result<Moved, RpcError> {
        self.storage
            .write(|tx| branch::move_changes(tx, who, &self.sealing, request))
    }

    /// [`Query::Move`].
    ///
    /// # Errors
    /// As [`read`](Self::read); `invalid_argument` unless the sides are a Branch
    /// and its Parent.
    pub fn move_view(&self, who: &Actor, query: &MoveQuery) -> Result<MoveView, RpcError> {
        self.storage.read(|tx| branch::move_view(tx, who, query))
    }

    /// [`Command::CopyNode`].
    ///
    /// # Errors
    /// As [`write`](Self::write); `conflict` while the Branch doesn't run its
    /// Working State.
    pub fn copy_node(&self, who: &Actor, copy: &CopyNode) -> Result<Branched, RpcError> {
        self.storage.write(|tx| branch::copy_node(tx, who, copy))
    }

    /// [`Command::KeepBranch`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn keep_branch(&self, who: &Actor, keep: &KeepBranch) -> Result<Branched, RpcError> {
        self.storage.write(|tx| branch::keep_branch(tx, who, keep))
    }

    /// [`Query::Environments`].
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn environments(
        &self,
        who: &Actor,
        query: &EnvironmentsQuery,
    ) -> Result<EnvironmentsView, RpcError> {
        self.storage
            .read(|tx| teardown::environments(tx, who, query))
    }

    /// [`Command::SetDefaultEnvironment`].
    ///
    /// # Errors
    /// As [`write`](Self::write); `conflict` for an Environment being removed.
    pub fn set_default_environment(
        &self,
        who: &Actor,
        set: &SetDefaultEnvironment,
    ) -> Result<EnvironmentsView, RpcError> {
        self.storage.write(|tx| teardown::set_default(tx, who, set))
    }

    /// [`Command::RemoveEnvironment`].
    ///
    /// # Errors
    /// As [`write`](Self::write); `conflict` for the Default Environment, one with
    /// Branches, or one that may still run on the Servers (`details.deployed`).
    pub fn remove_environment(
        &self,
        who: &Actor,
        remove: &RemoveEnvironment,
    ) -> Result<EnvironmentRemoved, RpcError> {
        self.storage.write(|tx| teardown::remove(tx, who, remove))
    }

    /// [`Query::Projects`].
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn projects(&self, who: &Actor) -> Result<ProjectsView, RpcError> {
        self.storage.read(|tx| teardown::projects(tx, who))
    }

    /// [`Command::RemoveProject`].
    ///
    /// # Errors
    /// As [`write`](Self::write); `conflict` while any of its Environments may still
    /// run on the Servers (`details.deployed`, `details.environment` the next to take off).
    pub fn remove_project(
        &self,
        who: &Actor,
        remove: &RemoveProject,
    ) -> Result<ProjectRemoved, RpcError> {
        self.storage
            .write(|tx| teardown::remove_project(tx, who, remove))
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

    /// [`Query::Branch`].
    ///
    /// # Errors
    /// As [`read`](Self::read); `invalid_argument` for an Environment that isn't a Branch.
    pub fn branch(&self, who: &Actor, query: &BranchQuery) -> Result<BranchView, RpcError> {
        self.storage.read(|tx| branch::branch(tx, who, query))
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
    /// Returns `not_found` unless GitHub holds or held it, or a storage error.
    pub fn github_build(&self, id: &GithubBuildId) -> Result<GithubBuild, RpcError> {
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

    /// What GitHub build `id` builds: the Deployment's lowering input with its
    /// secrets unsealed, the pinned commit, and the Service's latest receipt.
    /// In-process only: only the authorized runner receives it.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown build, or a storage error.
    pub fn github_input(
        &self,
        id: &GithubBuildId,
    ) -> Result<(serde_json::Value, String, Option<serde_json::Value>), RpcError> {
        self.storage
            .read(|tx| build::github_input(tx, id, &self.sealing))
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

    /// The Organization's Build Order.
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn build_order(&self, who: &Actor) -> Result<BuildOrderView, RpcError> {
        self.storage.read(|tx| builders::build_order(tx, who))
    }

    /// [`Command::SetBuildOrder`].
    ///
    /// # Errors
    /// Returns a storage error.
    pub fn set_build_order(
        &self,
        who: &Actor,
        set: &SetBuildOrder,
    ) -> Result<BuildOrderView, RpcError> {
        self.storage
            .write(|tx| builders::set_build_order(tx, who, set))
    }

    /// One Git build of a Deployment, with its log.
    ///
    /// # Errors
    /// As [`Self::read`]; `not_found` for a Deployment of another Organization or a
    /// Service it didn't build.
    pub fn build_log(&self, who: &Actor, query: &BuildLogQuery) -> Result<BuildLogView, RpcError> {
        self.storage
            .read(|tx| deployment::build_log(tx, who, query))
    }

    /// [`Command::SetPrPlan`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn set_pr_plan(&self, who: &Actor, set: &SetPrPlan) -> Result<PrPlansView, RpcError> {
        self.storage
            .write(|tx| pull_request::set_plan(tx, who, set))
    }

    /// [`Query::PrPlans`].
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn pr_plans(&self, who: &Actor, query: &PrPlansQuery) -> Result<PrPlansView, RpcError> {
        self.storage.read(|tx| pull_request::plans(tx, who, query))
    }

    /// [`Query::PullRequest`].
    ///
    /// # Errors
    /// As [`read`](Self::read).
    pub fn pull_request(
        &self,
        who: &Actor,
        query: &PullRequestQuery,
    ) -> Result<PullRequestView, RpcError> {
        self.storage.read(|tx| pull_request::view(tx, who, query))
    }
}
