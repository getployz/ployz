//! The Config Store for Cloud: `read` and `write` as Promises. Every Store call,
//! the runner's and GitHub's included, goes through `ployz::sdk::store_call`: on a
//! worker thread, bounded, ending `unavailable` when it waits or runs too long.
//! The Store's in-process-only operations are never bound here without their own caller checks: Cloud's worker
//! runs a Deployment in one call, so its secrets and evidence never reach JavaScript.

use std::collections::BTreeMap;
use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use ployz_core::{RpcError, ServiceName};
use ployz_store::{Actor, DeploymentId, GithubBuildId, OrganizationId, RunEvidence, RunnerId};
use serde::de::DeserializeOwned;

use crate::{invalid_argument, rpc_to_napi};

/// One Config Store over one database.
#[napi]
pub struct ConfigStore {
    store: Arc<ployz_store::ConfigStore>,
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
    let store = tokio::task::spawn_blocking(move || ployz_store::ConfigStore::open(&url, sealing))
        .await
        .map_err(|error| Error::from_reason(error.to_string()))?
        .map_err(rpc_to_napi)?;
    Ok(ConfigStore {
        store: Arc::new(store),
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
    let connections = connections_of(connections)?;
    let sought = sought
        .into_iter()
        .map(ployz_core::DockerVolumeName::parse)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| invalid_argument("invalid Docker Volume name"))?;
    to_json(ployz::sdk::observe_volumes(connections, sought).await)
}

/// Every copy of every Volume on `connections`' Servers, by role
/// (`CopyObservation`): what Cloud's sweep finds orphaned mirror slots with. Servers
/// that don't answer are named, never assumed empty.
///
/// # Errors
/// Returns `invalid_argument` for malformed input, or the connection's error when
/// no Server can be reached.
#[napi]
pub async fn observe_copies(connections: serde_json::Value) -> Result<serde_json::Value> {
    let connections = connections_of(connections)?;
    to_json(ployz::sdk::observe_copies(connections).await)
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
        let who = actor(organization, None)?;
        let query: ployz_store::Query = decode(query, "Expected a Config Store query")?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.read_trusted(&who, &query, &trusted))
            .await
    }

    /// Apply a `ConfigCommand` as `principal` (who Cloud authenticated; none for
    /// Cloud itself) in the given Organization, in one transaction. `trusted` is
    /// evidence Cloud gathered itself (`ConfigTrusted`), never the caller's.
    ///
    /// # Errors
    /// Returns the Store's RPC error, or `unavailable` when it is too busy or slow.
    #[napi]
    pub async fn write(
        &self,
        organization: String,
        command: serde_json::Value,
        trusted: Option<serde_json::Value>,
        principal: Option<String>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, principal)?;
        let command: ployz_store::Command = decode(command, "Expected a Config Store command")?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.commit(&who, &command, &trusted))
            .await
    }

    /// The pull requests (`PullRequestRef[]`) whose checks a change in Environment
    /// `environment` may move, such as a Deployment of it ending. Only Cloud calls this.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed ID, or a storage error.
    #[napi]
    pub async fn checks(&self, environment: String) -> Result<serde_json::Value> {
        let environment = ployz_store::EnvironmentId::parse(environment).map_err(rpc_to_napi)?;
        self.run(move |store| store.checks(&environment)).await
    }

    /// The Git Services Deployment `deployment` builds (`GitSource[]`), each with its
    /// pinned commit, if any. Only Cloud's worker calls this, to read their sources.
    ///
    /// # Errors
    /// Returns `not_found` for an unknown Deployment, or a storage error.
    #[napi]
    pub async fn deployment_sources(&self, deployment: String) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        self.run(move |store| store.sources(&deployment)).await
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
        let commits = decode(commits, "Expected commits by Service name")?;
        self.run(move |store| store.pin(&deployment, &commits))
            .await
    }

    /// Run the Organization's queued Deployment `deployment` as `runner` on one of
    /// `connections` (`Connection[]`), and resolve to its summary once its outcome is
    /// recorded. Its Git Services build from `sources.checkouts` (`{runtime Service
    /// name: directory}`) at their pinned commits, and its uploaded Services from
    /// `sources.upload`, the directory Cloud extracted its upload to (without it, they
    /// reuse a usable image or need a new upload); `sources.failure` says why Cloud
    /// could not read them, and is recorded as the reason nothing ran. Only Cloud's
    /// worker calls this. It takes as long as its builds and Deploy do.
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
        sources: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        #[derive(Default, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Sources {
            #[serde(default)]
            checkouts: BTreeMap<ServiceName, std::path::PathBuf>,
            upload: Option<std::path::PathBuf>,
            failure: Option<String>,
        }
        let who = actor(organization, None)?;
        let (deployment, runner) = ids(deployment, runner)?;
        let connections = connections_of(connections)?;
        let sources: Sources = sources
            .map(|sources| decode(sources, "Expected checkouts, upload and failure"))
            .transpose()?
            .unwrap_or_default();
        let checkouts = match sources.failure {
            Some(_) if !sources.checkouts.is_empty() || sources.upload.is_some() => {
                return Err(invalid_argument(
                    "Expected checkouts and an upload, or why they couldn't be read: not both",
                ));
            }
            Some(reason) => Err(reason),
            None => Ok(ployz::sdk::Sources {
                checkouts: sources.checkouts,
                upload: sources.upload,
            }),
        };
        // Only a Deployment of this Organization runs here.
        let owned = deployment.clone();
        self.run(move |store| store.read(&who, &ployz_store::DeploymentQuery { id: owned }))
            .await?;
        to_json(
            ployz::sdk::run_deployment(
                Arc::clone(&self.store),
                deployment,
                runner,
                connections,
                checkouts,
            )
            .await,
        )
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
        self.run(move |store| store.record(&deployment, &runner, RunEvidence::Abandoned))
            .await
    }

    /// Apply a `SystemEvent` Cloud observed of GitHub for the given Organization;
    /// resolves to what it did (`ConfigWritten`, `automated`). `trusted` carries how
    /// many Servers the Organization has (`ConfigTrusted.servers`). Only Cloud's GitHub
    /// workers call this.
    ///
    /// # Errors
    /// Returns `conflict` when a branch head's base is stale, or a storage error.
    #[napi]
    pub async fn system(
        &self,
        organization: String,
        event: serde_json::Value,
        trusted: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let event: ployz_store::SystemEvent = decode(event, "Expected a system event")?;
        let trusted = evidence(trusted)?;
        self.run(move |store| store.system(&who.organization, &event, &trusted))
            .await
    }

    /// Forget the Organization's configuration once it has no Project
    /// (`OrganizationRemoved`). Only Cloud's own Organization removal calls this.
    ///
    /// # Errors
    /// Returns `conflict` while it has a Project, or a storage error.
    #[napi]
    pub async fn remove_organization(&self, organization: String) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        self.run(move |store| store.remove_organization(&who)).await
    }

    /// The Volumes a Deploy put on the Organization's Servers (`AppliedVolume[]`):
    /// what Forget Servers lists before the user confirms.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi]
    pub async fn applied_volumes(&self, organization: String) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        self.run(move |store| store.applied_volumes(&who)).await
    }

    /// Every queued Deployment no runner claimed, admitted before `before` (Unix
    /// seconds), across Organizations (`Unclaimed[]`): what Cloud's sweep dispatches
    /// again.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi]
    pub async fn unclaimed(&self, before: i64) -> Result<serde_json::Value> {
        self.run(move |store| store.unclaimed(before)).await
    }

    /// The head of a GitHub branch the Store last saw for the Organization, or null:
    /// what Cloud compares a new head from. Only Cloud's GitHub workers call this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi]
    pub async fn branch_head(
        &self,
        organization: String,
        repository_id: i64,
        branch: String,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let (repository_id, branch) = github_branch(repository_id, &branch)?;
        self.run(move |store| store.branch_head(&who.organization, repository_id, &branch))
            .await
    }

    /// The Conditional Syncs a push to a GitHub branch may freeze or carry:
    /// `{standing: [number]}`. Only Cloud's GitHub workers call this.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi]
    pub async fn pending_syncs(
        &self,
        organization: String,
        repository_id: i64,
        branch: String,
    ) -> Result<serde_json::Value> {
        let who = actor(organization, None)?;
        let (repository_id, branch) = github_branch(repository_id, &branch)?;
        self.run(move |store| store.pending_syncs(&who.organization, repository_id, &branch))
            .await
    }
}

/// GitHub as a Builder: Cloud dispatches and follows the run; these do the parts
/// that touch the Store's secrets or a Machine.
#[napi]
impl ConfigStore {
    /// Start GitHub build `build` (`DEPLOYMENT.SERVICE`): `{kind: "reused"}`,
    /// `{kind: "dispatch", runner}` or `{kind: "skipped", message}`.
    ///
    /// # Errors
    /// Returns `conflict` once the build started or ended.
    #[napi]
    pub async fn github_start(
        &self,
        build: String,
        connections: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let (build, connections) = (github_id(&build)?, connections_of(connections)?);
        to_json(ployz::sdk::github_start(Arc::clone(&self.store), build, connections).await)
    }

    /// Hand a pinned build to GitHub run `run` (`GithubRun`).
    ///
    /// # Errors
    /// Returns `conflict` once the Deployment no longer wants it: cancel the run.
    #[napi]
    pub async fn github_dispatched(
        &self,
        build: String,
        run: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        let run = decode(run, "Expected a GitHub run")?;
        self.run(move |store| store.github_dispatched(&build, &run))
            .await
    }

    /// A build handed to GitHub (`GithubBuild`).
    ///
    /// # Errors
    /// Returns `not_found` unless GitHub holds or held it.
    #[napi]
    pub async fn github_build(&self, build: String) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        self.run(move |store| store.github_build(&build)).await
    }

    /// Skip GitHub for a build it can't take, before any run: the next Builder takes it.
    ///
    /// # Errors
    /// Returns `conflict` once the build started or ended.
    #[napi]
    pub async fn github_skip(&self, build: String, message: String) -> Result<serde_json::Value> {
        let build = github_id(&build)?;
        self.run(move |store| {
            store.github_end(&build, None, &ployz_store::GithubEnd::Skipped { message })
        })
        .await
    }

    /// A runner's check-in with its verified OIDC `claims` (`GithubClaims`): its Build
    /// Grant and build inputs, secrets included. Only the runner may receive them.
    ///
    /// # Errors
    /// Returns `unauthenticated` for another repository, workflow or run, `conflict`
    /// once it checked in or is no longer wanted, or the Machine's refusal.
    #[napi]
    pub async fn github_check_in(
        &self,
        build: String,
        claims: serde_json::Value,
        connections: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let (build, claims) = (github_id(&build)?, claims_of(claims)?);
        let connections = connections_of(connections)?;
        to_json(
            ployz::sdk::github_check_in(Arc::clone(&self.store), build, claims, connections).await,
        )
    }

    /// A runner's report of its `ployz build --events` lines: `{received, ended}`.
    ///
    /// # Errors
    /// As `githubCheckIn`, `invalid_argument` for a body that is not a report, and
    /// `conflict` before check-in, after the end, or for missing lines.
    #[napi]
    pub async fn github_report(
        &self,
        build: String,
        claims: serde_json::Value,
        report: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let (build, claims) = (github_id(&build)?, claims_of(claims)?);
        to_json(ployz::sdk::github_report(Arc::clone(&self.store), build, claims, report).await)
    }

    /// End a build whose run reported its end, completed or ran out of time:
    /// `"waiting"` while its Machine can't end the grant, else `{ended: status}`.
    ///
    /// # Errors
    /// Returns a storage error, or `conflict` when it ended meanwhile.
    #[napi]
    pub async fn github_finish(
        &self,
        build: String,
        timed_out: bool,
        connections: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let (build, connections) = (github_id(&build)?, connections_of(connections)?);
        to_json(
            ployz::sdk::github_finish(Arc::clone(&self.store), build, timed_out, connections).await,
        )
    }

    /// Stop Deployment `deployment`'s builds still on GitHub: end their grants and
    /// fail them. Returns them (`GithubBuild[]`) so Cloud cancels their runs.
    ///
    /// # Errors
    /// Returns a storage error.
    #[napi]
    pub async fn github_cancel(
        &self,
        deployment: String,
        connections: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let deployment = DeploymentId::parse(deployment).map_err(rpc_to_napi)?;
        let connections = connections_of(connections)?;
        to_json(ployz::sdk::github_cancel(Arc::clone(&self.store), deployment, connections).await)
    }
}

fn github_id(build: &str) -> Result<GithubBuildId> {
    GithubBuildId::parse(build).map_err(rpc_to_napi)
}

fn claims_of(claims: serde_json::Value) -> Result<ployz_store::GithubClaims> {
    decode(claims, "Expected OIDC claims")
}

fn connections_of(connections: serde_json::Value) -> Result<Vec<ployz::context::Connection>> {
    decode(connections, "invalid management connections")
}

fn repository(repository_id: i64) -> Result<u64> {
    u64::try_from(repository_id).map_err(|_| invalid_argument("Expected a GitHub repository ID"))
}

/// Decode what JavaScript sent, its whole numbers as integers first; `refusal` names
/// what was expected.
fn decode<T: DeserializeOwned>(value: serde_json::Value, refusal: &str) -> Result<T> {
    serde_json::from_value(whole(value)).map_err(|_| invalid_argument(refusal))
}

fn to_json<T: serde::Serialize>(
    value: std::result::Result<T, RpcError>,
) -> Result<serde_json::Value> {
    serde_json::to_value(value.map_err(rpc_to_napi)?)
        .map_err(|error| Error::from_reason(error.to_string()))
}

impl ConfigStore {
    async fn run<T: serde::Serialize + Send + 'static>(
        &self,
        work: impl FnOnce(&ployz_store::ConfigStore) -> std::result::Result<T, RpcError>
        + Send
        + 'static,
    ) -> Result<serde_json::Value> {
        to_json(ployz::sdk::store_call(&self.store, work).await)
    }
}

fn actor(organization: String, principal: Option<String>) -> Result<Actor> {
    Ok(Actor {
        organization: OrganizationId::parse(organization).map_err(rpc_to_napi)?,
        principal: principal
            .map(ployz_store::Principal::parse)
            .transpose()
            .map_err(rpc_to_napi)?,
    })
}

/// A GitHub repository's ID and one of its branches, as JavaScript hands them over.
fn github_branch(
    repository_id: i64,
    branch: &str,
) -> Result<(ployz_store::RepositoryId, ployz_store::BranchName)> {
    Ok((
        ployz_store::RepositoryId::parse(repository(repository_id)?).map_err(rpc_to_napi)?,
        ployz_store::BranchName::parse(branch).map_err(rpc_to_napi)?,
    ))
}

fn ids(deployment: String, runner: String) -> Result<(DeploymentId, RunnerId)> {
    Ok((
        DeploymentId::parse(deployment).map_err(rpc_to_napi)?,
        RunnerId::parse(runner).map_err(rpc_to_napi)?,
    ))
}

/// Evidence Cloud gathered itself (`ConfigTrusted`), or none.
fn evidence(trusted: Option<serde_json::Value>) -> Result<ployz_store::Trusted> {
    Ok(trusted
        .map(|trusted| decode(trusted, "Expected Config Store evidence"))
        .transpose()?
        .unwrap_or_default())
}

/// `value` with whole numbers as integers: JavaScript hands GitHub's IDs past 2^31
/// over as floats, which integer fields refuse.
fn whole(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Number(number) => match number.as_f64() {
            #[expect(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "only whole, non-negative values below 2^53 are converted"
            )]
            Some(float)
                if float.fract() == 0.0 && (0.0..9_007_199_254_740_992.0).contains(&float) =>
            {
                Value::from(float as u64)
            }
            Some(_) | None => Value::Number(number),
        },
        Value::Array(items) => Value::Array(items.into_iter().map(whole).collect()),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(key, value)| (key, whole(value)))
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::String(_) => value,
    }
}
