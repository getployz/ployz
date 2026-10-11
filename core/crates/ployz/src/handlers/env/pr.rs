//! A PR Environment's plan, what its merge lands, and shutting one down.

use clap::ArgMatches;
use ployz_store::{DeploymentStatus, EnvironmentName, EnvironmentRef, Ride};
use serde_json::{Value, json};

use super::super::deploy;
use super::super::store::{self, project, store};
use super::super::{Error, leaf_matches, required};
use super::branch::setups;
use crate::handlers::teardown::{accepted, take_off, unfinished, with_accepted};

/// Take an Environment off the Servers and keep everything else of it.
pub(super) fn shutdown(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let at = EnvironmentRef {
        project: project(matches)?,
        environment: Some(name.clone()),
    };
    let store = store(root)?.args([name.as_str()]);
    let accept = accepted(matches)?;
    let again = store.again(&with_accepted(&accept));
    let events = deploy::open_events(matches)?;
    let version = matches.get_one::<String>("expect-version").cloned();
    let (view, ran) = take_off(matches, &store, &at, (&accept, version), events)?;
    if view.deployment.status != DeploymentStatus::Applied {
        return unfinished(matches, &view, &[], ran, &again);
    }
    let on = store::next(matches, &["deploy", "--env", name.as_str()]);
    deploy::finish_view(&view, Some(on))
}

/// Show the Project's PR plans, or change one.
pub(super) fn pr(root: &ArgMatches) -> Result<(), Error> {
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
    let query = ployz_store::PrPlansQuery {
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

/// What a PR Environment's pull request lands when it merges.
pub(super) fn on_merge(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store(root)?.read(&ployz_store::OnMergeQuery {
        environment: store::environment(matches)?,
    })?;
    let mut table = crate::ui::Table::new(
        ["INTO", "ROW", "NOW", "LANDS", "DETAILS"],
        format!("Merging #{} lands nothing.", view.pull_request.number),
    );
    for into in &view.destinations {
        for row in &into.rows {
            let (lands, tone) = match row.ride {
                Ride::Rides | Ride::Edited if row.lands.is_null() => {
                    ("removed".to_owned(), crate::ui::Tone::Bad)
                }
                Ride::Rides | Ride::Edited => (shown(&row.lands), crate::ui::Tone::Good),
                Ride::Excluded => ("excluded".to_owned(), crate::ui::Tone::Muted),
                Ride::ProductionKeeps => ("keeps its own".to_owned(), crate::ui::Tone::Muted),
            };
            let mut details = Vec::new();
            if row.ride == Ride::Edited {
                details.push("edited");
            }
            if row.conflict {
                details.push("changed there too");
            }
            table.row([
                crate::ui::Cell::from(into.name.to_string()),
                crate::ui::Cell::from(row.at.to_string()),
                crate::ui::Cell::from(shown(&row.production)),
                crate::ui::Cell::status(lands, tone),
                crate::ui::Cell::from(details.join(" · ")),
            ]);
        }
    }
    crate::ui::list(&view, &table)
}

/// A value as the On merge list shows it.
fn shown(value: &Value) -> String {
    match value {
        Value::Null => "not set".to_owned(),
        Value::String(text) => text.clone(),
        Value::Object(secret) => match secret.get("secret") {
            Some(Value::Bool(true)) => "secret".to_owned(),
            _ => "Not set".to_owned(),
        },
        other @ (Value::Bool(_) | Value::Number(_) | Value::Array(_)) => other.to_string(),
    }
}
