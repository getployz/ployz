//! The Config Store access every authoring command shares: which Store, which
//! Organization, the `--project`/`--env` scope, and IDs for creates.

use clap::{Arg, ArgMatches, Command};
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::{
    Actor, ConfigStore, EnvironmentName, EnvironmentRef, OrganizationId, ProjectName,
};
use serde_json::json;

use super::Error;
use crate::cli::{env, value};

/// The Organization of the hidden in-process Store.
const LOCAL_ORGANIZATION: &str = "local";

/// `--project`: which Project a command addresses.
pub(crate) fn project_arg() -> Arg {
    value("project", None)
        .env(env::PROJECT)
        .help("Project [default: the only Project]")
}

/// `--project` and `--env`, which every Environment-scoped command takes.
pub(crate) fn scoped(command: Command) -> Command {
    command.arg(project_arg()).arg(
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

/// A command's result and, when there is one, the command to run next.
#[derive(serde::Serialize)]
pub(crate) struct Next<'a, T> {
    #[serde(flatten)]
    value: &'a T,
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

impl<'a, T> Next<'a, T> {
    pub(crate) fn new(value: &'a T, next: Option<String>) -> Self {
        Self { value, next }
    }
}

/// `ployz WORDS…` in the same Project and Environment as this command.
pub(crate) fn next(matches: &ArgMatches, words: &[&str]) -> String {
    let mut next = vec!["ployz".to_owned()];
    next.extend(words.iter().map(|word| (*word).to_owned()));
    for flag in ["project", "env"] {
        if let Some(value) = matches.get_one::<String>(flag) {
            next.extend([format!("--{flag}"), value.clone()]);
        }
    }
    shell_words::join(next)
}

/// A refused stale write names the `read` command that shows the fresh state.
pub(crate) fn with_refresh_hint(mut error: RpcError, matches: &ArgMatches, read: &str) -> RpcError {
    if error.code == RpcErrorCode::Conflict
        && let Some(details) = error.details.as_object_mut()
    {
        details.insert("next".into(), json!(next(matches, &[read])));
    }
    error
}

/// A Store error, with the next step only the command line can name: an ambiguous
/// Project is fixed by rerunning this command with `--project`.
pub(crate) fn failed(mut error: RpcError) -> Error {
    if error.code == RpcErrorCode::Ambiguous
        && let Some(details) = error.details.as_object_mut()
        && details.contains_key("projects")
    {
        let mut next = vec!["ployz".to_owned()];
        next.extend(std::env::args().skip(1));
        next.extend(["--project".to_owned(), "PROJECT".to_owned()]);
        details.insert("next".into(), json!(shell_words::join(next)));
    }
    error.into()
}
