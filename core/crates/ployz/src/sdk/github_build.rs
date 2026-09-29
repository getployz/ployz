//! GitHub as a Builder for Store Deployments. Cloud dispatches the repository's
//! build workflow and follows the run; these calls do the parts that touch the
//! Store's secrets or a Machine, so neither leaves Rust except to the one runner
//! the Store authorized:
//!
//!   start (reuse or dispatch) ──▶ check-in (grant + inputs) ──▶ reports … ──▶ finish (end grant ──▶ receipt)
//!
//! A run that never checked in, or that GitHub failed for its own reasons, is
//! skipped: the next Builder of the build's walk takes it. A failed build step fails it.

use std::collections::BTreeMap;
use std::sync::Arc;

use ployz_core::{
    BuildGrantId, BuildGrantRepository, EndBuildGrantRequest, MintBuildGrantRequest,
    RETAINED_DIGEST_TAG_PREFIX, RpcError, RpcErrorCode,
};
use ployz_store::{
    BuildStatus, ConfigStore, GithubBuildId, GithubClaims, GithubEnd, GithubGrant, GithubReport,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::build::OutsideBuild;
use super::preparation::{BuildReceipt, OutsideBuildInput};
use super::store_runner::{internal, log_line, only};
use super::{Session, connect_connections};
use crate::connect::SystemConnector;
use crate::context::Connection;

/// What starting a GitHub build found.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GithubStart {
    /// An earlier build's image, still on a Machine, served it: nothing to dispatch.
    Reused,
    /// Dispatch the workflow on this GitHub-hosted runner, native to its one platform.
    Dispatch { runner: String },
    /// GitHub can't take it; the Store recorded why, for the next Builder.
    Skipped { message: String },
}

/// Decide how GitHub build `id` starts: reuse the Service's latest image when a
/// Machine still holds it for this commit, else dispatch to the runner of its one
/// platform. A Service that needs several platforms, or a Cluster Cloud can't read,
/// skips GitHub.
///
/// # Errors
/// Returns the Store's `conflict` when the build already started or ended.
pub async fn github_start(
    store: Arc<ConfigStore>,
    id: GithubBuildId,
    connections: Vec<Connection>,
) -> Result<GithubStart, RpcError> {
    let (input, commit, receipt) = call(&store, {
        let id = id.clone();
        move |store| store.github_input(&id)
    })
    .await?;
    let deployment = only(&input, &id.service);
    let receipt = receipt.and_then(|receipt| serde_json::from_value(receipt).ok());
    let outside = match session(connections).await {
        Ok(session) => {
            let outside = outside(&session, deployment, commit, receipt).await;
            session.close().await;
            outside
        }
        Err(error) => Err(error),
    };
    let end = match outside {
        Ok(OutsideBuild::Reuse { receipt, .. }) => GithubEnd::Built {
            receipt: serde_json::to_value(receipt).expect("a build receipt is JSON"),
        },
        Ok(OutsideBuild::Build { platforms }) => match platforms.as_slice() {
            [platform] => {
                let runner = if platform == "linux/arm64" {
                    "ubuntu-24.04-arm"
                } else {
                    "ubuntu-latest"
                };
                return Ok(GithubStart::Dispatch {
                    runner: runner.to_owned(),
                });
            }
            _ => GithubEnd::Skipped {
                message: format!(
                    "GitHub builds one platform, and this Service runs on {}",
                    platforms.join(" and ")
                ),
            },
        },
        Err(error) => GithubEnd::Skipped {
            message: format!("Cloud couldn't read your Servers: {}", error.message),
        },
    };
    let start = match &end {
        GithubEnd::Skipped { message } => GithubStart::Skipped {
            message: message.clone(),
        },
        GithubEnd::Built { .. } | GithubEnd::Failed { .. } => GithubStart::Reused,
    };
    call(&store, move |store| store.github_end(&id, None, &end)).await?;
    Ok(start)
}

/// A receipt that can't be checked is only a missed shortcut: GitHub builds.
async fn outside(
    session: &Session,
    deployment: Value,
    commit: String,
    receipt: Option<BuildReceipt>,
) -> Result<OutsideBuild, RpcError> {
    let input = |receipt| OutsideBuildInput {
        deployment: deployment.clone(),
        commit: commit.clone(),
        receipt,
    };
    match session.outside_build(input(receipt.clone())).await {
        Err(_) if receipt.is_some() => session.outside_build(input(None)).await,
        outside => outside,
    }
}

/// What a runner receives at check-in. Holds the grant and the build secrets.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubCheckIn {
    pub grant: String,
    pub commit: String,
    pub fingerprint: String,
    /// The ployz version that computed the fingerprint: the runner installs it.
    pub ployz_version: String,
    /// The Deployment's lowering input narrowed to this Service, secrets unsealed.
    pub deployment: Value,
}

/// A runner's one check-in, with the OIDC `claims` Cloud verified: the build
/// starts. Mints a Build Grant on a Machine and returns it with the Service's build
/// inputs. Accepted once, while GitHub still holds the build.
///
/// # Errors
/// Returns `unauthenticated` for a token of another repository, workflow or run,
/// `conflict` once it checked in or is no longer wanted, or the Machine's refusal.
pub async fn github_check_in(
    store: Arc<ConfigStore>,
    id: GithubBuildId,
    claims: GithubClaims,
    connections: Vec<Connection>,
) -> Result<GithubCheckIn, RpcError> {
    let build = call(&store, {
        let id = id.clone();
        move |store| store.github_authorize(&id, &claims)
    })
    .await?;
    if build.status != BuildStatus::Building || build.grant.is_some() {
        return Err(conflict(
            "This build already checked in or is no longer wanted",
        ));
    }
    let (input, commit, _) = call(&store, {
        let id = id.clone();
        move |store| store.github_input(&id)
    })
    .await?;
    let deployment = only(&input, &id.service);
    let fingerprint = super::expected_fingerprints(
        deployment.clone(),
        BTreeMap::from([(id.service.clone(), commit.clone())]),
        BTreeMap::new(),
    )?
    .remove(&id.service)
    .ok_or_else(|| internal("The build has no fingerprint"))?;
    let session = session(connections).await?;
    let minted = mint(&session, &id).await;
    let (machine, minted) = match minted {
        Ok(minted) => minted,
        Err(error) => {
            session.close().await;
            return Err(error);
        }
    };
    let grant = GithubGrant {
        id: minted.id.to_string(),
        machine,
        fingerprint: fingerprint.clone(),
    };
    let run_id = build.run.run_id;
    let checked_in = call(&store, {
        let id = id.clone();
        move |store| store.github_check_in(&id, run_id, &grant)
    })
    .await;
    if let Err(error) = checked_in {
        // Lost to another check-in or the walk moving on: the secret never left, but end it.
        let _ = session
            .end_build_grant(EndBuildGrantRequest { id: minted.id })
            .await;
        session.close().await;
        return Err(error);
    }
    session.close().await;
    Ok(GithubCheckIn {
        grant: minted.grant.to_secret_string(),
        commit,
        fingerprint,
        ployz_version: super::preparation::VERSION.to_owned(),
        deployment,
    })
}

async fn mint(
    session: &Session,
    id: &GithubBuildId,
) -> Result<(ployz_core::MachineId, ployz_core::BuildGrantMinted), RpcError> {
    // Inspect first: a failure after the mint would leave an unclaimed grant.
    let machine = session.inspect().await?.id;
    let minted = session
        .mint_build_grant(MintBuildGrantRequest {
            repository: repository(id)?,
        })
        .await?;
    Ok((machine, minted))
}

/// The one repository a runner's Build Grant may push into.
fn repository(id: &GithubBuildId) -> Result<BuildGrantRepository, RpcError> {
    BuildGrantRepository::parse(format!("ployz-build/{}", id.service))
        .map_err(|_| internal("The Service has no Build Grant repository"))
}

/// One batch of a runner's `ployz build --events` lines, from line `from`, and once
/// its build ended, the platforms it built (empty: it failed). `installFailed`: it
/// couldn't install ployz, which GitHub failing it, not the build.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StepsReport {
    from: u64,
    events: Vec<Line>,
    #[serde(default)]
    platforms: Option<Vec<String>>,
    #[serde(default)]
    install_failed: Option<String>,
}

#[derive(Deserialize)]
struct Line {
    event: Value,
}

/// What a report did: how many lines were taken, and whether it was the last.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GithubReported {
    pub received: u64,
    pub ended: bool,
}

/// Take a runner's report, with the OIDC `claims` Cloud verified. Its lines go to
/// the build's log; a final one marks the build ended, for [`github_finish`].
///
/// # Errors
/// Returns `unauthenticated` as [`github_check_in`], `invalid_argument` for a body
/// that is not a report, and the Store's `conflict` before check-in, after the end,
/// or for missing lines.
pub async fn github_report(
    store: Arc<ConfigStore>,
    id: GithubBuildId,
    claims: GithubClaims,
    report: Value,
) -> Result<GithubReported, RpcError> {
    let report: StepsReport = serde_json::from_value(report).map_err(|_| RpcError {
        code: RpcErrorCode::InvalidArgument,
        message: "The report is not Build Steps".to_owned(),
        details: Value::Null,
    })?;
    let mut lines: Vec<String> = report
        .events
        .iter()
        .map(|line| log_line(&line.event))
        .collect();
    let platforms = match report.install_failed {
        // No final report: GitHub failed it, so the next Builder takes it.
        Some(version) => {
            lines.push(format!("GitHub couldn't install ployz {version}\n"));
            None
        }
        None => report.platforms,
    };
    let ended = platforms.is_some();
    let report = GithubReport {
        from: report.from,
        lines,
        platforms,
    };
    let received = call(&store, move |store| {
        let build = store.github_authorize(&id, &claims)?;
        store.github_report(&id, build.run.run_id, &report)
    })
    .await?;
    Ok(GithubReported { received, ended })
}

/// How [`github_finish`] left a build.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubFinish {
    /// Still on GitHub: its Machine couldn't be reached to end the grant yet.
    Waiting,
    /// Where the build is now: built, failed, or pending for the next Builder.
    Ended(BuildStatus),
}

/// End a GitHub build whose run reported its end, completed, or ran out of time
/// (`timed_out`, and Cloud cancelled it): end its grant on the Machine that minted
/// it, and write the receipt from the digest it received. A run that never checked
/// in, or that GitHub failed for its own reasons, is skipped for the next Builder.
///
/// # Errors
/// Returns a storage error; the Store's `conflict` means the build already ended.
pub async fn github_finish(
    store: Arc<ConfigStore>,
    id: GithubBuildId,
    timed_out: bool,
    connections: Vec<Connection>,
) -> Result<GithubFinish, RpcError> {
    let build = call(&store, {
        let id = id.clone();
        move |store| store.github_build(&id)
    })
    .await?;
    if build.status != BuildStatus::Building {
        return Ok(GithubFinish::Ended(build.status));
    }
    let run_id = build.run.run_id;
    let end = match &build.grant {
        None => GithubEnd::Skipped {
            message: if timed_out {
                "no runner started the build in time".to_owned()
            } else {
                "the run ended before it started the build".to_owned()
            },
        },
        Some(grant) => {
            let Some(pushed) = end_grant(grant, connections).await else {
                if !timed_out {
                    return Ok(GithubFinish::Waiting);
                }
                let end = GithubEnd::Failed {
                    message: "your Machine couldn't be reached to confirm the push".to_owned(),
                };
                return settle(&store, id, run_id, end).await;
            };
            match (pushed, build.platforms.as_deref()) {
                (Some(pushed), Some(platforms)) if !platforms.is_empty() => {
                    let tag = format!(
                        "{}:{RETAINED_DIGEST_TAG_PREFIX}{}",
                        repository(&id)?,
                        pushed.hex()
                    );
                    let receipt = BuildReceipt {
                        fingerprint: grant.fingerprint.clone(),
                        machine_id: grant.machine.clone(),
                        image: ployz_build::BuiltImage {
                            reference: pushed.to_string(),
                            tags: vec![tag],
                            platforms: platforms.to_vec(),
                            location: "build-grant".to_owned(),
                        },
                    };
                    GithubEnd::Built {
                        receipt: serde_json::to_value(receipt).expect("a build receipt is JSON"),
                    }
                }
                (_, Some([])) => GithubEnd::Failed {
                    message: "a build step failed".to_owned(),
                },
                _ if timed_out => GithubEnd::Skipped {
                    message: "the run ran out of time".to_owned(),
                },
                _ => GithubEnd::Skipped {
                    message: "the run ended without pushing an image".to_owned(),
                },
            }
        }
    };
    settle(&store, id, run_id, end).await
}

async fn settle(
    store: &Arc<ConfigStore>,
    id: GithubBuildId,
    run_id: u64,
    end: GithubEnd,
) -> Result<GithubFinish, RpcError> {
    call(store, move |store| {
        store.github_end(&id, Some(run_id), &end)
    })
    .await
    .map(GithubFinish::Ended)
}

/// End `grant` on the Machine that minted it: what it received, if anything;
/// none while that Machine can't be reached. Idempotent.
async fn end_grant(
    grant: &GithubGrant,
    connections: Vec<Connection>,
) -> Option<Option<ployz_core::ImageDigest>> {
    let connections: Vec<Connection> = connections
        .into_iter()
        .filter(|connection| connection.machine_id() == Some(&grant.machine))
        .collect();
    let session = session(connections).await.ok()?;
    let id = BuildGrantId::parse(&grant.id).ok()?;
    let ended = session.end_build_grant(EndBuildGrantRequest { id }).await;
    session.close().await;
    match ended {
        Ok(ended) => Some(ended.pushed),
        // The grant expired or its Machine restarted: nothing more can arrive.
        Err(error) if error.code == RpcErrorCode::NotFound => Some(None),
        Err(_) => None,
    }
}

/// Stop Deployment `id`'s builds still on GitHub: end each grant and fail each
/// build. Returns them, for Cloud to cancel their runs. Idempotent.
///
/// # Errors
/// Returns a storage error.
pub async fn github_cancel(
    store: Arc<ConfigStore>,
    deployment: ployz_store::DeploymentId,
    connections: Vec<Connection>,
) -> Result<Vec<ployz_store::GithubBuild>, RpcError> {
    let builds = call(&store, move |store| store.github_outstanding(&deployment)).await?;
    for build in &builds {
        if let Some(grant) = &build.grant {
            // ponytail: a Machine out of reach keeps the grant until it expires by itself.
            let _ = end_grant(grant, connections.clone()).await;
        }
        let (id, run_id) = (build.id.clone(), build.run.run_id);
        let end = GithubEnd::Failed {
            message: "cancelled".to_owned(),
        };
        // Another step may have ended it meanwhile.
        let _ = call(&store, move |store| {
            store.github_end(&id, Some(run_id), &end)
        })
        .await;
    }
    Ok(builds)
}

async fn session(connections: Vec<Connection>) -> Result<Session, RpcError> {
    if connections.is_empty() {
        return Err(RpcError {
            code: RpcErrorCode::Unavailable,
            message: "No Server is enrolled in this Organization".to_owned(),
            details: Value::Null,
        });
    }
    connect_connections(connections, Arc::new(SystemConnector::default())).await
}

fn conflict(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: message.to_owned(),
        details: Value::Null,
    }
}

/// Store calls block on the database, so they run off the async threads.
async fn call<T: Send + 'static>(
    store: &Arc<ConfigStore>,
    work: impl FnOnce(&ConfigStore) -> Result<T, RpcError> + Send + 'static,
) -> Result<T, RpcError> {
    let store = Arc::clone(store);
    tokio::task::spawn_blocking(move || work(&store))
        .await
        .map_err(|_| internal("A Config Store call stopped unexpectedly"))?
}
