//! A person's approval of a redeploy, such as a rollback, on Servers that enforce Device
//! approval. A redeploy has no branch head behind it, so this Device approves each Git
//! Service's commit instead: the person is the one who can judge it. Each Server checks
//! the approval itself in place of the head, along with the rule, its generation, the
//! image token and every setting, as it does for a push.
//!
//! This client verifies nothing Cloud relays. It shows each Service and commit, and
//! echoes the rule's ID and generation into each approval for the Servers to compare. The
//! Cluster key comes from the Servers over this Device's own connection.
//!
//!   probe ‖ redeployed view ‖ rule ─▶ sign ─▶ admit ─▶ builds end ─▶ sign again ─▶ run
//!
//! An approval lasts minutes and a build may take longer, so the Deployment waits, queued,
//! once its builds end, until this client approves the same commits again.

use std::collections::BTreeMap;
use std::time::Duration;

use clap::ArgMatches;
use ployz_core::authority::{
    CommitApproval, CommitSha, RuleId, Signed, Signer, SignerKey, Timestamp, validity,
};
use ployz_core::{RpcErrorCode, SealingPublicKey, ServiceName};
use ployz_store::{
    ApproveDeployment, BuildStatus, BuildView, Cancel, DeploymentId, DeploymentQuery,
    DeploymentSummary, DeploymentView, EnvironmentRef, RuleQuery,
};
use serde_json::json;

use super::super::Error;
use super::super::store::Store;
use super::approve::{self, Probe};

/// How often the redeploy is read while GitHub builds.
const BUILD_LOOK: Duration = Duration::from_secs(3);

/// A redeploy this Device approves, before admission and again once its builds end.
pub(in crate::handlers) struct Redeploy {
    signer: Signer,
    terms: Terms,
    /// Each Git Service the person redeploys, and the commit it was built from.
    commits: BTreeMap<ServiceName, CommitSha>,
    /// The approvals admission keeps, which a rebuild's grant Machine checks.
    pub(in crate::handlers) approvals: Vec<Signed<CommitApproval>>,
}

/// What every approval of one redeploy names besides its Service and commit.
struct Terms {
    /// As the Servers report it.
    cluster: SealingPublicKey,
    /// As Cloud relays them: a Server refuses any other than its own.
    rule: RuleId,
    generation: u64,
}

/// Prepare to approve a redeploy of `source`, when this CLI holds a Device key and a
/// Server enforces Device approval. None otherwise: the redeploy runs as before, and a
/// Server that enforces refuses what nobody approved.
///
/// # Errors
/// Refuses before admission when a Server can't check an approval, when the source built
/// no Service from a commit or built one from an upload, when its Environment has no
/// automatic deploy rule, and when the Servers or the Store can't be read.
pub(in crate::handlers) fn prepare(
    matches: &ArgMatches,
    store: &Store,
    source: &DeploymentId,
) -> Result<Option<Redeploy>, Error> {
    let key = super::super::device::key::path(&super::super::config_path(matches)?);
    let Ok(Some(secret)) = super::super::device::key::load(&key) else {
        return Ok(None);
    };
    // The Servers are asked while this reads the source, so the probe adds no wait.
    let (probe, view) = std::thread::scope(|scope| {
        let probe = scope.spawn(|| approve::probe(matches));
        let view = store.read(&DeploymentQuery { id: source.clone() });
        (probe.join().unwrap_or(Probe::Unknown), view)
    });
    let enforcing = match probe {
        Probe::Off => return Ok(None),
        Probe::Unknown => {
            crate::ui::warn(
                "Couldn't ask your servers whether they enforce device approval, so this \
                 redeploy isn't signed. Servers that enforce will refuse it.",
            );
            return Ok(None);
        }
        Probe::NoClusterKey => {
            return Err(Error::coded(
                RpcErrorCode::Unavailable,
                "Your servers enforce device approval, but none reported the cluster's key, \
                 so this device can't sign the redeploy. Run ployz status, then redeploy again.",
            ));
        }
        Probe::Enforcing(enforcing) => enforcing,
    };
    if !enforcing.commit_approval {
        return Err(Error::coded(
            RpcErrorCode::Unsupported,
            "Some of your servers run a Ployz too old to check your approval. Upgrade Ployz \
             on every server, then redeploy. Nothing deployed.",
        ));
    }
    let view = view?;
    let commits = git_builds(&view)?;
    let environment = &view.environment.name;
    let rule = store
        .read(&RuleQuery {
            environment: EnvironmentRef {
                project: Some(view.environment.project.clone()),
                environment: Some(environment.clone()),
            },
        })?
        .rule
        .and_then(|signed| signed.open().ok())
        .ok_or_else(|| {
            Error::coded(
                RpcErrorCode::Conflict,
                format!(
                    "{environment} has no automatic deploy rule. Approve one with ployz github \
                     rule approve, then redeploy. Nothing deployed."
                ),
            )
        })?;
    let redeploy = Redeploy {
        signer: Signer::from_seed(&secret),
        terms: Terms {
            cluster: enforcing.cluster,
            rule: rule.rule,
            generation: rule.generation,
        },
        commits,
        approvals: Vec::new(),
    };
    for (service, commit) in &redeploy.commits {
        crate::ui::stream(format_args!("Approving {service} at {}", short(*commit)));
    }
    let approvals = redeploy.signed()?;
    Ok(Some(Redeploy {
        approvals,
        ..redeploy
    }))
}

impl Redeploy {
    /// Wait for `admitted`'s builds, then approve the same commits again and hand the
    /// approvals to the Store, which lets the redeploy run. Cancels the redeploy on any
    /// failure, so nothing waits for an approval that won't come.
    ///
    /// # Errors
    /// Returns a build that failed or GitHub passed on, a redeploy that ended meanwhile,
    /// and the Store's refusal.
    pub(in crate::handlers) fn approve(
        self,
        store: &Store,
        admitted: &DeploymentSummary,
    ) -> Result<(), Error> {
        let approved = ended_builds(store, &admitted.id).and_then(|()| {
            store.write(&ApproveDeployment {
                deployment: admitted.id.clone(),
                approvals: self.signed()?,
            })
        });
        if approved.is_err() {
            // A failed cancel leaves the claim to fail it: it won't run unapproved.
            let _ = store.try_write(&Cancel {
                deployment: admitted.id.clone(),
            });
        }
        approved.map(drop)
    }

    /// An approval of each Service's commit, valid from now for as long as any may be.
    fn signed(&self) -> Result<Vec<Signed<CommitApproval>>, Error> {
        let issued_at = Timestamp::now();
        self.commits
            .iter()
            .map(|(service, commit)| {
                let body = CommitApproval {
                    cluster: self.terms.cluster,
                    rule: self.terms.rule,
                    generation: self.terms.generation,
                    service: service.clone(),
                    commit: *commit,
                    issued_at,
                    expires_at: issued_at.plus(validity::MAX_LIFETIME_MILLIS),
                    signer: SignerKey::Device(self.signer.public().into()),
                };
                Signed::sign(&body, &self.signer).map_err(Error::command)
            })
            .collect()
    }
}

/// Each Service `view` built, and the commit it built from.
///
/// # Errors
/// Refuses a source that built no Service from a commit, or one from an upload: no
/// approval can name what an upload holds.
fn git_builds(view: &DeploymentView) -> Result<BTreeMap<ServiceName, CommitSha>, Error> {
    if view.builds.is_empty() {
        return Err(Error::coded(
            RpcErrorCode::Conflict,
            format!(
                "{} built no service from a commit. On servers that enforce device approval, \
                 a redeploy runs only what GitHub built. Deploy with ployz deploy. Nothing \
                 deployed.",
                view.deployment.label()
            ),
        ));
    }
    view.builds
        .iter()
        .map(|build| Ok((build.service.clone(), built_from(build)?)))
        .collect()
}

/// The commit `build` built from.
fn built_from(build: &BuildView) -> Result<CommitSha, Error> {
    let Some(commit) = &build.commit else {
        return Err(Error::detailed(
            RpcErrorCode::InvalidArgument,
            format!(
                "{} builds from an upload. Servers that enforce device approval run only what \
                 GitHub built from a commit. Deploy it from an image pinned by digest. Nothing \
                 deployed.",
                build.service
            ),
            json!({ "service": build.service }),
        ));
    };
    CommitSha::parse(&commit.to_string()).map_err(Error::command)
}

/// Wait until every build of redeploy `id` ended with an image.
///
/// # Errors
/// Returns a build that failed or GitHub passed on, a redeploy that ended, and an
/// interrupt.
fn ended_builds(store: &Store, id: &DeploymentId) -> Result<(), Error> {
    let interrupted = crate::cancellation::interrupted().map_err(Error::command)?;
    let mut said = BTreeMap::new();
    loop {
        let view = store.read(&DeploymentQuery { id: id.clone() })?;
        if !view.deployment.status.in_flight() {
            return Err(Error::coded(
                RpcErrorCode::Conflict,
                "The redeploy ended before this device approved it. Nothing deployed.",
            ));
        }
        for build in &view.builds {
            if let Some(passed) = &build.github_skipped {
                return Err(unbuilt(build, passed));
            }
            if build.status == BuildStatus::Failed {
                return Err(unbuilt(
                    build,
                    build.message.as_deref().unwrap_or("its build failed"),
                ));
            }
            if said.insert(build.service.clone(), build.status) != Some(build.status)
                && build.status == BuildStatus::Building
            {
                crate::ui::stream(format_args!("Building {} on GitHub", building(build)));
            }
        }
        if view
            .builds
            .iter()
            .all(|build| matches!(build.status, BuildStatus::Built | BuildStatus::Reused))
        {
            return Ok(());
        }
        let cancelled = super::super::runtime()?.block_on(async {
            tokio::select! {
                () = interrupted.cancelled() => true,
                () = tokio::time::sleep(BUILD_LOOK) => false,
            }
        });
        if cancelled {
            return Err(Error::cancelled());
        }
    }
}

fn unbuilt(build: &BuildView, why: &str) -> Error {
    Error::detailed(
        RpcErrorCode::Conflict,
        format!(
            "GitHub didn't build {}: {why}. Nothing deployed.",
            building(build)
        ),
        json!({ "service": build.service }),
    )
}

fn building(build: &BuildView) -> String {
    match &build.commit {
        Some(commit) => format!("{} at {}", build.service, short_str(&commit.to_string())),
        None => build.service.to_string(),
    }
}

fn short(commit: CommitSha) -> String {
    short_str(&commit.to_string())
}

fn short_str(commit: &str) -> String {
    commit.chars().take(12).collect()
}

#[cfg(test)]
#[path = "built_tests.rs"]
mod tests;
