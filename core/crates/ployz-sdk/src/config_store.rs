//! The Config Store for Cloud: `read` and `write` as Promises. Each call runs its
//! blocking database work on a worker thread, at most [`CONCURRENCY`] at once, and
//! ends `unavailable` when it waits or runs too long. The Store's in-process-only
//! operations are never bound here without their own caller checks: Cloud's worker
//! runs a Deployment in one call, so its secrets and evidence never reach JavaScript.

use std::sync::Arc;
use std::time::Duration;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{Actor, DeploymentId, OrganizationId, RunEvidence, RunnerId};
use tokio::sync::Semaphore;

use crate::{invalid_argument, rpc_to_napi};

/// Store calls running at once per handle; more wait their turn.
const CONCURRENCY: usize = 8;
/// How long a call may wait for its turn.
const QUEUE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a call may run. The database's own statement and lock timeouts end the
/// work itself; past this the caller stops waiting and the outcome is unknown.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// One Config Store over one database.
#[napi]
pub struct ConfigStore {
    store: Arc<ployz_store::ConfigStore>,
    permits: Arc<Semaphore>,
}

/// Open the Config Store at `url` (`postgres://…`, or `sqlite:PATH` in tests),
/// migrating its tables as needed. It seals secrets with a key derived from
/// `sealing_secret`, Cloud's encryption secret.
///
/// # Errors
/// Returns `invalid_argument` for an unsupported URL, or a storage error.
#[napi]
pub async fn open_config_store(url: String, sealing_secret: String) -> Result<ConfigStore> {
    let sealing = ployz_store::SealingKey::new(sealing_secret.as_bytes()).map_err(rpc_to_napi)?;
    let store = blocking(move || ployz_store::ConfigStore::open(&url, sealing)).await?;
    Ok(ConfigStore {
        store: Arc::new(store),
        permits: Arc::new(Semaphore::new(CONCURRENCY)),
    })
}

#[napi]
impl ConfigStore {
    /// Answer a `ConfigQuery` as the given Organization. `trusted` is what Cloud
    /// observed itself (`ConfigTrusted`), never the caller's.
    ///
    /// # Errors
    /// Returns the Store's RPC error, or `unavailable` when it is too busy or slow.
    #[napi]
    pub async fn read(
        &self,
        organization: String,
        query: serde_json::Value,
        trusted: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization)?;
        let query: ployz_store::Query = serde_json::from_value(query)
            .map_err(|_| invalid_argument("Expected a Config Store query"))?;
        let trusted = evidence(trusted)?;
        let store = Arc::clone(&self.store);
        self.run(move || store.read_trusted(&who, &query, &trusted))
            .await
    }

    /// Apply a `ConfigCommand` as the given Organization, in one transaction.
    /// `trusted` is evidence Cloud gathered itself (`ConfigTrusted`), never the caller's.
    ///
    /// # Errors
    /// Returns the Store's RPC error, or `unavailable` when it is too busy or slow.
    #[napi]
    pub async fn write(
        &self,
        organization: String,
        command: serde_json::Value,
        trusted: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization)?;
        let command: ployz_store::Command = serde_json::from_value(command)
            .map_err(|_| invalid_argument("Expected a Config Store command"))?;
        let trusted = evidence(trusted)?;
        let store = Arc::clone(&self.store);
        self.run(move || store.write_trusted(&who, &command, &trusted))
            .await
    }

    /// The Git Services Deployment `deployment` builds (`GitSource[]`), each with its
    /// pinned commit, if any. Only Cloud's worker calls this, to read their sources.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    #[napi]
    pub async fn deployment_sources(&self, deployment: String) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        let store = Arc::clone(&self.store);
        self.run(move || store.sources(&deployment)).await
    }

    /// Pin the commits Cloud resolved (`{runtime Service name: commit}`) for
    /// Deployment `deployment`'s Git Services; a pinned commit never changes. Resolves
    /// to every source with its pin. Only Cloud's worker calls this.
    ///
    /// # Errors
    /// Returns `conflict` once the Deployment was replaced, cancelled or ended,
    /// `invalid_argument` for a bad pin, or a storage error.
    #[napi]
    pub async fn pin_sources(
        &self,
        deployment: String,
        commits: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        let commits = serde_json::from_value(commits)
            .map_err(|_| invalid_argument("Expected commits by Service name"))?;
        let store = Arc::clone(&self.store);
        self.run(move || store.pin(&deployment, &commits)).await
    }

    /// Run the Organization's queued Deployment `deployment` as `runner` on one of
    /// `connections` (`Connection[]`), and resolve to its summary once its outcome is
    /// recorded. Its Git Services build from `checkouts` (`{runtime Service name:
    /// directory}`) at their pinned commits; `source_failure` says why Cloud could not
    /// read them, and is recorded as the reason nothing ran. Only Cloud's worker calls
    /// this. It takes as long as its builds and Deploy do.
    ///
    /// # Errors
    /// Returns `conflict` when this runner has nothing to run, or a storage error.
    #[napi]
    pub async fn run_deployment(
        &self,
        organization: String,
        deployment: String,
        runner: String,
        connections: serde_json::Value,
        checkouts: Option<serde_json::Value>,
        source_failure: Option<String>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization)?;
        let (deployment, runner) = ids(deployment, runner)?;
        let connections = serde_json::from_value(connections)
            .map_err(|_| invalid_argument("invalid management connections"))?;
        let checkouts = match source_failure {
            Some(reason) => Err(reason),
            None => Ok(checkouts
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| invalid_argument("Expected checkouts by Service name"))?
                .unwrap_or_default()),
        };
        let summary = ployz::sdk::run_deployment(
            Arc::clone(&self.store),
            who,
            deployment,
            runner,
            connections,
            checkouts,
        )
        .await
        .map_err(rpc_to_napi)?;
        serde_json::to_value(summary).map_err(|error| Error::from_reason(error.to_string()))
    }

    /// Record that `runner` stopped without finishing Deployment `deployment`: its
    /// outcome is unknown once it prepared it. Recording it after an outcome changes
    /// nothing.
    ///
    /// # Errors
    /// Returns `conflict` when another runner owns the Deployment, or a storage error.
    #[napi]
    pub async fn abandon_deployment(
        &self,
        deployment: String,
        runner: String,
    ) -> Result<serde_json::Value> {
        let (deployment, runner) = ids(deployment, runner)?;
        let store = Arc::clone(&self.store);
        self.run(move || store.record(&deployment, &runner, RunEvidence::Abandoned))
            .await
    }
}

impl ConfigStore {
    async fn run<T: serde::Serialize + Send + 'static>(
        &self,
        work: impl FnOnce() -> std::result::Result<T, RpcError> + Send + 'static,
    ) -> Result<serde_json::Value> {
        let permit = tokio::time::timeout(QUEUE_TIMEOUT, Arc::clone(&self.permits).acquire_owned())
            .await
            .map_err(|_| unavailable("The Config Store is busy; retry"))?
            .map_err(|_| unavailable("The Config Store is closed"))?;
        let value = blocking(move || {
            // The turn ends when the work does, even if the caller stopped waiting.
            let _permit = permit;
            work()
        });
        let value = tokio::time::timeout(RUN_TIMEOUT, value)
            .await
            .map_err(|_| {
                unavailable("The Config Store did not answer in time; the outcome is unknown")
            })??;
        serde_json::to_value(value).map_err(|error| Error::from_reason(error.to_string()))
    }
}

fn actor(organization: String) -> Result<Actor> {
    Ok(Actor {
        organization: OrganizationId::parse(organization).map_err(rpc_to_napi)?,
    })
}

fn ids(deployment: String, runner: String) -> Result<(DeploymentId, RunnerId)> {
    Ok((
        DeploymentId::parse(deployment).map_err(rpc_to_napi)?,
        RunnerId::parse(runner).map_err(rpc_to_napi)?,
    ))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::result::Result<T, RpcError> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| unavailable("The Config Store call stopped unexpectedly"))?
        .map_err(rpc_to_napi)
}

fn unavailable(message: &str) -> Error {
    rpc_to_napi(RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.to_owned(),
        details: serde_json::Value::Null,
    })
}

/// Evidence Cloud gathered itself (`ConfigTrusted`), or none.
fn evidence(trusted: Option<serde_json::Value>) -> Result<ployz_store::Trusted> {
    Ok(trusted
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| invalid_argument("Expected Config Store evidence"))?
        .unwrap_or_default())
}
