//! A PR Environment's plan, what its merge lands, and shutting one down.

use clap::ArgMatches;
use ployz_core::config::At;
use ployz_store::{DeploymentStatus, EnvironmentName, EnvironmentRef, MergeRow, Ride};
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
            let Shown {
                now,
                lands,
                tone,
                details,
            } = shown_row(row);
            table.row([
                crate::ui::Cell::from(into.name.to_string()),
                crate::ui::Cell::from(row.at.to_string()),
                crate::ui::Cell::from(now),
                crate::ui::Cell::status(lands, tone),
                crate::ui::Cell::from(details.join(" · ")),
            ]);
        }
    }
    crate::ui::list(&view, &table)
}

/// One On merge row as the table shows it.
#[derive(Debug, PartialEq)]
struct Shown {
    now: String,
    lands: String,
    tone: crate::ui::Tone,
    details: Vec<String>,
}

fn shown_row(row: &MergeRow) -> Shown {
    let mut details = Vec::new();
    // A new node's row is a node row too; only a removal has no preview.
    if *row.at.row().at() == At::Node && row.preview.is_null() {
        let (lands, tone) = match row.ride {
            Ride::Kept => ("Kept".to_owned(), crate::ui::Tone::Muted),
            Ride::Excluded => ("Excluded".to_owned(), crate::ui::Tone::Muted),
            Ride::ProductionKeeps => ("keeps its own".to_owned(), crate::ui::Tone::Muted),
            Ride::Rides | Ride::Edited if !row.refused_by.is_empty() => {
                details.extend(row.refused_by.iter().map(ToString::to_string));
                ("In use".to_owned(), crate::ui::Tone::Muted)
            }
            Ride::Rides | Ride::Edited => {
                if row.conflict {
                    details.push("Changed there".to_owned());
                }
                (format!("Removes {}", row.at.node()), crate::ui::Tone::Bad)
            }
        };
        return Shown {
            now: String::new(),
            lands,
            tone,
            details,
        };
    }
    let (lands, tone) = match row.ride {
        Ride::Rides | Ride::Edited if row.lands.is_null() => {
            ("removed".to_owned(), crate::ui::Tone::Bad)
        }
        Ride::Rides | Ride::Edited if !row.production.is_null() && is_file(&row.lands) => {
            ("changed".to_owned(), crate::ui::Tone::Good)
        }
        Ride::Rides | Ride::Edited => (shown(&row.lands), crate::ui::Tone::Good),
        Ride::Excluded => ("excluded".to_owned(), crate::ui::Tone::Muted),
        Ride::ProductionKeeps => ("keeps its own".to_owned(), crate::ui::Tone::Muted),
        Ride::Kept => ("Kept".to_owned(), crate::ui::Tone::Muted),
    };
    if row.ride == Ride::Edited {
        details.push("edited".to_owned());
    }
    if row.conflict {
        details.push("changed there too".to_owned());
    }
    Shown {
        now: shown(&row.production),
        lands,
        tone,
        details,
    }
}

fn is_file(value: &Value) -> bool {
    value.get("content").is_some()
}

/// A value as the On merge list shows it: a secret only as set or Not set, a
/// Config file's contents as a file.
fn shown(value: &Value) -> String {
    if *value == json!({ "secret": true }) {
        return "secret".to_owned();
    }
    if *value == json!({ "secret": false }) {
        return "Not set".to_owned();
    }
    match value {
        Value::Null => "not set".to_owned(),
        Value::String(text) => text.clone(),
        Value::Object(file) if file.contains_key("content") => "file".to_owned(),
        other @ (Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_)) => {
            other.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Shown, shown, shown_row};
    use crate::ui::Tone;

    #[test]
    fn only_a_missing_secret_reads_not_set() {
        assert_eq!(shown(&json!({ "secret": false })), "Not set");
        assert_eq!(shown(&json!({ "secret": true })), "secret");
        assert_eq!(
            shown(&json!({ "type": "http", "path": "/up" })),
            r#"{"path":"/up","type":"http"}"#
        );
    }

    /// pr-5's row removing `db`, `ride` and `refused_by` as given.
    fn removal(ride: &str, conflict: bool, refused_by: serde_json::Value) -> Shown {
        let row = serde_json::from_value(json!({
            "row": "00000000-0000-4000-8000-000000000020:node",
            "node": "db",
            "kind": "service",
            "name": null,
            "production": { "image": "postgres:17" },
            "lands": null,
            "preview": null,
            "ride": ride,
            "conflict": conflict,
            "requires": null,
            "refused_by": refused_by,
        }))
        .unwrap();
        shown_row(&row)
    }

    #[test]
    fn a_new_node_row_still_shows_what_lands() {
        let row = serde_json::from_value(json!({
            "row": "00000000-0000-4000-8000-000000000020:node",
            "node": "db",
            "kind": "service",
            "name": null,
            "production": null,
            "lands": "postgres:17",
            "preview": "postgres:17",
            "ride": "rides",
            "conflict": false,
            "requires": null,
        }))
        .unwrap();
        assert_eq!(shown_row(&row).lands, "postgres:17");
    }

    #[test]
    fn a_node_row_reads_as_its_removal_without_json() {
        let shown = |lands: &str, tone, details: &[&str]| Shown {
            now: String::new(),
            lands: lands.to_owned(),
            tone,
            details: details.iter().map(ToString::to_string).collect(),
        };
        assert_eq!(
            removal("rides", false, json!([])),
            shown("Removes db", Tone::Bad, &[])
        );
        assert_eq!(
            removal("rides", true, json!([])),
            shown("Removes db", Tone::Bad, &["Changed there"])
        );
        let web = json!([{
            "row": "00000000-0000-4000-8000-000000000003:variables.DB_URL",
            "node": "web",
            "kind": "service",
            "name": "env.DB_URL",
        }]);
        assert_eq!(
            removal("rides", false, web),
            shown("In use", Tone::Muted, &["web.env.DB_URL"])
        );
        assert_eq!(
            removal("excluded", false, json!([])),
            shown("Excluded", Tone::Muted, &[])
        );
        assert_eq!(
            removal("kept", false, json!([])),
            shown("Kept", Tone::Muted, &[])
        );
    }
}
