//! `ployz project`: Projects in the Config Store.

use clap::{ArgMatches, Command};
use ployz_store::{CreateProject, EnvironmentId, ProjectId, ProjectName, Written};

use super::config::{mint, store};
use super::{Error, leaf_matches, required};
use crate::cli::positional;
use crate::output::say;

pub(crate) fn command() -> Command {
    Command::new("project")
        .about("Manage Projects")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("new")
                .about("Create a Project with its Default Environment, production")
                .arg(positional("name", true)),
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
    let name = ProjectName::parse(required(leaf_matches(root), "name")?)?;
    let (store, actor) = store()?;
    let Written::Project(created) = store.write(
        &actor,
        ployz_store::Command::CreateProject(CreateProject {
            id: ProjectId::parse(mint())?,
            name,
            default_environment: EnvironmentId::parse(mint())?,
        }),
    )?
    else {
        unreachable!("a Project create writes a Project");
    };
    crate::output::finish(&created, || {
        say!(
            "Created Project {} with Environment {}.",
            created.project.name,
            created.environment.name
        );
    })
}
