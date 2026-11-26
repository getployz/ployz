//! The arguments authoring commands share: the `--project`/`--env` scope and the
//! Service and Volume names they take.

use clap::{Arg, ArgMatches};
use ployz_store::{EnvironmentName, EnvironmentRef, ProjectName};

use super::super::{Error, required};
use crate::cli::value;
use crate::env;

/// `--project`: which Project a command addresses.
pub(crate) fn project_arg() -> Arg {
    value("project", None)
        .env(env::PROJECT)
        .hide_env_values(true)
        .help("Project [default: the only Project]")
}

/// `--project` and `--env`, which every Environment-scoped command takes.
pub(crate) fn scoped(command: clap::Command) -> clap::Command {
    command.arg(project_arg()).arg(
        value("env", None)
            .env(env::ENVIRONMENT)
            .hide_env_values(true)
            .help("Environment [default: the Project's Default Environment]"),
    )
}

/// The Project a command addresses: `--project`, `PLOYZ_PROJECT`, else the directory link.
pub(crate) fn project(matches: &ArgMatches) -> Result<Option<ProjectName>, Error> {
    Ok(super::super::link::scope(matches)?.at().project)
}

/// The Environment a command addresses: flags, environment variables, else the directory link.
pub(crate) fn environment(matches: &ArgMatches) -> Result<EnvironmentRef, Error> {
    Ok(super::super::link::scope(matches)?.at())
}

/// The Environment the required argument `arg` names, in the Project a command addresses.
pub(crate) fn named_environment(
    matches: &ArgMatches,
    arg: &str,
) -> Result<(EnvironmentName, EnvironmentRef), Error> {
    let name = EnvironmentName::parse(required(matches, arg)?)?;
    let at = EnvironmentRef {
        project: project(matches)?,
        environment: Some(name.clone()),
    };
    Ok((name, at))
}

/// Argument `arg`, a Service name. Core's name error quotes the value; a rejected
/// value is never echoed.
pub(crate) fn service_name(
    matches: &ArgMatches,
    arg: &str,
) -> Result<ployz_core::ServiceName, Error> {
    ployz_core::ServiceName::parse(super::required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Service name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit, like web")
    })
}

/// Every value of argument `arg`, Service names.
pub(crate) fn service_names(
    matches: &ArgMatches,
    arg: &str,
) -> Result<Vec<ployz_core::ServiceName>, Error> {
    names(
        matches,
        arg,
        |name| ployz_core::ServiceName::parse(name).ok(),
        "Service",
    )
}

/// Every value of argument `arg`, Volume names.
pub(crate) fn volume_names(
    matches: &ArgMatches,
    arg: &str,
) -> Result<Vec<ployz_store::VolumeName>, Error> {
    names(
        matches,
        arg,
        |name| ployz_store::VolumeName::parse(name).ok(),
        "Volume",
    )
}

/// A rejected value is never echoed.
fn names<T>(
    matches: &ArgMatches,
    arg: &str,
    parse: impl Fn(String) -> Option<T>,
    what: &str,
) -> Result<Vec<T>, Error> {
    super::super::string_values(matches, arg)
        .into_iter()
        .map(|name| {
            parse(name).ok_or_else(|| {
                Error::usage(format!(
                    "Expected {what} names: each up to 63 lowercase letters, digits and -, starting and ending with a letter or digit"
                ))
            })
        })
        .collect()
}

/// Argument `arg`, a Volume name.
pub(crate) fn volume_name(
    matches: &ArgMatches,
    arg: &str,
) -> Result<ployz_store::VolumeName, Error> {
    ployz_store::VolumeName::parse(super::required(matches, arg)?).map_err(|_| {
        Error::usage("Expected a Volume name: up to 63 lowercase letters, digits and -, starting and ending with a letter or digit, like data")
    })
}
