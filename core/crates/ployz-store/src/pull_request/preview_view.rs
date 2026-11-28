//! What a repository's previews show: each pull request's preview, how many are live
//! against the cap, and the settings its preview rule signed.

use ployz_core::MachineId;
use ployz_core::authority::{SignerKey, Timestamp};

use super::preview::{Shown, live, shown};
use super::*;

/// A repository's previews in a Project, and one pull request's even once it closed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PreviewsQuery {
    /// The Project; omitted means the Organization's only Project.
    #[serde(default)]
    pub project: Option<ProjectName>,
    pub repository: RepositoryName,
    /// A pull request listed whatever its state.
    #[serde(default)]
    pub number: Option<PullRequestNumber>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewsView {
    pub project: ProjectSummary,
    pub repository: RepositoryName,
    pub repository_id: RepositoryId,
    /// None while previews are off, or were never set up.
    pub settings: Option<PreviewSettings>,
    /// Previews deploying or live, across every Server of the plan.
    pub live: u32,
    pub limit: u32,
    /// The open pull requests, those with a preview and the asked one, newest change first.
    pub pull_requests: Vec<PreviewRow>,
}

/// What the preview rule a person signed says.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewSettings {
    pub start_from: EnvironmentName,
    /// The Servers previews may run on.
    pub servers: Vec<MachineId>,
    pub limit: u32,
    #[ts(type = "number")]
    pub generation: u64,
    pub signer: SignerKey,
    pub issued_at: Timestamp,
    pub rule: Signed<AutoDeployRule>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewRow {
    pub number: PullRequestNumber,
    pub title: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub author_id: Option<GithubUserId>,
    /// Whether its author is a member, as Cloud last read the members.
    pub member: bool,
    /// Whether it merges from another repository than its own.
    pub fork: bool,
    pub head: Option<CommitSha>,
    pub updated: crate::GithubTimestamp,
    pub open: bool,
    pub preview: PreviewOutcome,
}

/// A pull request's preview: live, deploying, or why it isn't deployed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PreviewOutcome {
    NotDeployed {
        /// None when nothing stands in the way of a deploy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        why: Option<NotDeployed>,
    },
    Deploying {
        environment: EnvironmentName,
        deployment: crate::DeploymentId,
        head: CommitSha,
    },
    Live {
        environment: EnvironmentName,
        /// Its generated address, once a Deployment that served one applied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        address: Option<String>,
        /// The head it runs.
        head: CommitSha,
        /// The Server it runs on, when it is one the rule or Cloud names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        server: Option<MachineId>,
        /// The pull request's head, when that isn't the one it runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        newer: Option<CommitSha>,
    },
    Closed,
}

/// Why a pull request has no preview.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NotDeployed {
    /// Every place under the cap is taken.
    Full {
        live: u32,
        limit: u32,
    },
    /// It changes `service`'s settings, which the preview rule doesn't cover.
    NotCovered {
        service: ServiceName,
    },
    Off,
    MergesIntoStartFrom,
    /// No Server can hold a preview.
    NoServer,
    /// It isn't a member's pull request from the repository itself.
    NotMember {
        author: String,
    },
    /// Its last deploy failed: why, for a person to read.
    Failed {
        message: String,
    },
}

/// A pull request's preview in one Project, as its GitHub comment links it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PreviewLink {
    pub project: ProjectName,
    pub repository: RepositoryName,
    pub outcome: PreviewOutcome,
}

/// A plan's previews as they stand.
struct Board {
    project: crate::id::ProjectId,
    repository_id: RepositoryId,
    start_from: Option<EnvironmentId>,
    settings: Option<PreviewSettings>,
    /// The Service whose settings the rule doesn't cover, as a preview opening now would
    /// deploy them.
    not_covered: Option<ServiceName>,
    limit: u32,
    /// Each open preview's pull request, Environment, Namespace and what it shows.
    previews: Vec<(PullRequestNumber, EnvironmentId, PreviewNamespace, Shown)>,
}

impl Board {
    fn of(
        tx: &mut dyn Tx,
        who: &Actor,
        (project, repository_id): (&crate::id::ProjectId, RepositoryId),
        plan: &Stored,
    ) -> Result<Self, RpcError> {
        let limit = plan
            .preview
            .as_ref()
            .map_or(preview::DEFAULT_CAP, |preview| preview.cap);
        let mut previews = Vec::new();
        for open in live(tx, who)? {
            if open.project != *project || open.repository_id != repository_id {
                continue;
            }
            if let Some(pr) = crate::pr_environment::of(tx, &open.environment)? {
                let shown = shown(tx, &open.environment)?;
                previews.push((pr.number, open.environment, open.preview, shown));
            }
        }
        Ok(Self {
            project: project.clone(),
            repository_id,
            start_from: plan.start_from.clone(),
            settings: settings(tx, plan)?,
            not_covered: preview::not_covered(tx, who, plan)?,
            limit: u32::from(limit.get()),
            previews,
        })
    }

    fn live(&self) -> u32 {
        let holding = self
            .previews
            .iter()
            .filter(|(.., shown)| !matches!(shown, Shown::Failed(_)))
            .count();
        u32::try_from(holding).unwrap_or(u32::MAX)
    }

    /// What `record`'s pull request shows as its preview, by `members` when Cloud read
    /// them for this, else by its standing when it was last reported, and `homes`, the
    /// Servers Cloud read can hold one.
    fn outcome(
        &self,
        tx: &mut dyn Tx,
        record: &Record,
        (members, homes): (Option<&[GithubUserId]>, Option<&[MachineId]>),
    ) -> Result<PreviewOutcome, RpcError> {
        let facts = record.facts();
        let open = matches!(record.phase, Phase::Open { .. });
        if let Some((_, environment, preview, shown)) = self
            .previews
            .iter()
            .find(|(number, ..)| *number == facts.number)
        {
            let name = environment_name(tx, environment)?;
            return Ok(match shown {
                Shown::Deploying(deployment, head) => PreviewOutcome::Deploying {
                    environment: name,
                    deployment: deployment.clone(),
                    head: head.clone().unwrap_or(facts.head),
                },
                Shown::Live => {
                    let head = live_head(tx, environment)?.unwrap_or_else(|| facts.head.clone());
                    let servers = self.settings.iter().flat_map(|s| &s.servers);
                    PreviewOutcome::Live {
                        environment: name,
                        address: address(tx, environment, self.repository_id)?,
                        server: servers
                            .chain(homes.into_iter().flatten())
                            .find(|server| preview.homed_on(server))
                            .cloned(),
                        newer: (open && facts.head != head).then_some(facts.head),
                        head,
                    }
                }
                Shown::Failed(message) => PreviewOutcome::NotDeployed {
                    why: Some(match &self.not_covered {
                        Some(service) => NotDeployed::NotCovered {
                            service: service.clone(),
                        },
                        None => NotDeployed::Failed {
                            message: message.clone(),
                        },
                    }),
                },
            });
        }
        if !open {
            return Ok(PreviewOutcome::Closed);
        }
        let merges_into_start = match &self.start_from {
            Some(start) => {
                destinations_of(tx, &self.project, self.repository_id, &facts.target_branch)?
                    .contains(start)
            }
            None => false,
        };
        let deployable = match members {
            Some(members) => trusts(&facts, Some(members)),
            None => record.phase.open_standing() == OpenStanding::Trusted,
        };
        let why = if self.settings.is_none() {
            Some(NotDeployed::Off)
        } else if merges_into_start {
            Some(NotDeployed::MergesIntoStartFrom)
        } else if !deployable {
            Some(NotDeployed::NotMember {
                author: facts.author,
            })
        } else if let Some(service) = &self.not_covered {
            Some(NotDeployed::NotCovered {
                service: service.clone(),
            })
        } else if homes.is_some_and(<[MachineId]>::is_empty) {
            Some(NotDeployed::NoServer)
        } else if self.live() >= self.limit {
            Some(NotDeployed::Full {
                live: self.live(),
                limit: self.limit,
            })
        } else {
            None
        };
        Ok(PreviewOutcome::NotDeployed { why })
    }
}

/// What `plan`'s preview rule says, while it is on and its start-from stands.
fn settings(tx: &mut dyn Tx, plan: &Stored) -> Result<Option<PreviewSettings>, RpcError> {
    let Some(stored) = &plan.preview else {
        return Ok(None);
    };
    let Ok(terms) = stored.rule.open() else {
        return Ok(None);
    };
    let start = match &plan.start_from {
        Some(id) => start_from(tx, id)?,
        None => None,
    };
    let Some(start) = start.filter(|_| !terms.ended) else {
        return Ok(None);
    };
    Ok(Some(PreviewSettings {
        start_from: start.summary.name,
        servers: terms.machines,
        limit: u32::from(stored.cap.get()),
        generation: terms.generation,
        signer: terms.signer,
        issued_at: terms.issued_at,
        rule: stored.rule.clone(),
    }))
}

fn environment_name(tx: &mut dyn Tx, id: &EnvironmentId) -> Result<EnvironmentName, RpcError> {
    let rows = tx.query(
        "SELECT name FROM config_environment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first()
        .ok_or_else(|| error::corrupt("preview"))?
        .parse(0, "Environment name")
}

/// The head preview `environment` last deployed from a push, when a Deployment of one
/// applied among its latest.
fn live_head(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Option<CommitSha>, RpcError> {
    Ok(deployment::history(tx, environment, 20)?
        .into_iter()
        .filter(|deployment| deployment.status == deployment::DeploymentStatus::Applied)
        .find_map(|deployment| deployment.push)
        .map(|push| push.head))
}

/// The https address of preview `environment`: the first generated domain its Applied
/// State serves, of a Service that builds from the repository first, under the Cluster
/// Domain its Deployment expanded it in.
fn address(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    repository_id: RepositoryId,
) -> Result<Option<String>, RpcError> {
    let loaded = scope::load_by_id(tx, environment)?;
    let head = deployment::head(tx, &loaded)?;
    let clusters = deployment::cluster_domains(tx, environment, &head)?;
    let mut services: Vec<_> = head.applied.services.iter().collect();
    services.sort_by_key(|service| !service.config.source.builds_repository(repository_id.get()));
    Ok(services.into_iter().find_map(|service| {
        service.config.managed_hostnames.iter().find_map(|managed| {
            let cluster = clusters.get(&(service.id.clone(), managed.prefix.clone()))?;
            let hostname = crate::domain::generated_hostname(&managed.prefix, cluster);
            Some(format!("https://{hostname}"))
        })
    }))
}

pub(crate) fn previews(
    tx: &mut dyn Tx,
    who: &Actor,
    trusted: &Trusted,
    query: &PreviewsQuery,
) -> Result<PreviewsView, RpcError> {
    let project = scope::project(tx, who, query.project.as_ref())?;
    let repositories = repositories(tx, &project.id)?;
    let Some(repository) = repositories
        .iter()
        .find(|repository| repository.name == query.repository)
    else {
        let names = repositories
            .iter()
            .map(|repository| repository.name.as_str());
        return Err(error::choices(
            format!(
                "No Service of Project {} deploys from {} through the GitHub App",
                project.name, query.repository
            ),
            query.repository.as_str(),
            names,
        ));
    };
    let repository_id = repository.id;
    let plan = load(tx, &project.id, repository_id)?.unwrap_or_else(off);
    let board = Board::of(tx, who, (&project.id, repository_id), &plan)?;
    let rows = tx.query(
        &format!(
            "SELECT number, {RECORD} FROM config_pull_request \
             WHERE organization_id = ?1 AND repository_id = ?2 AND updated <> ''"
        ),
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    let members = trusted.members.as_deref();
    let mut pull_requests = Vec::new();
    for row in rows {
        let number: PullRequestNumber = row.number(0, "pull request")?;
        let record = Record::read(&row, 1)?;
        let open = matches!(record.phase, Phase::Open { .. });
        let previewing = board.previews.iter().any(|(has, ..)| *has == number);
        if !open && !previewing && query.number != Some(number) {
            continue;
        }
        let preview = board.outcome(tx, &record, (members, trusted.preview_homes.as_deref()))?;
        let facts = record.facts();
        let member = match members {
            Some(members) => facts
                .author_id
                .is_some_and(|author| members.contains(&author)),
            None => record.phase.open_standing() == OpenStanding::Trusted && !facts.bot,
        };
        pull_requests.push(PreviewRow {
            number,
            fork: facts.head_repository_id != Some(repository_id),
            member,
            title: facts.title,
            author: facts.author,
            author_id: facts.author_id,
            head: Some(facts.head),
            updated: facts.updated,
            open,
            preview,
        });
    }
    pull_requests
        .sort_by(|a, b| (b.updated.as_str(), b.number).cmp(&(a.updated.as_str(), a.number)));
    Ok(PreviewsView {
        project: ProjectSummary {
            id: project.id,
            name: project.name,
        },
        repository: repository.name.clone(),
        repository_id,
        live: board.live(),
        limit: board.limit,
        settings: board.settings,
        pull_requests,
    })
}

/// `record`'s preview in each Project whose plan for its repository keeps a preview rule.
pub(super) fn links(
    tx: &mut dyn Tx,
    who: &Actor,
    record: &Record,
) -> Result<Vec<PreviewLink>, RpcError> {
    let repository_id = record.facts.repository_id;
    let rows = tx.query(
        "SELECT p.project_id, j.name, p.repository, p.plan FROM config_pr_plan p \
         JOIN config_project j ON j.id = p.project_id \
         WHERE p.organization_id = ?1 AND p.repository_id = ?2 ORDER BY j.name",
        &[who.organization.as_str().into(), repository_id.into()],
    )?;
    let mut links = Vec::new();
    for row in rows {
        let plan: Stored = row.json(3, "PR plan")?;
        if plan.preview.is_none() {
            continue;
        }
        let project = row.parse::<crate::id::ProjectId>(0, "Project ID")?;
        let board = Board::of(tx, who, (&project, repository_id), &plan)?;
        links.push(PreviewLink {
            project: row.parse(1, "Project name")?,
            repository: row.parse(2, "repository")?,
            outcome: board.outcome(tx, record, (None, None))?,
        });
    }
    Ok(links)
}
