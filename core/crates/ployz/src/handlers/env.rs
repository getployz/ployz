//! `ployz env`: Environments in the Config Store.

use clap::{ArgMatches, Command};
use ployz_store::{CreateEnvironment, EnvironmentId, EnvironmentName};

use super::store::{failed, mint, project, project_arg, store};
use super::{Error, leaf_matches, required};
use crate::cli::positional;
use crate::output::say;

pub(crate) fn command() -> Command {
    Command::new("env")
        .about("Manage Environments")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("new")
                .about("Create an empty Environment")
                .arg(positional("name", true))
                .arg(project_arg()),
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
    let (store, actor) = store()?;
    let create = CreateEnvironment {
        id: EnvironmentId::parse(mint())?,
        project: project(matches)?,
        name,
    };
    let created = store.create_environment(&actor, &create).map_err(failed)?;
    crate::output::finish(&created, || {
        say!(
            "Created Environment {} in Project {}.",
            created.environment.name,
            created.environment.project
        );
    })
}
