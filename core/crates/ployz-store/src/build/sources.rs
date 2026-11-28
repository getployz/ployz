//! Which Git Services a Deployment builds, the commit each is pinned to, and the Builders it tries.

use std::collections::BTreeMap;

use ployz_core::authority::{CommitApproval, Signed};
use ployz_core::config::{BuildMethod, SavedServiceIntent, ServiceGitAccess, ServiceSource};
use ployz_core::{MachineId, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::builders::{self, Builder};
use crate::deployment::{self, Stored};
use crate::error;
use crate::id::{BranchName, CommitSha, DeploymentId, EnvironmentId, RepositoryId, RepositoryName};
use crate::storage::{Tx, json_text};

use super::{BuildRow, BuildStatus, rows};

/// A Git Service a Deployment builds, and the commit it is pinned to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GitSource {
    /// Its Private DNS name, which a rename keeps.
    pub service: ServiceName,
    /// The GitHub repository, as `owner/name`.
    pub repository: RepositoryName,
    pub repository_id: RepositoryId,
    /// How Cloud reads it.
    pub access: ServiceGitAccess,
    /// The branch it follows; none once it was disconnected.
    pub branch: Option<BranchName>,
    /// The directory it builds from, inside the repository.
    pub root_dir: String,
    /// Its Dockerfile, relative to `root_dir`, when it builds from one.
    pub dockerfile_path: Option<String>,
    /// The commit it builds; none until pinned.
    pub commit: Option<CommitSha>,
    /// The Builders its build tries, in turn: its Preferred Builder, then the
    /// Organization's Build Order.
    pub builders: Vec<Builder>,
    /// The Server the servers try first, when it prefers one.
    pub preferred_machine: Option<MachineId>,
    /// Where its build is; none until pinned.
    pub status: Option<BuildStatus>,
    /// Why its build failed, or why the last Builder skipped it.
    pub message: Option<String>,
}

/// The Git Services Deployment `id` builds, each with its pin, if any.
pub(crate) fn sources(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<GitSource>, RpcError> {
    let stored = deployment::load(tx, id)?;
    sources_of(tx, &stored)
}

/// Pin the commits of `commits` (by runtime Service name) that aren't pinned yet, for
/// worker attempt `attempt`. A pinned commit never changes, and one its PR
/// Environment's pull request doesn't admit by `trusted`'s members never pins. Only the
/// attempt that owns the Deployment chooses its commits: an outranked one is `newer`.
pub(crate) fn pin(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    commits: &BTreeMap<ServiceName, CommitSha>,
    trusted: &crate::Trusted,
    attempt: &str,
) -> Result<(), RpcError> {
    let stored = pinner(tx, id, attempt)?;
    pin_stored(tx, &stored, (commits, None), trusted)
}

/// Deployment `id`, locked, when worker attempt `attempt` may pin its commits: it is in
/// flight and `attempt` owns it. Otherwise the refusal a pin from `attempt` gets, before
/// the pin commits anything.
pub(crate) fn pinner(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    attempt: &str,
) -> Result<Stored, RpcError> {
    let stored = deployment::locked(tx, id)?;
    if !stored.summary.status.in_flight() {
        return Err(error::conflict(
            "This Deployment is no longer waiting to run",
            json!({ "deployment": id, "status": stored.summary.status }),
        ));
    }
    if !super::github::owns(tx, &stored, attempt)? {
        return Err(super::github::newer_attempt(id));
    }
    Ok(stored)
}

/// Pin the commits of `commits` for Deployment `id` in the write that admits it, before
/// any worker attempt began: the Store chose them, so no attempt is checked. Each build of
/// the commit `approval` approves keeps it, and pins without its pull request's admission:
/// the caller checked the approval as a Server would.
pub(crate) fn pin_admitted(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    (commits, approval): (
        &BTreeMap<ServiceName, CommitSha>,
        Option<&Signed<CommitApproval>>,
    ),
    trusted: &crate::Trusted,
) -> Result<(), RpcError> {
    let stored = deployment::locked(tx, id)?;
    pin_stored(tx, &stored, (commits, approval), trusted)
}

fn pin_stored(
    tx: &mut dyn Tx,
    stored: &Stored,
    (commits, approval): (
        &BTreeMap<ServiceName, CommitSha>,
        Option<&Signed<CommitApproval>>,
    ),
    trusted: &crate::Trusted,
) -> Result<(), RpcError> {
    let id = &stored.summary.id;
    let sources = unrouted(tx, stored)?;
    for (service, commit) in commits {
        let Some(source) = sources.iter().find(|source| source.service == *service) else {
            return Err(error::invalid(
                format!("This Deployment builds no Git Service {service}"),
                json!({ "services": sources.iter().map(|source| &source.service).collect::<Vec<_>>() }),
            ));
        };
        let environment = &stored.summary.environment_id;
        let approved = approval.filter(|approval| {
            approval
                .open()
                .is_ok_and(|approved| approved.commit.to_string() == commit.as_str())
        });
        if approved.is_none()
            && !crate::pull_request::admits(
                tx,
                environment,
                (source.repository_id, commit),
                trusted,
            )?
        {
            return Err(error::invalid(
                format!("{commit} needs approval before it deploys"),
                json!({ "service": service, "commit": commit }),
            ));
        }
        pending(tx, id, service, Some(commit))?;
        if let Some(approved) = approved {
            tx.execute(
                "UPDATE config_build SET commit_approval = ?3 \
                 WHERE deployment_id = ?1 AND service = ?2",
                &[
                    id.as_str().into(),
                    service.as_str().into(),
                    json_text(approved).as_str().into(),
                ],
            )?;
        }
    }
    Ok(())
}

/// Add a pending build of `service` to Deployment `id`, of `commit` or of an upload,
/// unless it has one.
pub(super) fn pending(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    service: &ServiceName,
    commit: Option<&CommitSha>,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_build \
         (deployment_id, service, organization_id, commit_sha, status, message, log, reservable) \
         SELECT id, ?2, organization_id, ?3, 'pending', NULL, '', 1 \
         FROM config_deployment WHERE id = ?1 \
         ON CONFLICT (deployment_id, service) DO NOTHING",
        &[
            id.as_str().into(),
            service.as_str().into(),
            commit.map(CommitSha::as_str).into(),
        ],
    )
    .map(drop)
}

/// Give retry `id` a pending build of each commit its `source` pinned, which may reuse
/// the image `source` built, with that image's token and verified build, and the approval
/// of its commit.
pub(crate) fn retried(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    source: &DeploymentId,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_build \
         (deployment_id, service, organization_id, commit_sha, status, message, log, \
          receipt, image_token, verified, commit_approval, reservable) \
         SELECT ?1, service, organization_id, commit_sha, 'pending', NULL, '', receipt, \
          image_token, verified, commit_approval, 1 \
         FROM config_build WHERE deployment_id = ?2",
        &[id.as_str().into(), source.as_str().into()],
    )
    .map(drop)
}

/// Pin `service`'s build in Deployment `id` to the commit it runs, as built by the
/// Deployment that put it in Applied State, with the approval of that commit. A Service without a Git source has no
/// build to pin. Builds go by the runtime name, which a rename keeps.
pub(crate) fn pin_running(
    tx: &mut dyn Tx,
    (environment, id): (&EnvironmentId, &DeploymentId),
    service: &SavedServiceIntent,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_build \
         (deployment_id, service, organization_id, commit_sha, status, message, log, \
          commit_approval) \
         SELECT ?1, b.service, b.organization_id, b.commit_sha, 'pending', NULL, '', \
          b.commit_approval \
         FROM config_build b JOIN config_applied a ON a.deployment_id = b.deployment_id \
         WHERE a.environment_id = ?2 AND a.node_id = ?3 AND b.service = ?4 \
         AND b.commit_sha IS NOT NULL",
        &[
            id.as_str().into(),
            environment.as_str().into(),
            service.id.as_str().into(),
            service.config.private_dns.as_str().into(),
        ],
    )
    .map(drop)
}

/// The Services `stored` targets that have no source of their own: they build from
/// its upload.
pub(crate) fn uploads_of(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<BTreeMap<ServiceName, Option<ployz_core::UploadDigest>>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id() == service.id))
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
        .map(|service| {
            (
                service.config.private_dns.clone(),
                deployment::uploads::own(stored, &service.id).cloned(),
            )
        })
        .collect())
}

/// The Git Services `stored` builds, with the Builders each tries.
pub(crate) fn sources_of(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<GitSource>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    // A Deployment that doesn't lower fails its claim; until then its builds route as any.
    let target = match deployment::lowered(tx, stored, &saved) {
        Ok(lowered) => lowered.intent.target,
        Err(refused) if refused.code == ployz_core::RpcErrorCode::InvalidArgument => Vec::new(),
        Err(error) => return Err(error),
    };
    routed(tx, stored, &saved, &target)
}

/// The Git Services `stored` builds from `saved`, each with the Builders it tries: only
/// GitHub for one that builds under the automatic deploy rule, and only the Servers for a
/// free Build of `target`.
pub(crate) fn routed(
    tx: &mut dyn Tx,
    stored: &Stored,
    saved: &ployz_core::config::SavedEnvironmentIntent,
    target: &[ployz_core::RequestedServiceSpec],
) -> Result<Vec<GitSource>, RpcError> {
    let organization = deployment::organization(tx, &stored.summary.id)?;
    let free = deployment::versions::free_builds(
        tx,
        (
            &stored.summary.environment_id,
            organization.as_str(),
            stored.namespace(),
        ),
        saved,
        (&stored.nodes, target),
    )?;
    let mut sources = Vec::new();
    for (id, mut source) in git_sources(tx, stored, saved)? {
        (source.builders, source.preferred_machine) = builders::walk(
            tx,
            organization.as_str(),
            &stored.summary.environment_id,
            &id,
        )?;
        // A build under the automatic deploy rule runs only on the rule's builder, which
        // GitHub runs: a Server would build it with no approval of its own to show, and
        // every Server that enforces refuses that.
        if super::github::rule_of(tx, stored, &source.service)?.is_some() {
            if !source.builders.contains(&Builder::Github) && source.message.is_none() {
                source.message = Some(
                    "Its automatic deploy rule builds it on GitHub, which your Build Order \
                     leaves out"
                        .to_owned(),
                );
            }
            source
                .builders
                .retain(|builder| *builder == Builder::Github);
        }
        // A free Build runs only where its image's receipt is made: on a Server, whatever
        // the Build Order says, as an upload's does.
        if free.contains(&source.service) {
            source.builders = vec![Builder::Servers];
        }
        sources.push(source);
    }
    Ok(sources)
}

/// The Git Services `stored` builds, before any Builder is chosen.
pub(super) fn unrouted(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<GitSource>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    Ok(git_sources(tx, stored, &saved)?
        .into_iter()
        .map(|(_, source)| source)
        .collect())
}

/// Each Git Service of `saved` that `stored` builds, by Service ID.
fn git_sources(
    tx: &mut dyn Tx,
    stored: &Stored,
    saved: &ployz_core::config::SavedEnvironmentIntent,
) -> Result<Vec<(String, GitSource)>, RpcError> {
    let rows = rows(tx, &stored.summary.id)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id() == service.id))
        .filter_map(|service| {
            let row = rows
                .iter()
                .find(|row| row.service == service.config.private_dns);
            Some((service.id.clone(), source_of(service, row)?))
        })
        .collect())
}

fn source_of(
    service: &ployz_core::config::SavedServiceIntent,
    row: Option<&BuildRow>,
) -> Option<GitSource> {
    let config = &service.config;
    let ServiceSource::Git {
        repository,
        repository_id,
        access,
        root_dir,
        branch,
        ..
    } = &config.source
    else {
        return None;
    };
    Some(GitSource {
        service: config.private_dns.clone(),
        repository: RepositoryName::parse(repository.as_str()).ok()?,
        repository_id: RepositoryId::parse(*repository_id).ok()?,
        access: access.clone(),
        branch: crate::git::branch_name(branch).and_then(|name| BranchName::parse(name).ok()),
        root_dir: root_dir.clone(),
        dockerfile_path: (config.build.build_method == BuildMethod::Dockerfile).then(|| {
            config
                .build
                .dockerfile_path
                .clone()
                .unwrap_or_else(|| ployz_core::config::DEFAULT_DOCKERFILE_PATH.into())
        }),
        commit: row.and_then(|row| row.commit.clone()),
        builders: Vec::new(),
        preferred_machine: None,
        status: row.map(|row| row.stage.status().into()),
        message: row.and_then(|row| row.message.clone()),
    })
}
