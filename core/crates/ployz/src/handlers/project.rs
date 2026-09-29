//! `ployz project`: Projects in the Config Store.

use clap::{ArgMatches, Command};
use ployz_store::{CreateProject, EnvironmentId, ProjectId, ProjectName};

use super::store::{failed, mint, store};
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
    let matches = leaf_matches(root);
    let name = ProjectName::parse(required(matches, "name")?)?;
    let store = store(root)?;
    let create = CreateProject {
        id: ProjectId::parse(mint())?,
        name,
        default_environment: EnvironmentId::parse(mint())?,
    };
    let words = ["project", "new", create.name.as_str()];
    let created = store
        .create_project(&create)
        .map_err(failed(matches, &words))?;
    crate::output::finish(&created, || {
        say!(
            "Created Project {} with Environment {}.",
            created.project.name,
            created.environment.name
        );
    })
}
