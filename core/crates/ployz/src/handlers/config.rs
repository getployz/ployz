//! `ployz get`, `set` and `unset`: read and edit Settings in the Config Store,
//! and the Store access every authoring command shares.
//!
//! `get` narrows by depth: the whole Environment shows what differs from a default
//! (`--all` adds the rest), `get SERVICE` shows every Setting plus the `values`
//! object that `set SERVICE --patch` takes back.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    Actor, Change, ConfigStore, Edit, Edited, EnvironmentName, EnvironmentQuery, EnvironmentRef,
    OrganizationId, ProjectName, Query, Revision, View, Written,
};
use serde_json::{Value, json};

use super::{Error, leaf_matches};
use crate::cli::{env, positional, switch, value};
use crate::output::say;

/// The Organization of the hidden in-process Store.
const LOCAL_ORGANIZATION: &str = "local";

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

fn expect() -> Arg {
    value("expect", None)
        .value_name("REVISION")
        .value_parser(clap::value_parser!(u64))
        .help("Refuse unless Working State is still at this revision")
}

/// `--project` and `--env`, which every Environment-scoped command takes.
pub(crate) fn scoped(command: Command) -> Command {
    command
        .arg(
            value("project", None)
                .env(env::PROJECT)
                .help("Project [default: the only Project]"),
        )
        .arg(
            value("env", None)
                .env(env::ENVIRONMENT)
                .help("Environment [default: the Project's Default Environment]"),
        )
}

pub(crate) fn project(matches: &ArgMatches) -> Result<Option<ProjectName>, Error> {
    matches
        .get_one::<String>("project")
        .map(|name| ProjectName::parse(name.as_str()))
        .transpose()
        .map_err(Into::into)
}

pub(crate) fn environment(matches: &ArgMatches) -> Result<EnvironmentRef, Error> {
    Ok(EnvironmentRef {
        project: project(matches)?,
        environment: matches
            .get_one::<String>("env")
            .map(|name| EnvironmentName::parse(name.as_str()))
            .transpose()?,
    })
}

/// The Config Store this command writes to. Only the hidden in-process SQLite
/// Store exists yet; Cloud's arrives with its HTTPS transport.
pub(crate) fn store() -> Result<(ConfigStore, Actor), Error> {
    let Ok(url) = std::env::var(env::STORE) else {
        return Err(Error::coded(
            RpcErrorCode::Unsupported,
            "This command needs Ployz Cloud's Config Store, which this CLI cannot reach yet",
        ));
    };
    let store = ConfigStore::open(&url)?;
    let actor = Actor {
        organization: OrganizationId::parse(LOCAL_ORGANIZATION).expect("a valid ID"),
    };
    Ok((store, actor))
}

/// Mint the ID a create is keyed by, so a retried request replays instead of repeating.
pub(crate) fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(super) fn get(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (store, actor) = store()?;
    let query = Query::Environment(EnvironmentQuery {
        environment: environment(matches)?,
        path: matches.get_one::<String>("path").cloned(),
        all: matches.get_flag("all"),
    });
    let View::Environment(view) = store.read(&actor, &query)?;
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
            say!("{} = {}{default}", row.path, text(&row.value));
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
                path: service,
                value,
            }],
        );
    }
    let changes = matches
        .get_many::<String>("assignment")
        .into_iter()
        .flatten()
        .map(|assignment| {
            let (path, value) = assignment
                .split_once('=')
                .ok_or_else(|| Error::usage("Expected PATH=VALUE, for example web.replicas=3"))?;
            Ok(Change::Set {
                path: path.to_owned(),
                value: Value::String(value.to_owned()),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    edit(matches, changes)
}

pub(super) fn unset(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let changes = matches
        .get_many::<String>("path")
        .into_iter()
        .flatten()
        .map(|path| Change::Unset { path: path.clone() })
        .collect();
    edit(matches, changes)
}

fn edit(matches: &ArgMatches, changes: Vec<Change>) -> Result<(), Error> {
    let (store, actor) = store()?;
    let written = store.write(
        &actor,
        ployz_store::Command::Edit(Edit {
            environment: environment(matches)?,
            expect: matches.get_one::<u64>("expect").copied().map(Revision),
            changes,
        }),
    );
    let Written::Edited(edited) = written.map_err(|error| stale(error, matches))? else {
        unreachable!("an edit writes an edit");
    };
    report(&edited)
}

fn report(edited: &Edited) -> Result<(), Error> {
    crate::output::finish(edited, || {
        let where_ = format!("{}/{}", edited.environment.project, edited.environment.name);
        if !edited.staged.is_empty() {
            say!(
                "Staged {} in {where_} (revision {}).",
                edited.staged.join(", "),
                edited.environment.revision
            );
        }
        if !edited.immediate.is_empty() {
            say!("Applied {} in {where_}.", edited.immediate.join(", "));
        }
    })
}

/// A refused `--expect` names the command that shows the fresh state.
fn stale(error: ployz_core::RpcError, matches: &ArgMatches) -> Error {
    let mut error = error;
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
    error.into()
}

/// A Setting value as a person reads it: text bare, anything else as JSON.
fn text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "-".to_owned(),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}
