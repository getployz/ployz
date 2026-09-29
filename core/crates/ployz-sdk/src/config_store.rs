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

/// Which of `connections`' Servers hold each Docker Volume named in `sought`: the
/// evidence (`VolumeObservation`) Cloud admits a Deploy that removes deployed
/// Volumes with. Servers that don't answer are named, never assumed empty.
///
/// # Errors
/// Returns `invalid_argument` for malformed input, or the connection's error when
/// no Server can be reached.
#[napi]
pub async fn observe_volumes(
    connections: serde_json::Value,
    sought: Vec<String>,
) -> Result<serde_json::Value> {
    let connections = serde_json::from_value(connections)
        .map_err(|_| invalid_argument("invalid management connections"))?;
    let sought = sought
        .into_iter()
        .map(ployz_core::DockerVolumeName::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid_argument("invalid Docker Volume name"))?;
    let observed = ployz::sdk::observe_volumes(connections, sought)
        .await
        .map_err(rpc_to_napi)?;
    serde_json::to_value(observed).map_err(|error| Error::from_reason(error.to_string()))
}

#[napi]
impl ConfigStore {
    /// Answer a `ConfigQuery` as the given Organization.
    ///
    /// # Errors
    /// Returns the Store's RPC error, or `unavailable` when it is too busy or slow.
    #[napi]
    pub async fn read(
        &self,
        organization: String,
        query: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let who = actor(organization)?;
        let query: ployz_store::Query = serde_json::from_value(query)
            .map_err(|_| invalid_argument("Expected a Config Store query"))?;
        let store = Arc::clone(&self.store);
        self.run(move || store.read(&who, &query)).await
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
        let trusted: ployz_store::Trusted = trusted
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| invalid_argument("Expected Config Store evidence"))?
            .unwrap_or_default();
        let store = Arc::clone(&self.store);
        self.run(move || store.write_trusted(&who, &command, &trusted))
            .await
    }

    /// Run the Organization's queued Deployment `deployment` as `runner` on one of
    /// `connections` (`Connection[]`), and resolve to its summary once its outcome is
    /// recorded. Only Cloud's worker calls this. It takes as long as the Deploy does.
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
    ) -> Result<serde_json::Value> {
        let who = actor(organization)?;
        let (deployment, runner) = ids(deployment, runner)?;
        let connections = serde_json::from_value(connections)
            .map_err(|_| invalid_argument("invalid management connections"))?;
        let summary = ployz::sdk::run_deployment(
            Arc::clone(&self.store),
            who,
            deployment,
            runner,
            connections,
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
