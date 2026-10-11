//! What a pull request's page and checks show.

use std::fmt::Write as _;

use serde_json::Value;

use super::*;

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &PullRequestQuery,
) -> Result<PullRequestView, RpcError> {
    let pull_request = facts(tx, who, query.repository_id, query.number)?;
    let mut environments = Vec::new();
    for (id, _) in current(tx, who, query.repository_id, query.number)? {
        let environment = scope::load_by_id(tx, &id)?;
        let (_, landings) = on_merge::landings(tx, who, &environment)?;
        environments.push(PrEnvironment {
            deployment: deployment::history(tx, &id, 1)?.into_iter().next(),
            environment: environment.summary,
            destinations: landings
                .into_iter()
                .map(|landing| OnMergeInto {
                    name: landing.into.summary.name,
                    rows: landing.rows,
                })
                .collect(),
        });
    }
    let check = pull_request
        .as_ref()
        .and_then(|pull_request| check(pull_request, &environments));
    Ok(PullRequestView {
        pull_request,
        environments,
        check,
    })
}

/// What the pull request's GitHub check says, if it says anything: while it is open,
/// what its merge lands; once merged, only what didn't land.
pub(super) fn check(
    pull_request: &PullRequest,
    environments: &[PrEnvironment],
) -> Option<OnMergeCheck> {
    let merged = pull_request.merge_commit.is_some();
    if environments.is_empty() || !(pull_request.open || merged) {
        return None;
    }
    let sections: Vec<(&PrEnvironment, &OnMergeInto)> = environments
        .iter()
        .flat_map(|environment| {
            environment
                .destinations
                .iter()
                .map(move |into| (environment, into))
        })
        .collect();
    let riding: Vec<&MergeRow> = sections
        .iter()
        .flat_map(|(_, into)| &into.rows)
        .filter(|row| row.ride.rides())
        .collect();
    let mut not_set: Vec<String> = Vec::new();
    for row in &riding {
        if row.lands == json!({ "secret": false }) && !not_set.contains(&key(row)) {
            not_set.push(key(row));
        }
    }
    let neutral = !not_set.is_empty() || riding.iter().any(|row| row.conflict);
    let mut summary = String::new();
    let title = if merged {
        let stuck: BTreeSet<&str> = sections
            .iter()
            .filter(|(_, into)| into.rows.iter().any(|row| row.ride.rides()))
            .map(|(_, into)| into.name.as_str())
            .collect();
        if stuck.is_empty() {
            return None;
        }
        let stuck = stuck.into_iter().collect::<Vec<_>>().join(", ");
        let _ = writeln!(
            summary,
            "Ployz couldn't land these when #{} merged. They stay in the preview; sync it into {stuck} to take them.\n",
            pull_request.number
        );
        format!("Didn't land in {stuck}")
    } else if riding.is_empty() {
        "No config changes".to_owned()
    } else {
        let mut title = config_changes(riding.len());
        if !not_set.is_empty() {
            let _ = write!(title, " · {} not set", not_set.join(", "));
        }
        title
    };
    let headed = sections.len() > 1;
    for (environment, into) in &sections {
        if headed {
            let _ = writeln!(
                summary,
                "## {} · {} → {}\n",
                environment.environment.project, environment.environment.name, into.name
            );
        }
        section(&mut summary, &environment.environment.name, into);
    }
    if sections.iter().all(|(_, into)| into.rows.is_empty()) {
        summary.push_str("No config changes.\n");
    }
    Some(OnMergeCheck {
        conclusion: if merged || neutral {
            OnMergeConclusion::Neutral
        } else {
            OnMergeConclusion::Success
        },
        title,
        summary: summary.trim_end().to_owned(),
    })
}

/// One Destination's rows: what lands, what lands over the Destination's own change,
/// and what stays in the preview.
fn section(summary: &mut String, preview: &EnvironmentName, into: &OnMergeInto) {
    let rides = |conflict: bool| {
        into.rows
            .iter()
            .filter(move |row| row.ride.rides() && row.conflict == conflict)
    };
    list(summary, rides(false), lands);
    if rides(true).next().is_some() {
        let _ = writeln!(summary, "### Changed in {}\n", into.name);
        list(summary, rides(true), lands);
        summary.push_str("The merge sets these. Undo in Ployz History reverts it.\n\n");
    }
    let staying = || into.rows.iter().filter(|row| row.ride == Ride::Excluded);
    if staying().next().is_some() {
        let _ = writeln!(summary, "### Stays in {preview}\n");
        list(summary, staying(), |row| {
            format!("{} → {}", from(&row.production), to(&row.preview))
        });
    }
}

fn list<'a>(
    summary: &mut String,
    rows: impl Iterator<Item = &'a MergeRow>,
    line: fn(&MergeRow) -> String,
) {
    let mut any = false;
    for row in rows {
        let _ = writeln!(summary, "- `{}`: {}", label(row), line(row));
        any = true;
    }
    if any {
        summary.push('\n');
    }
}

/// What a riding row lands, over the Destination's value.
fn lands(row: &MergeRow) -> String {
    if is_secret(&row.lands) || is_secret(&row.production) {
        return shown(&row.lands);
    }
    let over = if row.conflict { "production " } else { "" };
    format!("{over}{} → {}", from(&row.production), to(&row.lands))
}

fn from(value: &Value) -> String {
    if value.is_null() {
        "new".to_owned()
    } else {
        shown(value)
    }
}

fn to(value: &Value) -> String {
    if value.is_null() {
        "removed".to_owned()
    } else {
        shown(value)
    }
}

/// A value as the check shows it: a secret only as set or not set.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(object) => match object.get("secret") {
            Some(Value::Bool(true)) => "set".to_owned(),
            Some(_) => "not set".to_owned(),
            None => value.to_string(),
        },
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => value.to_string(),
    }
}

fn is_secret(value: &Value) -> bool {
    value.get("secret").is_some()
}

/// The row's setting within its node: `A` for `env.A`.
fn key(row: &MergeRow) -> String {
    row.at.name.as_deref().map_or_else(
        || row.at.node.to_string(),
        |name| name.strip_prefix("env.").unwrap_or(name).to_owned(),
    )
}

fn label(row: &MergeRow) -> String {
    match &row.at.name {
        Some(_) => format!("{} · {}", row.at.node, key(row)),
        None => row.at.node.to_string(),
    }
}

fn config_changes(n: usize) -> String {
    format!("{n} config {}", if n == 1 { "change" } else { "changes" })
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
