//! A PR Environment's plan, and shutting one down.

use clap::ArgMatches;
use ployz_store::{
    ApprovePullRequest, DeploymentStatus, EnvironmentName, OpenPullRequest, PrPlansQuery,
    PullRequestNumber, Standing,
};
use serde_json::json;

use super::super::deploy;
use super::super::store::{self, named_environment, project, store};
use super::super::{Error, leaf_matches};
use super::branch::setups;
use crate::handlers::shared::accepted::{accepted, take_off, unfinished};

/// Take an Environment off the Servers and keep everything else of it.
pub(crate) fn shutdown(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (name, at) = named_environment(matches, "name")?;
    let store = store(root)?;
    let accept = accepted(matches)?;
    let again = store.again();
    let events = deploy::open_events(matches)?;
    let version = matches.get_one::<String>("expect-version").cloned();
    let (view, ran) = take_off(matches, &store, &at, (&accept, version), events)?;
    if view.deployment.status != DeploymentStatus::Applied {
        return unfinished(matches, &view, ran, &again);
    }
    let on = store::next(matches, &["deploy", "--env", name.as_str()]);
    deploy::finish_view(&view, Some(on))
}

pub(crate) fn pr(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let list = |flag: &str| -> Option<Vec<String>> {
        matches
            .get_many::<String>(flag)
            .map(|values| values.cloned().collect())
    };
    let enabled = match (matches.get_flag("on"), matches.get_flag("off")) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let start_from = matches
        .get_one::<String>("from")
        .map(|from| EnvironmentName::parse(from.as_str()))
        .transpose()?;
    let setup = list("setup").map(|values| setups(&values)).transpose()?;
    let repository = matches
        .get_one::<String>("repository")
        .map(|name| ployz_store::RepositoryName::parse(name.as_str()))
        .transpose()?;
    let project = project(matches)?;
    let copy = list("copy")
        .map(|names| super::node_names(&names))
        .transpose()?;
    let remove_on_close = matches.get_one::<bool>("remove-on-close").copied();
    let include_bots = matches.get_one::<bool>("bots").copied();
    let store = store(root)?;
    let query = PrPlansQuery {
        project: project.clone(),
    };
    let changing = enabled.is_some()
        || start_from.is_some()
        || copy.is_some()
        || setup.is_some()
        || remove_on_close.is_some()
        || include_bots.is_some();
    let view = match changing {
        false => store.read(&query)?,
        true => {
            let repository = match repository {
                Some(repository) => repository,
                None => {
                    let plans = store.read(&query)?;
                    match plans.plans.as_slice() {
                        [only] => only.repository.clone(),
                        _ => {
                            return Err(Error::usage(
                                "Name the repository: `ployz env pr` lists the Project's",
                            ));
                        }
                    }
                }
            };
            let set = ployz_store::SetPrPlan {
                project,
                repository,
                enabled,
                start_from,
                copy,
                setup,
                remove_on_close,
                include_bots,
                preview_rule: None,
                preview_cap: None,
            };
            store.write(&set)?
        }
    };
    let mut json = serde_json::to_value(&view).expect("PR plans are JSON");
    if let (true, Some(fields)) = (changing, json.as_object_mut()) {
        fields.insert("immediate".to_owned(), json!(true));
    }
    let mut table = crate::ui::Table::new(
        ["REPOSITORY", "PR ENVIRONMENTS", "FROM", "DETAILS"],
        format!(
            "No Service in {} deploys from a GitHub repository through the GitHub App.",
            view.project.name
        ),
    );
    for plan in &view.plans {
        let from = match &plan.start_from {
            Some(from) => from.to_string(),
            None if plan.enabled => "pick --from to start".to_owned(),
            None => String::new(),
        };
        let mut words = Vec::new();
        if !plan.copy.is_empty() {
            words.push(format!(
                "also copies {}",
                crate::handlers::joined(&plan.copy)
            ));
        }
        for setup in &plan.setup {
            words.push(format!("then {}: {}", setup.service, setup.command));
        }
        if !plan.remove_on_close {
            words.push("kept after close".to_owned());
        }
        if plan.include_bots {
            words.push("bots too".to_owned());
        }
        for open in &plan.open {
            words.push(format!("#{} {}", open.number, standing(open)));
        }
        table.row([
            crate::ui::Cell::from(plan.repository.to_string()),
            if plan.enabled {
                crate::ui::Cell::status("on", crate::ui::Tone::Good)
            } else {
                crate::ui::Cell::from("off")
            },
            crate::ui::Cell::from(from),
            crate::ui::Cell::from(words.join(" · ")),
        ]);
    }
    crate::ui::list(&json, &table)
}

/// Let a pull request's current head deploy: only that head, so a push waits again. A
/// repository with previews deploys none this way: a preview's commit deploys only with a
/// person's passkey approval of it, on the pull request's page in Ployz Cloud.
pub(crate) fn approve(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let number = matches
        .get_one::<u64>("number")
        .copied()
        .ok_or_else(|| Error::usage("Name the pull request: `ployz env pr approve NUMBER`"))?;
    let number = PullRequestNumber::parse(number)?;
    let repository = matches
        .get_one::<String>("repository")
        .map(|name| ployz_store::RepositoryName::parse(name.as_str()))
        .transpose()?;
    let store = store(root)?;
    let query = PrPlansQuery {
        project: project(matches)?,
    };
    let find =
        |view: &ployz_store::PrPlansView| -> Vec<(ployz_store::RepositoryId, bool, OpenPullRequest)> {
            view.plans
                .iter()
                .filter(|plan| {
                    repository
                        .as_ref()
                        .is_none_or(|name| plan.repository == *name)
                })
                .flat_map(|plan| {
                    plan.open
                        .iter()
                        .filter(|open| open.number == number)
                        .map(|open| (plan.repository_id, plan.preview.is_some(), open.clone()))
                })
                .collect()
        };
    let (repository_id, previews, open) = match find(&store.read(&query)?).as_slice() {
        [only] => only.clone(),
        [] => {
            return Err(Error::not_found(format!(
                "No open pull request #{number}: `ployz env pr` lists them"
            )));
        }
        _ => {
            return Err(Error::usage(
                "More than one repository has that pull request: name one with --repository",
            ));
        }
    };
    if previews {
        return Err(Error::conflict(format!(
            "#{number} deploys as a preview: deploy it from Pull requests in Ployz Cloud"
        )));
    }
    let Some(head) = open.head.clone() else {
        return Err(Error::conflict(format!(
            "GitHub hasn't reported #{number}'s commit yet: try again shortly"
        )));
    };
    store.write(&ApprovePullRequest {
        repository_id,
        number,
        head: head.clone(),
    })?;
    let approved = find(&store.read(&query)?)
        .into_iter()
        .map(|(_, _, open)| open)
        .next()
        .unwrap_or(open);
    let short = head.short();
    let sentence = match &approved.environment {
        Some(environment) => format!("Approved #{number} at {short}. {environment} deploys it."),
        None => format!("Approved #{number} at {short}."),
    };
    crate::ui::done(&approved, sentence)
}

/// A pull request's standing as the PR plans list shows it.
fn standing(open: &OpenPullRequest) -> String {
    let short = |head: &ployz_store::CommitSha| head.short().to_owned();
    match (&open.standing, &open.head) {
        (Standing::Trusted, _) => "deploys".to_owned(),
        (Standing::Approved(head), _) => format!("approved {}", short(head)),
        (Standing::NeedsApproval, Some(head)) => format!("needs approval {}", short(head)),
        (Standing::NeedsApproval, None) => "needs approval".to_owned(),
    }
}
