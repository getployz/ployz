//! The Config Store: all authored configuration and its history, behind one
//! synchronous interface. `read` answers a [`Query`] and `write` applies a
//! [`Command`], each in one transaction. Storage is its only I/O.

pub mod catalog;
mod command;
mod deployment;
mod error;
mod id;
mod query;
mod review;
mod scope;
mod settings;
mod storage;

use ployz_core::RpcError;

pub use command::*;
pub use deployment::{
    Claimed, DeploymentStatus, DeploymentSummary, DeploymentView, NodeOutcome, NodeStatus, Outcome,
    RunEvidence,
};
pub use id::*;
pub use query::*;
pub use review::{DiffView, NodeChange};
pub use scope::{EnvironmentRef, EnvironmentSummary};
pub use settings::Apply;

/// Who is asking, and in which Organization. Every read and write is scoped to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor {
    pub organization: OrganizationId,
}

/// One Config Store over one database.
pub struct ConfigStore {
    storage: storage::Storage,
}

impl ConfigStore {
    /// Open the Store at `url` (`sqlite:PATH`, or `sqlite::memory:` for tests),
    /// creating and migrating its tables as needed.
    ///
    /// # Errors
    /// Returns `invalid_argument` for an unsupported URL, or a storage error.
    pub fn open(url: &str) -> Result<Self, RpcError> {
        Ok(Self {
            storage: storage::Storage::open(url)?,
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
    pub fn write(&self, who: &Actor, command: Command) -> Result<Written, RpcError> {
        self.storage.write(|tx| command::run(tx, who, command))
    }

    /// What deploying would ship, from authored state alone.
    ///
    /// # Errors
    /// As [`Self::read`], plus `invalid_argument` when the Environment can't deploy.
    pub fn plan(&self, who: &Actor, query: &PlanQuery) -> Result<PlanView, RpcError> {
        self.storage
            .read(|tx| query::deployment::plan(tx, who, query))
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
        self.storage
            .read(|tx| deployment::view(tx, who, id))
    }

    /// Bind a queued Deployment to `runner` and return its frozen Deploy Intent.
    /// In-process only: never exposed over HTTPS.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, `conflict` when another runner
    /// owns it, a newer one replaced it, or it ended, or a storage error.
    pub fn claim(&self, deployment: &DeploymentId, runner: &RunnerId) -> Result<Claimed, RpcError> {
        self.storage
            .write(|tx| deployment::claim(tx, deployment, runner))
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
