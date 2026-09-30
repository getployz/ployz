//! `ployz project`: Projects in the Config Store. `rm` removes a Project through
//! the same teardown path as `env rm`, one Environment at a time.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    CreateProject, DeploymentSummary, EnvironmentId, EnvironmentRef, ProjectId, ProjectName,
    ProjectRemoved, RemoveProject, RenameProject,
};
use serde_json::json;

use super::store::{self, Store, mint, store};
use super::teardown::{confirmed, inventory, remove_all};
use super::{Error, deploy, leaf_matches, required};
use crate::cli::{base, positional, value};
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
        .subcommand(Command::new("ls").about("List the Organization's Projects"))
        .subcommand(
            Command::new("rename")
                .about("Rename a Project; its running Environments keep their Namespaces")
                .arg(positional("project", true))
                .arg(positional("name", true).help("Its new name, unique in the Organization")),
        )
        .subcommand(deploy::following(
            base(
                "rm",
                "Remove a Project: every Environment from the Servers first, then from Ployz",
            )
            .long_about(
                "Remove a Project. Each Environment that ran is taken off the Servers by \
                 a removal Deployment, Branches before their Parents and the Default \
                 Environment last, deleting deployed Volumes once each is accepted by \
                 name; then the Project, its configuration and history go. Type its name \
                 with --confirm; without it the command fails with confirmation_required, \
                 naming what goes and the exact retry. If a removal doesn't apply, the \
                 same command finishes it.",
            )
            .arg(positional("name", true))
            .arg(
                value("confirm", None)
                    .value_name("PROJECT")
                    .help("The Project's name, typed to confirm its removal"),
            )
            .arg(crate::cli::volume_acceptance())
            .arg(crate::cli::reviewed_version()),
        ))
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "new" => new,
        "ls" => ls,
        "rename" => rename,
        "rm" => rm,
        _ => return None,
    })
}

fn new(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = ProjectName::parse(required(matches, "name")?)?;
    let store = store(root)?.args([name.as_str()]);
    let create = CreateProject {
        id: ProjectId::parse(mint())?,
        name,
        default_environment: EnvironmentId::parse(mint())?,
    };
    let created = store.write(&create)?;
    crate::output::finish(&created, || {
        say!(
            "Created Project {} with Environment {}.",
            created.project.name,
            created.environment.name
        );
    })
}

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let listed = store(root)?.read(&ployz_store::ProjectsQuery {})?;
    crate::output::finish(&listed, || {
        if listed.projects.is_empty() {
            say!("No Projects yet. Create one: ployz project new NAME");
        }
        for project in &listed.projects {
            let environments: Vec<String> = project
                .environments
                .iter()
                .map(|name| match name == &project.default_environment {
                    true => format!("{name}*"),
                    false => name.to_string(),
                })
                .collect();
            say!("{}\t{}", project.name, environments.join(", "));
        }
    })
}

fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let rename = RenameProject {
        project: ProjectName::parse(required(matches, "project")?)?,
        name: ProjectName::parse(required(matches, "name")?)?,
    };
    let renamed = store(root)?
        .args([rename.project.as_str(), rename.name.as_str()])
        .write(&rename)?;
    let links = super::link::rename_project(
        &super::config_path(matches)?,
        &rename.project,
        &renamed.name,
    )?;
    crate::output::finish(&json!({ "project": renamed, "links": links }), || {
        say!("Renamed Project {} to {}.", rename.project, renamed.name);
        if links > 0 {
            say!("Moved {links} linked director(ies) on this device to it.");
        }
    })
}

/// What `project rm` removed, and the Deployments that took it off the Servers.
#[derive(serde::Serialize)]
struct Removal<'a> {
    #[serde(flatten)]
    removed: &'a ProjectRemoved,
    deployments: &'a [DeploymentSummary],
}

/// Remove a Project: while the Store names an Environment of it that may still
/// run, take that one off the Servers (as `env rm` does), then delete it all.
fn rm(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = ProjectName::parse(required(matches, "name")?)?;
    let store = store(root)?.args([name.as_str()]);
    let again = ["project", "rm", name.as_str(), "--confirm", name.as_str()];
    if !confirmed(matches, name.as_str(), "Project")? {
        return Err(unconfirmed(matches, &store, &name, &again)?);
    }
    let remove = RemoveProject {
        project: name.clone(),
    };
    let events = deploy::open_events(matches)?;
    match remove_all(matches, &store, &remove, &name, events, &again)? {
        Some((removed, ran)) => finish(&removed, &ran),
        None => Ok(()),
    }
}

/// Refuse an unconfirmed `project rm`, naming every Environment with what goes
/// with it, and the exact retry.
fn unconfirmed(
    matches: &ArgMatches,
    store: &Store,
    project: &ProjectName,
    again: &[&str],
) -> Result<Error, Error> {
    let listed = store.read(&ployz_store::ProjectsQuery {})?;
    let Some(listing) = listed
        .projects
        .iter()
        .find(|listing| &listing.name == project)
    else {
        return Err(Error::detailed(
            RpcErrorCode::NotFound,
            format!("No Project named {project}"),
            json!({ "next": "ployz project ls" }),
        ));
    };
    let environments = listing
        .environments
        .iter()
        .map(|environment| {
            let at = EnvironmentRef {
                project: Some(project.clone()),
                environment: Some(environment.clone()),
            };
            inventory(store, &at)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let retry = store::next(matches, again);
    Ok(Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Removing Project {project} deletes every Environment in it ({}) with its \
             configuration, history, Services and Volumes; this can't be undone. No changes \
             made.\nRetry: {retry}",
            super::joined(&listing.environments)
        ),
        json!({ "project": project, "environments": environments, "next": retry }),
    ))
}

fn finish(removed: &ProjectRemoved, ran: &[DeploymentSummary]) -> Result<(), Error> {
    let removal = Removal {
        removed,
        deployments: ran,
    };
    crate::output::finish(&removal, || {
        for deployment in ran {
            say!(
                "Took an Environment off the Servers (Deployment #{}).",
                deployment.number
            );
        }
        say!(
            "Removed Project {} and its Environments ({}).",
            removed.project.name,
            super::joined(&removed.environments)
        );
    })
}
