//! PR plans: which Environment a repository's pull requests start from, and how.

use super::*;

/// A repository no plan was saved for: off, removed on close.
pub(super) fn off() -> Stored {
    Stored {
        remove_on_close: true,
        ..Stored::default()
    }
}

pub(crate) fn set_plan(
    tx: &mut dyn Tx,
    who: &Actor,
    set: &SetPrPlan,
) -> Result<PrPlansView, RpcError> {
    let project = scope::project(tx, who, set.project.as_ref())?;
    // Serialize plan writes per Project.
    tx.execute(
        "UPDATE config_project SET name = name WHERE id = ?1",
        &[project.id.as_str().into()],
    )?;
    let repositories = repositories(tx, &project.id)?;
    let Some(Repository {
        id: repository_id, ..
    }) = repositories
        .iter()
        .find(|repository| repository.name == set.repository)
    else {
        let names = repositories
            .iter()
            .map(|repository| repository.name.as_str());
        return Err(error::choices(
            format!(
                "No Service of Project {} deploys from {} through the GitHub App",
                project.name, set.repository
            ),
            set.repository.as_str(),
            names,
        ));
    };
    let mut plan = load(tx, &project.id, *repository_id)?.unwrap_or_else(off);
    if let Some(enabled) = set.enabled {
        plan.enabled = enabled;
    }
    if let Some(remove) = set.remove_on_close {
        plan.remove_on_close = remove;
    }
    if let Some(bots) = set.include_bots {
        plan.include_bots = bots;
    }
    if let Some(name) = &set.start_from {
        let environment = scope::environment(
            tx,
            who,
            &EnvironmentRef {
                project: Some(project.name.clone()),
                environment: Some(name.clone()),
            },
        )?;
        if pr_environment(tx, &environment.summary.id)? {
            return Err(error::invalid(
                format!("{name} is a PR Environment: start from one that isn't"),
                json!({ "start_from": name }),
            ));
        }
        plan.start_from = Some(environment.summary.id);
    }
    if set.copy.is_some() || set.setup.is_some() {
        let Some(start) = plan
            .start_from
            .as_ref()
            .map(|id| start_from(tx, id))
            .transpose()?
            .flatten()
        else {
            return Err(error::invalid(
                "Pick the Environment PR Environments start from first: copies and Setup Commands name its nodes",
                json!({ "next": format!("ployz env pr {} --from ENV --project {}", set.repository, project.name) }),
            ));
        };
        if let Some(copy) = &set.copy {
            plan.copy = copy
                .iter()
                .map(|name| branch::lineage_named(&start.working, name))
                .collect::<Result<_, _>>()?;
        }
        if let Some(setup) = &set.setup {
            plan.setup = setup
                .iter()
                .map(|setup| {
                    let service = start.service(&setup.service)?;
                    Ok(Setup {
                        lineage: service.lineage_id.clone(),
                        command: branch::setup_command(setup)?,
                    })
                })
                .collect::<Result<_, RpcError>>()?;
        }
    }
    let name = set.repository.as_str();
    tx.execute(
        "INSERT INTO config_pr_plan (project_id, repository_id, organization_id, repository, plan) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (project_id, repository_id) DO UPDATE SET repository = excluded.repository, plan = excluded.plan",
        &[
            project.id.as_str().into(),
            (*repository_id).into(),
            who.organization.as_str().into(),
            name.into(),
            serde_json::to_string(&plan).expect("a plan is JSON").as_str().into(),
        ],
    )?;
    plans(
        tx,
        who,
        &PrPlansQuery {
            project: Some(project.name),
        },
    )
}

pub(crate) fn plans(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &PrPlansQuery,
) -> Result<PrPlansView, RpcError> {
    let project = scope::project(tx, who, query.project.as_ref())?;
    let mut plans = Vec::new();
    for Repository {
        id: repository_id,
        name,
        installation_id,
    } in repositories(tx, &project.id)?
    {
        let stored = load(tx, &project.id, repository_id)?.unwrap_or_else(off);
        let start = match &stored.start_from {
            Some(id) => start_from(tx, id)?,
            None => None,
        };
        let named = |lineage: &str| {
            start
                .as_ref()
                .and_then(|start| branch::name_of(&start.working, lineage))
        };
        let open = open_in(tx, who, &project.id, repository_id)?;
        plans.push(PrPlan {
            repository: name,
            repository_id,
            installation_id,
            enabled: stored.enabled,
            start_from: start.as_ref().map(|start| start.summary.name.clone()),
            copy: stored
                .copy
                .iter()
                .filter_map(|lineage| {
                    start
                        .as_ref()
                        .and_then(|start| branch::node_of(&start.working, lineage))
                })
                .collect(),
            setup: stored
                .setup
                .iter()
                .filter_map(|setup| {
                    Some(SetupCommand {
                        service: ServiceName::parse(named(&setup.lineage)?).ok()?,
                        command: setup.command.clone(),
                    })
                })
                .collect(),
            remove_on_close: stored.remove_on_close,
            include_bots: stored.include_bots,
            open,
        });
    }
    Ok(PrPlansView {
        project: ProjectSummary {
            id: project.id,
            name: project.name,
        },
        plans,
    })
}

/// Every repository a Service of the Project deploys from through the GitHub App,
/// by ID, with its name and installation, sorted by name.
pub(super) fn repositories(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
) -> Result<Vec<Repository>, RpcError> {
    let rows = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 ORDER BY id",
        &[project.as_str().into()],
    )?;
    let mut found = std::collections::BTreeMap::new();
    for row in rows {
        let id = row.parse::<EnvironmentId>(0, "Environment ID")?;
        for service in scope::load_by_id(tx, &id)?.working.services {
            if let ServiceSource::Git {
                repository,
                repository_id,
                access: ServiceGitAccess::GithubInstallation { installation_id },
                ..
            } = service.config.source
            {
                let (Ok(id), Ok(name)) = (
                    RepositoryId::parse(repository_id),
                    RepositoryName::parse(repository),
                ) else {
                    return Err(error::corrupt("Git Service"));
                };
                found.entry(id).or_insert(Repository {
                    id,
                    name,
                    installation_id,
                });
            }
        }
    }
    let mut found: Vec<Repository> = found.into_values().collect();
    found.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(found)
}

/// A repository a Project's Services deploy from through the GitHub App.
pub(super) struct Repository {
    id: RepositoryId,
    name: RepositoryName,
    installation_id: u64,
}
