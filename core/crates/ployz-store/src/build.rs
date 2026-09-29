//! Git and uploaded builds. Before a Deployment's runner reads a Git Service's source,
//! Cloud pins the commit it builds. A pin never moves, so every retry of the Deployment
//! builds the same commit. Services without a source of their own build from the
//! Deployment's upload. The runner reports each build's progress and log here.

use std::collections::BTreeMap;

use ployz_core::config::{BuildMethod, ServiceGitAccess, ServiceGitBranch, ServiceSource};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{self, Stored};
use crate::error;
use crate::id::DeploymentId;
use crate::storage::Tx;

/// The most log a build keeps; older output goes first.
const LOG_LIMIT: usize = 256 * 1024;

/// A Git Service a Deployment builds, and the commit it is pinned to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GitSource {
    /// Its runtime Service name.
    pub service: ServiceName,
    /// The GitHub repository, as `owner/name`.
    pub repository: String,
    /// GitHub's ID for it.
    #[ts(type = "number")]
    pub repository_id: u64,
    /// How Cloud reads it.
    pub access: ServiceGitAccess,
    /// The branch it follows; none once it was disconnected.
    pub branch: Option<String>,
    /// The directory it builds from, inside the repository.
    pub root_dir: String,
    /// Its Dockerfile, relative to `root_dir`, when it builds from one.
    pub dockerfile_path: Option<String>,
    /// The commit it builds; none until pinned.
    pub commit: Option<String>,
}

/// Where one Git build is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// Pinned; not started.
    Pending,
    Building,
    Built,
    /// A matching image from an earlier build served it.
    Reused,
    Failed,
}

impl BuildStatus {
    const ALL: [Self; 5] = [
        Self::Pending,
        Self::Building,
        Self::Built,
        Self::Reused,
        Self::Failed,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Building => "building",
            Self::Built => "built",
            Self::Reused => "reused",
            Self::Failed => "failed",
        }
    }
}

/// A runner's report on one build: where it is now, and its log output since the
/// last report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildReport {
    /// The runtime Service it builds.
    pub service: ServiceName,
    pub status: BuildStatus,
    /// Why it failed. Users read it: it holds no secret.
    pub message: Option<String>,
    pub log: String,
}

/// One build of a Deployment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildView {
    /// The Service's name when admitted.
    pub service: String,
    /// The commit it builds; none when it builds from the Deployment's upload.
    pub commit: Option<String>,
    pub status: BuildStatus,
    /// Why it failed.
    pub message: Option<String>,
}

/// One build's log.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildLogQuery {
    pub deployment: DeploymentId,
    /// The Service it built.
    pub service: ServiceName,
}

/// A build with its log, newest output last.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildLogView {
    pub deployment: DeploymentId,
    #[serde(flatten)]
    pub build: BuildView,
    pub log: String,
}

/// The Git Services Deployment `id` builds, each with its pin, if any.
pub(crate) fn sources(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<GitSource>, RpcError> {
    let stored = deployment::load(tx, id)?;
    sources_of(tx, &stored)
}

/// Pin the commits of `commits` (by runtime Service name) that aren't pinned yet,
/// and return every source with its pin. A pinned commit never changes.
pub(crate) fn pin(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    commits: &BTreeMap<ServiceName, String>,
) -> Result<Vec<GitSource>, RpcError> {
    let stored = deployment::locked(tx, id)?;
    if !stored.summary.status.in_flight() {
        return Err(error::conflict(
            "This Deployment is no longer waiting to run",
            json!({ "deployment": id, "status": stored.summary.status }),
        ));
    }
    let sources = sources_of(tx, &stored)?;
    for (service, commit) in commits {
        if !sources.iter().any(|source| source.service == *service) {
            return Err(error::invalid(
                format!("This Deployment builds no Git Service {service}"),
                json!({ "services": sources.iter().map(|source| &source.service).collect::<Vec<_>>() }),
            ));
        }
        if !ployz_core::is_lower_hex(commit, 40) {
            return Err(error::invalid(
                "A pin is a full lowercase Git commit",
                json!({ "service": service }),
            ));
        }
        tx.execute(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log) \
             SELECT id, ?2, organization_id, ?3, 'pending', '', '' \
             FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, service) DO NOTHING",
            &[
                id.as_str().into(),
                service.as_str().into(),
                commit.as_str().into(),
            ],
        )?;
    }
    sources_of(tx, &stored)
}

/// The pinned commit of each Git Service Deployment `id` builds.
pub(crate) fn pins(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<BTreeMap<ServiceName, String>, RpcError> {
    rows(tx, id)?
        .into_iter()
        .filter(|row| !row.commit.is_empty())
        .map(|row| Ok((row.service, row.commit)))
        .collect()
}

/// Record a runner's report on one pinned or uploaded build of `stored`, which it
/// claimed.
pub(crate) fn record(
    tx: &mut dyn Tx,
    stored: &Stored,
    report: &BuildReport,
) -> Result<(), RpcError> {
    let id = &stored.summary.id;
    if uploads_of(tx, stored)?.contains(&report.service) {
        tx.execute(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log) \
             SELECT id, ?2, organization_id, '', 'pending', '', '' \
             FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, service) DO NOTHING",
            &[id.as_str().into(), report.service.as_str().into()],
        )?;
    }
    let Some(row) = rows(tx, id)?
        .into_iter()
        .find(|row| row.service == report.service)
    else {
        return Err(error::invalid(
            format!("{} has no pinned build to report on", report.service),
            json!({}),
        ));
    };
    let mut log = row.log;
    log.push_str(&report.log);
    if log.len() > LOG_LIMIT {
        let cut = log.len() - LOG_LIMIT;
        let cut = (cut..log.len())
            .find(|index| log.is_char_boundary(*index))
            .unwrap_or(log.len());
        log.drain(..cut);
    }
    let message: String = report
        .message
        .as_deref()
        .unwrap_or_default()
        .chars()
        .take(500)
        .collect();
    tx.execute(
        "UPDATE config_build SET status = ?3, message = ?4, log = ?5 \
         WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.as_str().into(),
            report.service.as_str().into(),
            report.status.as_str().into(),
            message.as_str().into(),
            log.as_str().into(),
        ],
    )?;
    Ok(())
}

/// Every build of `stored`, by the Service names it was admitted with.
pub(crate) fn views(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<BuildView>, RpcError> {
    Ok(rows(tx, &stored.summary.id)?
        .into_iter()
        .map(|row| row.view(stored))
        .collect())
}

/// One build's log; `service` is its Service's name or runtime name.
pub(crate) fn log(
    tx: &mut dyn Tx,
    stored: &Stored,
    service: &ServiceName,
) -> Result<BuildLogView, RpcError> {
    let row = rows(tx, &stored.summary.id)?
        .into_iter()
        .find(|row| row.service == *service || row.view(stored).service == service.as_str())
        .ok_or_else(|| {
            error::not_found(
                format!(
                    "Deployment {} built no Service {service}",
                    stored.summary.id
                ),
                json!({}),
            )
        })?;
    Ok(BuildLogView {
        deployment: stored.summary.id.clone(),
        build: row.view(stored),
        log: row.log,
    })
}

struct BuildRow {
    service: ServiceName,
    commit: String,
    status: BuildStatus,
    message: String,
    log: String,
}

impl BuildRow {
    fn view(&self, stored: &Stored) -> BuildView {
        BuildView {
            service: stored
                .nodes
                .iter()
                .find(|node| node.service.as_ref() == Some(&self.service))
                .map_or_else(|| self.service.to_string(), |node| node.name.clone()),
            commit: (!self.commit.is_empty()).then(|| self.commit.clone()),
            status: self.status,
            message: (!self.message.is_empty()).then(|| self.message.clone()),
        }
    }
}

fn rows(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<BuildRow>, RpcError> {
    tx.query(
        "SELECT service, commit_sha, status, message, log FROM config_build \
         WHERE deployment_id = ?1 ORDER BY service",
        &[id.as_str().into()],
    )?
    .iter()
    .map(|row| {
        let status = row.text(2)?;
        Ok(BuildRow {
            service: ServiceName::parse(row.text(0)?).map_err(|_| error::corrupt("build"))?,
            commit: row.text(1)?.to_owned(),
            status: BuildStatus::ALL
                .into_iter()
                .find(|known| known.as_str() == status)
                .ok_or_else(|| error::corrupt("build status"))?,
            message: row.text(3)?.to_owned(),
            log: row.text(4)?.to_owned(),
        })
    })
    .collect()
}

/// The Services `stored` targets that have no source of their own: they build from
/// its upload.
pub(crate) fn uploads_of(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<ServiceName>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.environment, stored.summary.saved)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id == service.id))
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
        .map(|service| service.config.private_dns.clone())
        .collect())
}

pub(crate) fn sources_of(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<GitSource>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.environment, stored.summary.saved)?;
    let pins = pins(tx, &stored.summary.id)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id == service.id))
        .filter_map(|service| {
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
                repository: repository.clone(),
                repository_id: *repository_id,
                access: access.clone(),
                branch: match branch {
                    ServiceGitBranch::Connected { name } => Some(name.clone()),
                    ServiceGitBranch::Disconnected { .. } => None,
                },
                root_dir: root_dir.clone(),
                dockerfile_path: (config.build.build_method == BuildMethod::Dockerfile).then(
                    || {
                        config
                            .build
                            .dockerfile_path
                            .clone()
                            .unwrap_or_else(|| "Dockerfile".into())
                    },
                ),
                commit: pins.get(&config.private_dns).cloned(),
            })
        })
        .collect())
}
