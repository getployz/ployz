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
    for (id, _) in current(tx, who, query.repository_id, query.number, false)? {
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
    let reference = PullRequestRef {
        repository_id: query.repository_id,
        number: query.number,
    };
    let unlanded = on_merge::unlanded(tx, who, &reference)?.unwrap_or_default();
    let check = pull_request
        .as_ref()
        .and_then(|pull_request| check(pull_request, &environments, &unlanded));
    Ok(PullRequestView {
        pull_request,
        environments,
        check,
    })
}

/// What the pull request's GitHub check says, if it says anything: while it is open,
/// what its merge lands, or that it lands nowhere; once merged, only what the merge
/// recorded it didn't do.
pub(super) fn check(
    pull_request: &PullRequest,
    environments: &[PrEnvironment],
    unlanded: &[on_merge::Unlanded],
) -> Option<OnMergeCheck> {
    if pull_request.merge_commit.is_some() {
        return merged_check(pull_request, unlanded);
    }
    if environments.is_empty() || !pull_request.open {
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
    if sections.is_empty() {
        let target = &pull_request.target_branch;
        return Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: format!("Nothing deploys from {target}"),
            summary: format!(
                "No Environment deploys from `{target}`, so the merge lands nowhere. \
                 Changes stay in the preview."
            ),
            merged_into: None,
        });
    }
    let riding: Vec<&MergeRow> = sections
        .iter()
        .flat_map(|(_, into)| &into.rows)
        .filter(|row| lands_here(row))
        .collect();
    let mut not_set: Vec<String> = Vec::new();
    for row in &riding {
        if row.lands == json!({ "secret": false }) && !not_set.contains(&key(row)) {
            not_set.push(key(row));
        }
    }
    let neutral = !not_set.is_empty()
        || riding.iter().any(|row| row.conflict)
        || sections
            .iter()
            .any(|(_, into)| into.rows.iter().any(in_use));
    let mut summary = String::new();
    let title = if riding.is_empty() {
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
        conclusion: if neutral {
            OnMergeConclusion::Neutral
        } else {
            OnMergeConclusion::Success
        },
        title,
        summary: summary.trim_end().to_owned(),
        merged_into: None,
    })
}

/// A merged pull request's check, from what its merge recorded: silent when every
/// Destination took it and deployed.
fn merged_check(
    pull_request: &PullRequest,
    unlanded: &[on_merge::Unlanded],
) -> Option<OnMergeCheck> {
    let names = |saved: bool| {
        let names: BTreeSet<&str> = unlanded
            .iter()
            .filter(|unlanded| unlanded.saved == saved)
            .map(|unlanded| unlanded.into.as_str())
            .collect();
        names.into_iter().collect::<Vec<_>>().join(", ")
    };
    let (stuck, undeployed) = (names(false), names(true));
    let mut title = Vec::new();
    let mut summary = String::new();
    if !stuck.is_empty() {
        title.push(format!("Didn't land in {stuck}"));
        let _ = writeln!(
            summary,
            "Ployz couldn't land these when #{} merged. They stay in the preview; sync it into {stuck} to take them.\n",
            pull_request.number
        );
    }
    if !undeployed.is_empty() {
        title.push(format!("Saved in {undeployed}, not deployed"));
        let _ = writeln!(
            summary,
            "The merge saved its changes in {undeployed}, but Ployz couldn't deploy them.\n"
        );
    }
    if title.is_empty() {
        return None;
    }
    for unlanded in unlanded {
        let _ = writeln!(summary, "- {}: {}", unlanded.into, unlanded.reason);
    }
    Some(OnMergeCheck {
        conclusion: OnMergeConclusion::Neutral,
        title: title.join(" · "),
        summary: summary.trim_end().to_owned(),
        merged_into: unlanded.first().map(|unlanded| EnvironmentRef {
            project: Some(unlanded.project.clone()),
            environment: Some(unlanded.into.clone()),
        }),
    })
}

/// One Destination's rows: what lands, what lands over the Destination's own change,
/// and what stays in the preview.
fn section(summary: &mut String, preview: &EnvironmentName, into: &OnMergeInto) {
    let rides = |conflict: bool| {
        into.rows
            .iter()
            .filter(move |row| lands_here(row) && row.conflict == conflict)
    };
    list(summary, rides(false), lands);
    list(summary, into.rows.iter().filter(|row| in_use(row)), |row| {
        let names: Vec<String> = row.refused_by.iter().map(ToString::to_string).collect();
        format!("In use · {}", names.join(", "))
    });
    list(
        summary,
        into.rows.iter().filter(|row| row.ride == Ride::Kept),
        |_| "Kept".to_owned(),
    );
    if rides(true).next().is_some() {
        let _ = writeln!(summary, "### Changed in {}\n", into.name);
        list(summary, rides(true), lands);
        summary.push_str("The merge sets these. Undo in Ployz History reverts it.\n\n");
    }
    let staying = || into.rows.iter().filter(|row| row.ride == Ride::Excluded);
    if staying().next().is_some() {
        let _ = writeln!(summary, "### Stays in {preview}\n");
        list(summary, staying(), |row| match is_file(row) {
            _ if is_removal(row) => "Excluded".to_owned(),
            true => file_move(&row.production, &row.preview),
            false => format!("{} → {}", from(&row.production), to(&row.preview)),
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
        let _ = match is_removal(row) && lands_here(row) {
            true if row.conflict => writeln!(summary, "- Removes `{}` · Changed there", label(row)),
            true => writeln!(summary, "- Removes `{}`", label(row)),
            false => writeln!(summary, "- `{}`: {}", label(row), line(row)),
        };
        any = true;
    }
    if any {
        summary.push('\n');
    }
}

/// A riding row the merge lands; a removal still in use stays.
fn lands_here(row: &MergeRow) -> bool {
    row.ride.rides() && row.refused_by.is_empty()
}

fn in_use(row: &MergeRow) -> bool {
    row.ride.rides() && !row.refused_by.is_empty()
}

/// A node row with nothing in the preview; a new node's row has its record there.
fn is_removal(row: &MergeRow) -> bool {
    row.at.name.is_none() && row.preview.is_null()
}

/// What a riding row lands, over the Destination's value.
fn lands(row: &MergeRow) -> String {
    if is_file(row) {
        return file_move(&row.production, &row.lands);
    }
    if !row.lands.is_null() && (is_secret(&row.lands) || is_secret(&row.production)) {
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

/// A Config file's row: its contents read as a change, not a value.
fn is_file(row: &MergeRow) -> bool {
    row.at
        .name
        .as_deref()
        .is_some_and(|name| name.starts_with("files."))
}

fn file_move(before: &Value, after: &Value) -> String {
    match (before.is_null(), after.is_null()) {
        (true, _) => "new file".to_owned(),
        (_, true) => "removed".to_owned(),
        (false, false) => "changed".to_owned(),
    }
}

fn is_secret(value: &Value) -> bool {
    value.get("secret").is_some()
}

/// The row's setting within its node: `A` for `env.A`.
fn key(row: &MergeRow) -> String {
    row.at.name.as_deref().map_or_else(
        || row.at.node.to_string(),
        |name| {
            name.strip_prefix("env.")
                .or_else(|| name.strip_prefix("files."))
                .unwrap_or(name)
                .to_owned()
        },
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
