//! A PR Environment's plan, and shutting one down.

use clap::ArgMatches;
use ployz_store::{DeploymentStatus, EnvironmentName, EnvironmentRef};
use serde_json::json;

use super::super::deploy;
use super::super::store::{self, failed, project, store};
use super::super::{Error, leaf_matches, required};
use super::branch::setups;
use crate::failure::USAGE_EXIT;
use crate::handlers::teardown::{accepted, take_off, unfinished};
use crate::output::say;

/// Take an Environment off the Servers and keep everything else of it.
pub(super) fn shutdown(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let at = EnvironmentRef {
        project: project(matches)?,
        environment: Some(name.clone()),
    };
    let store = store(root)?;
    let words = ["env", "shutdown", name.as_str()];
    let accept = accepted(matches)?;
    let events = deploy::open_events(matches)?;
    let version = matches.get_one::<String>("expect-version").cloned();
    let (view, ran) = take_off(
        matches,
        &store,
        &at,
        (&accept, version),
        events,
        &words,
        &words,
    )?;
    if view.deployment.status != DeploymentStatus::Applied {
        let mut again: Vec<&str> = words.to_vec();
        again.extend(
            accept
                .iter()
                .flat_map(|name| ["--accept-volume-loss", name.as_str()]),
        );
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
    let words = ["env", "pr"];
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
        false => store.read(&query).map_err(failed(matches, &words))?,
        true => {
            let repository = match repository {
                Some(repository) => repository,
                None => {
                    let plans = store.read(&query).map_err(failed(matches, &words))?;
                    match plans.plans.as_slice() {
                        [only] => only.repository.clone(),
                        _ => {
                            return Err(Error::usage(
                                "Name the repository: `ployz env pr` lists the Project's",
                            )
                            .with_exit(USAGE_EXIT));
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
            store.write(&set).map_err(failed(matches, &words))?
        }
    };
    let mut json = serde_json::to_value(&view).expect("PR plans are JSON");
    if let (true, Some(fields)) = (changing, json.as_object_mut()) {
        fields.insert("immediate".to_owned(), json!(true));
    }
    crate::output::finish(&json, || {
        say!("PR Environments of Project {}:", view.project.name);
        if view.plans.is_empty() {
            say!("  No Service deploys from a GitHub repository through the GitHub App.");
        }
        for plan in &view.plans {
            let mut words = vec![if plan.enabled { "on" } else { "off" }.to_owned()];
            match &plan.start_from {
                Some(from) => words.push(format!("from {from}")),
                None if plan.enabled => words.push("pick --from to start".to_owned()),
                None => {}
            }
            if !plan.copy.is_empty() {
                words.push(format!("also copies {}", super::joined(&plan.copy)));
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
            say!("  {}: {}", plan.repository, words.join(" · "));
        }
    })
}
