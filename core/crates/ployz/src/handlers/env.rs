//! `ployz env`: Environments in the Config Store.

use clap::{ArgMatches, Command};
use ployz_store::{CreateEnvironment, EnvironmentId, EnvironmentName, Written};

use super::config::{mint, project, store};
use super::{Error, leaf_matches, required};
use crate::cli::{env, positional, value};
use crate::output::say;

pub(crate) fn command() -> Command {
    Command::new("env")
        .about("Manage Environments")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("new")
                .about("Create an empty Environment")
                .arg(positional("name", true))
                .arg(
                    value("project", None)
                        .env(env::PROJECT)
                        .help("Project [default: the only Project]"),
                ),
        )
}

pub(super) fn handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "new" => (new, Supported),
        _ => return None,
    })
}

fn new(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let Written::Environment(created) =
        store(root)?.write(ployz_store::Command::CreateEnvironment(CreateEnvironment {
            id: EnvironmentId::parse(mint())?,
            project: project(matches)?,
            name,
        }))?
    else {
        unreachable!("an Environment create writes an Environment");
    };
    crate::output::finish(&created, || {
        say!(
            "Created Environment {} in Project {}.",
            created.environment.name,
            created.environment.project
        );
    })
}
