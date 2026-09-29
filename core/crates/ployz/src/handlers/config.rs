//! `ployz get`, `set` and `unset`: read and edit Settings in the Config Store,
//! and the Store access every authoring command shares.

use clap::{Arg, ArgAction, ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    Actor, Change, ConfigStore, Edit, Edited, EnvironmentName, EnvironmentQuery, EnvironmentRef,
    OrganizationId, ProjectName, Query, Revision, View, Written,
};
use serde_json::{Value, json};

use super::{Error, config_path, leaf_matches, runtime};
use crate::cli::{env, positional, value};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::CredentialStore;
use crate::output::say;

/// The Organization of the hidden in-process Store.
const LOCAL_ORGANIZATION: &str = "local";

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

/// The Config Store a command reads and writes: Cloud's over HTTPS, as
/// `PLOYZ_TOKEN` or this device's sign-in, or the hidden in-process SQLite Store
/// when `PLOYZ_STORE` is set.
pub(crate) enum Store {
    Local(ConfigStore, Actor),
    Cloud(tokio::runtime::Runtime, Credential),
}

impl Store {
    pub(crate) fn read(&self, query: &Query) -> Result<View, StoreCallError> {
        match self {
            Self::Local(store, actor) => store.read(actor, query).map_err(StoreCallError::Refused),
            Self::Cloud(runtime, credential) => {
                runtime.block_on(cloud_account::config_store(credential, "read", query))
            }
        }
    }

    pub(crate) fn write(&self, command: ployz_store::Command) -> Result<Written, StoreCallError> {
        match self {
            Self::Local(store, actor) => {
                store.write(actor, command).map_err(StoreCallError::Refused)
            }
            Self::Cloud(runtime, credential) => {
                runtime.block_on(cloud_account::config_store(credential, "write", &command))
            }
        }
    }
}

impl From<StoreCallError> for Error {
    fn from(error: StoreCallError) -> Self {
        match error {
            StoreCallError::Refused(error) => error.into(),
            StoreCallError::Cloud(error) => error.into(),
        }
    }
}

pub(crate) fn store(root: &ArgMatches) -> Result<Store, Error> {
    if let Ok(url) = std::env::var(env::STORE) {
        let actor = Actor {
            organization: OrganizationId::parse(LOCAL_ORGANIZATION).expect("a valid ID"),
        };
        return Ok(Store::Local(ConfigStore::open(&url)?, actor));
    }
    let credentials = CredentialStore::beside(&config_path(leaf_matches(root))?);
    let token = std::env::var(env::TOKEN).ok();
    let cloud = std::env::var(env::CLOUD_URL).ok();
    let runtime = runtime()?;
    let credential = runtime.block_on(cloud_account::credential(&credentials, token, cloud))?;
    Ok(Store::Cloud(runtime, credential))
}

/// Mint the ID a create is keyed by, so a retried request replays instead of repeating.
pub(crate) fn mint() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(super) fn get(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let query = Query::Environment(EnvironmentQuery {
        environment: environment(matches)?,
        path: matches.get_one::<String>("path").cloned(),
    });
    let View::Environment(view) = store.read(&query)?;
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
    edit(root, changes)
}

pub(super) fn unset(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let changes = matches
        .get_many::<String>("path")
        .into_iter()
        .flatten()
        .map(|path| Change::Unset { path: path.clone() })
        .collect();
    edit(root, changes)
}

fn edit(root: &ArgMatches, changes: Vec<Change>) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let written = store(root)?.write(ployz_store::Command::Edit(Edit {
        environment: environment(matches)?,
        expect: matches.get_one::<u64>("expect").copied().map(Revision),
        changes,
    }));
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
fn stale(error: StoreCallError, matches: &ArgMatches) -> Error {
    let StoreCallError::Refused(mut error) = error else {
        return error.into();
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
