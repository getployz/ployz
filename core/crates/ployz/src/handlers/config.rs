//! `ployz get`, `set` and `unset`: read and edit Settings in the Config Store.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{Change, Edit, Edited, EnvironmentQuery, Revision, SettingPath};
use serde_json::{Value, json};

use super::store::{environment, failed, scoped, store};
use super::{Error, leaf_matches};
use crate::cli::{positional, value};
use crate::cloud_account::StoreCallError;
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(crate) fn get_command() -> Command {
    scoped(Command::new("get").about("Show Settings: every Service, one Service, or one Setting"))
        .arg(positional("path", false).help("SERVICE or SERVICE.SETTING"))
}

pub(crate) fn set_command() -> Command {
    scoped(Command::new("set").about("Stage Setting values"))
        .arg(
            positional("assignment", true)
                .action(ArgAction::Append)
                .value_name("PATH=VALUE")
                .help("For example web.replicas=3"),
        )
        .arg(expect())
}

pub(crate) fn unset_command() -> Command {
    scoped(Command::new("unset").about("Return Settings to their defaults"))
        .arg(
            positional("path", true)
                .action(ArgAction::Append)
                .help("SERVICE.SETTING"),
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
    };
    let view = store.environment(&query).map_err(failed)?;
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
    let store = store(root)?;
    let edited = store
        .edit(&edit)
        .map_err(|error| failed(with_refresh_hint(error, matches)))?;
    report(&edited)
}

fn report(edited: &Edited) -> Result<(), Error> {
    crate::output::finish(edited, || {
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

fn join(paths: &[SettingPath]) -> String {
    paths
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A refused `--expect` names the command that shows the fresh state.
fn with_refresh_hint(error: StoreCallError, matches: &ArgMatches) -> StoreCallError {
    let StoreCallError::Refused(mut error) = error else {
        return error;
    };
    if error.code == RpcErrorCode::Conflict
        && let Some(details) = error.details.as_object_mut()
    {
        let mut next = vec!["ployz".to_owned(), "get".to_owned()];
        for flag in ["project", "env"] {
            if let Some(value) = matches.get_one::<String>(flag) {
                next.extend([format!("--{flag}"), value.clone()]);
            }
        }
        details.insert("next".into(), json!(shell_words::join(next)));
    }
    StoreCallError::Refused(error)
}

/// A Setting value as a person reads it: text bare, anything else as JSON.
fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "-".to_owned(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}
