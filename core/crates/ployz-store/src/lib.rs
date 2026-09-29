//! The Config Store: all authored configuration and its history, behind one
//! synchronous interface. `read` answers a [`Query`] and `write` applies a
//! [`Command`], each in one transaction; in-process callers use the typed method
//! for each, which runs the same code. Storage is its only I/O.

pub mod catalog;
mod command;
mod deployment;
mod error;
mod git;
mod id;
mod query;
mod review;
mod scope;
mod sealing;
mod settings;
mod storage;
mod trusted;
mod variables;

use ployz_core::RpcError;

pub use command::*;
pub use deployment::{
    Claimed, DeploymentStatus, DeploymentSummary, DeploymentView, NodeOutcome, NodeStatus, Outcome,
    RunEvidence,
};
pub use git::{AuthorizedRepository, CreateGitService};
pub use id::*;
pub use query::*;
pub use review::{DiffView, NodeChange};
pub use scope::{EnvironmentRef, EnvironmentSummary};
pub use sealing::SealingKey;
pub use settings::{Apply, SettingPath};
pub use trusted::Trusted;

/// Who is asking, and in which Organization. Every read and write is scoped to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor {
    /// The Organization whose configuration it reads and writes.
    pub organization: OrganizationId,
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
        self.storage.read(|tx| query::run(tx, who, query))
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

    /// [`Command::Publish`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn publish(&self, who: &Actor, publish: &Publish) -> Result<Published, RpcError> {
        self.storage.write(|tx| command::publish(tx, who, publish))
    }

    /// [`Command::Discard`].
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn discard(&self, who: &Actor, discard: &Discard) -> Result<Discarded, RpcError> {
        self.storage.write(|tx| command::discard(tx, who, discard))
    }

    /// [`Command::Admit`]: publish if needed, then freeze and queue a Deployment.
    ///
    /// # Errors
    /// As [`write`](Self::write).
    pub fn admit(&self, who: &Actor, admit: &Admit) -> Result<DeploymentSummary, RpcError> {
        self.storage.write(|tx| command::admit(tx, who, admit))
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
    /// never exposed over HTTPS.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, `conflict` when another runner
    /// owns it, a newer one replaced it, or it ended, or a storage error.
    pub fn claim(&self, deployment: &DeploymentId, runner: &RunnerId) -> Result<Claimed, RpcError> {
        self.storage
            .write(|tx| deployment::claim(tx, deployment, runner, &self.sealing))
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
    ) -> Result<Written, RpcError> {
        self.storage
            .write(|tx| deployment::record(tx, deployment, runner, evidence))
            .map(Written::Deployment)
    }
}
