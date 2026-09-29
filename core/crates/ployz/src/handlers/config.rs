//! `ployz get`, `set` and `unset`: read and edit Settings in the Config Store.
//!
//! `get` narrows by depth: the whole Environment shows what differs from a default
//! (`--all` adds the rest), `get SERVICE` shows every Setting plus the `values`
//! object that `set SERVICE --patch` takes back.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_store::{Change, Edit, EnvironmentQuery, Revision, SettingPath};
use serde_json::Value;

use super::store::{Next, environment, failed, next, scoped, store, with_refresh_hint};
use super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(crate) fn get_command() -> Command {
    scoped(Command::new("get").about("Show Settings: every Service, one Service, or one Setting"))
        .arg(
            positional("path", false)
                .help("SERVICE or SERVICE.SETTING")
                .add(super::catalog::setting_paths()),
        )
        .arg(switch("all", None).help("Include Settings at their default across the Environment"))
}

pub(crate) fn set_command() -> Command {
    scoped(Command::new("set").about("Stage Setting values"))
        .arg(
            positional("assignment", true)
                .action(ArgAction::Append)
                .value_name("PATH=VALUE")
                .help("For example web.replicas=3, or the SERVICE a --patch applies to")
                .add(super::catalog::setting_paths()),
        )
        .arg(
            value("patch", None)
                .value_name("JSON")
                .help("Set a Service's Settings from an object shaped like `get SERVICE --json` values; omitted Settings stay; - reads stdin"),
        )
        .arg(expect())
}

pub(crate) fn unset_command() -> Command {
    scoped(Command::new("unset").about("Return Settings to their defaults"))
        .arg(
            positional("path", true)
                .action(ArgAction::Append)
                .help("SERVICE.SETTING")
                .add(super::catalog::setting_paths()),
        )
        .arg(expect())
}

// Parsed by the handler, not clap: clap's error would echo the rejected value.
fn expect() -> Arg {
    value("expect", None)
        .value_name("REVISION")
        .help("Refuse unless Working State is still at this revision")
}

fn expected(matches: &ArgMatches) -> Result<Option<Revision>, Error> {
    matches
        .get_one::<String>("expect")
        .map(|revision| {
            revision.parse().map(Revision).map_err(|_| {
                Error::usage("Expected --expect REVISION to be a revision number, for example 3")
                    .with_exit(USAGE_EXIT)
            })
        })
        .transpose()
}

pub(super) fn get(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let query = EnvironmentQuery {
        environment: environment(matches)?,
        path: matches
            .get_one::<String>("path")
            .map(|path| SettingPath::parse(path))
            .transpose()?,
        all: matches.get_flag("all"),
    };
    let path = query.path.as_ref().map(ToString::to_string);
    let mut words = vec!["get"];
    words.extend(path.as_deref());
    let view = store.environment(&query).map_err(failed(matches, &words))?;
    crate::output::finish(&view, || {
        if view.settings.is_empty() {
            say!(
                "No Services in {}/{}.",
                view.environment.project,
                view.environment.name
            );
        }
        for row in &view.settings {
            let default = if row.value == row.default {
                " (default)"
            } else {
                ""
            };
            say!("{} = {}{default}", row.path, display_value(&row.value));
        }
    })
}

pub(super) fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    if let Some(patch) = matches.get_one::<String>("patch") {
        let service = match matches.get_many::<String>("assignment") {
            Some(mut paths) if paths.len() == 1 => paths.next().cloned(),
            Some(_) | None => None,
        }
        .ok_or_else(|| {
            Error::usage(
                "--patch takes one SERVICE, for example set web --patch '{\"replicas\":3}'",
            )
        })?;
        let patch = if patch == "-" {
            std::io::read_to_string(std::io::stdin())?
        } else {
            patch.clone()
        };
        let value = serde_json::from_str(&patch).map_err(|_| {
            Error::usage("--patch expects a JSON object, for example '{\"replicas\":3}'")
        })?;
        return edit(
            matches,
            vec![Change::Patch {
                path: SettingPath::parse(&service)?,
                value,
            }],
        );
    }
    let changes = matches
        .get_many::<String>("assignment")
        .into_iter()
        .flatten()
        .map(|assignment| {
            let (path, value) = assignment.split_once('=').ok_or_else(|| {
                Error::usage("Expected PATH=VALUE, for example web.replicas=3")
                    .with_exit(USAGE_EXIT)
            })?;
            Ok(Change::Set {
                path: SettingPath::parse(path)?,
                value: Value::String(value.to_owned()),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    edit(root, changes)
}

pub(super) fn unset(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let changes = matches
        .get_many::<String>("path")
        .into_iter()
        .flatten()
        .map(|path| {
            Ok(Change::Unset {
                path: SettingPath::parse(path)?,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    edit(root, changes)
}

fn edit(root: &ArgMatches, changes: Vec<Change>) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let edit = Edit {
        environment: environment(matches)?,
        expect: expected(matches)?,
        changes,
    };
    let words = rerun(&edit.changes);
    let words = words.iter().map(String::as_str).collect::<Vec<_>>();
    let store = store(root)?;
    let edited = store
        .edit(&edit)
        .map_err(|error| failed(matches, &words)(with_refresh_hint(error, matches, "get")))?;
    let hint = (!edited.staged.is_empty()).then(|| next(matches, &["diff"]));
    crate::output::finish(&Next::new(&edited, hint), || {
        let where_ = format!("{}/{}", edited.environment.project, edited.environment.name);
        if !edited.staged.is_empty() {
            say!(
                "Staged {} in {where_} (revision {}).",
                join(&edited.staged),
                edited.environment.revision
            );
        }
        if !edited.immediate.is_empty() {
            say!("Applied {} in {where_}.", join(&edited.immediate));
        }
    })
}

/// The `set` or `unset` that makes `changes`, with every value left as a placeholder.
fn rerun(changes: &[Change]) -> Vec<String> {
    let verb = match changes.first() {
        Some(Change::Unset { .. }) => "unset",
        Some(Change::Set { .. } | Change::Patch { .. }) | None => "set",
    };
    let mut words = vec![verb.to_owned()];
    for change in changes {
        match change {
            Change::Set { path, .. } => words.push(format!("{path}=VALUE")),
            Change::Unset { path } => words.push(path.to_string()),
            Change::Patch { path, .. } => {
                words.extend([path.to_string(), "--patch".to_owned(), "JSON".to_owned()]);
            }
        }
    }
    words
}

fn join(paths: &[SettingPath]) -> String {
    paths
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A Setting value as a person reads it: text bare, anything else as JSON.
fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "-".to_owned(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}
