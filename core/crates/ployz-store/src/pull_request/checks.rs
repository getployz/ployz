//! What a pull request's page and checks show.

use super::*;

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &PullRequestQuery,
) -> Result<PullRequestView, RpcError> {
    let pull_request = facts(tx, who, query.repository_id, query.number)?;
    let target = pull_request
        .as_ref()
        .map(|facts| facts.target_branch.clone());
    let mut environments = Vec::new();
    for (id, project) in current(tx, who, query.repository_id, query.number)? {
        let environment = scope::load_by_id(tx, &id)?;
        let mut destinations = Vec::new();
        let into_all = match &target {
            Some(target) => destinations_of(tx, &project, query.repository_id, target)?,
            None => Vec::new(),
        };
        for into in into_all {
            let into = scope::load_by_id(tx, &into)?;
            destinations.push(Destination {
                changes: branch::changes_into(tx, &environment, &into)?,
                conditional_sync: crate::conditional_sync::standing_in(tx, &environment, &into)?,
                name: into.summary.name,
            });
        }
        environments.push(PrEnvironment {
            deployment: deployment::history(tx, &id, 1)?.into_iter().next(),
            environment: environment.summary,
            destinations,
        });
    }
    let (passing, reason) = check(
        &environments,
        target.as_ref().map_or("", BranchName::as_str),
    );
    Ok(PullRequestView {
        pull_request,
        environments,
        passing,
        reason,
    })
}

/// Whether the pull request is ready to merge, and why: every Destination with
/// changes has a standing Conditional Sync, and a value for each secret it brings.
pub(super) fn check(environments: &[PrEnvironment], target: &str) -> (bool, String) {
    let destinations: Vec<&Destination> = environments
        .iter()
        .flat_map(|environment| &environment.destinations)
        .collect();
    if destinations.is_empty() {
        return (true, format!("No environment deploys {target}"));
    }
    let waiting: Vec<&&Destination> = destinations
        .iter()
        .filter(|destination| {
            destination.changes > 0
                || destination
                    .conditional_sync
                    .as_ref()
                    .is_some_and(|sync| sync.standing)
        })
        .collect();
    if waiting.is_empty() {
        let names: BTreeSet<&str> = destinations
            .iter()
            .map(|destination| destination.name.as_str())
            .collect();
        return (
            true,
            format!(
                "No changes for {}",
                names.into_iter().collect::<Vec<_>>().join(", ")
            ),
        );
    }
    if waiting.iter().any(|destination| {
        destination
            .conditional_sync
            .as_ref()
            .is_some_and(|sync| !sync.standing)
    }) {
        return (false, "Changed since synced · sync again".into());
    }
    let unsaved: usize = waiting
        .iter()
        .filter(|destination| destination.conditional_sync.is_none())
        .map(|destination| destination.changes)
        .sum();
    if unsaved > 0 {
        return (false, format!("{} to sync in Ployz", changes(unsaved)));
    }
    for destination in &waiting {
        let secrets = destination
            .conditional_sync
            .as_ref()
            .map_or(&[][..], |sync| &sync.waiting);
        let of = match secrets {
            [] => continue,
            [one] => one.rsplit('.').next().unwrap_or(one).to_owned(),
            many => format!("{} secrets", many.len()),
        };
        return (
            false,
            format!("Waiting for {}'s value of {of}", destination.name),
        );
    }
    let saved: usize = waiting
        .iter()
        .filter_map(|destination| destination.conditional_sync.as_ref())
        .map(|sync| sync.changes)
        .sum();
    let verb = if saved == 1 { "goes" } else { "go" };
    (true, format!("{} {verb} live with this PR", changes(saved)))
}

pub(super) fn changes(n: usize) -> String {
    format!("{n} {}", if n == 1 { "change" } else { "changes" })
}

/// Where a merge into `target` lands: each Environment of the Project, other than
/// PR Environments, whose latest Saved State has a Service from the repository
/// tracking `target`, with none above it tracking it too.
pub(crate) fn destinations_of(
    tx: &mut dyn Tx,
    project: &crate::id::ProjectId,
    repository_id: RepositoryId,
    target: &BranchName,
) -> Result<Vec<EnvironmentId>, RpcError> {
    let rows = tx.query(
        "SELECT id FROM config_environment WHERE project_id = ?1 \
         AND id NOT IN (SELECT environment_id FROM config_pr_environment) ORDER BY id",
        &[project.as_str().into()],
    )?;
    let mut tracking = BTreeSet::new();
    for row in rows {
        let id: EnvironmentId = row.parse(0, "Environment ID")?;
        let tracks = review::latest_saved(tx, &id)?.is_some_and(|saved| {
            saved
                .intent
                .services
                .iter()
                .any(|service| crate::git::tracks(service, repository_id, target))
        });
        if tracks {
            tracking.insert(id);
        }
    }
    let mut destinations = Vec::new();
    for id in &tracking {
        let above = branch::ancestors(tx, id)?;
        if !above.iter().any(|ancestor| tracking.contains(ancestor)) {
            destinations.push(id.clone());
        }
    }
    Ok(destinations)
}
