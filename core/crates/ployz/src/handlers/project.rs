//! `ployz project`: Projects in the Config Store. `rm` removes a Project through
//! the same teardown path as `env rm`, one Environment at a time.

use super::catalog::{Approval::*, Runnable, cloud};
use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    CreateProject, DeploymentSummary, EnvironmentId, EnvironmentRef, ProjectId, ProjectName,
    ProjectRemoved, RemoveProject, RenameProject,
};
use serde_json::json;

use super::store::{Store, mint, store};
use super::teardown::{confirm, inventory, remove_all};
use super::{Error, deploy, leaf_matches, required};
use crate::cli::{base, positional, value};
use crate::ui::{Hint, Table, Tree};

pub(crate) fn command() -> Command {
    Command::new("project")
        .about("Manage Projects")
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
                 with --confirm, or in a terminal when it asks; elsewhere it fails with \
                 confirmation_required, naming what goes and the exact retry. If a removal doesn't apply, the \
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

pub(super) fn handler(path: &str) -> Option<Runnable> {
    Some(match path {
        "new" => cloud(Never, new),
        "ls" => cloud(Never, ls),
        "rename" => cloud(Never, rename),
        "rm" => cloud(Always, rm),
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
    let created = store.write(&create)?;
    crate::ui::finish(&created, || {
        crate::ui::stream(format_args!(
            "Created Project {} with Environment {}.",
            created.project.name, created.environment.name
        ));
    })
}

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let listed = store(root)?.read(&ployz_store::ProjectsQuery {})?;
    let mut table = Table::new(
        ["PROJECT", "DEFAULT ENVIRONMENT", "ENVIRONMENTS"],
        "No Projects yet.",
    );
    for project in &listed.projects {
        table.row([
            project.name.to_string(),
            project.default_environment.to_string(),
            super::joined(&project.environments),
        ]);
    }
    crate::ui::list(&listed, &table)?;
    if listed.projects.is_empty() {
        crate::ui::hint(&Hint::Next("ployz project new NAME".to_owned()));
    }
    Ok(())
}

fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let rename = RenameProject {
        project: ProjectName::parse(required(matches, "project")?)?,
        name: ProjectName::parse(required(matches, "name")?)?,
    };
    let config = super::config_path(matches)?;
    let store = store(root)?;
    // Resolved before the write, so a failed lookup cannot strand a done rename.
    let acting = super::link::identity(&store)?.organization;
    let renamed = store.write(&rename)?;
    crate::ui::stream(format_args!(
        "Renamed Project {} to {}.",
        rename.project, renamed.name
    ));
    // The rename is committed; moving this device's links is a follow-up.
    let links =
        super::link::rename_project(&config, acting.as_ref(), &rename.project, &renamed.name);
    match links {
        Ok(1) => crate::ui::stream("Moved 1 linked directory on this device to it."),
        Ok(moved @ 2..) => crate::ui::stream(format_args!(
            "Moved {moved} linked directories on this device to it."
        )),
        Ok(0) | Err(_) => {}
    }
    crate::ui::emit_committed(
        json!({ "project": renamed, "links": links.as_ref().ok() }),
        links.map(drop),
    )
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
    let store = store(root)?.args([name.as_str(), "--confirm", name.as_str()]);
    confirm(matches, name.as_str(), "Project", store.again(&[]), || {
        unconfirmed(&store, &name)
    })?;
    let remove = RemoveProject {
        project: name.clone(),
    };
    let events = deploy::open_events(matches)?;
    match remove_all(matches, &store, &remove, &name, events)? {
        Some((removed, ran)) => finish(&removed, &ran),
        None => Ok(()),
    }
}

/// Refuse an unconfirmed `project rm`, naming every Environment with what goes
/// with it, and the exact retry; and the same as a tree for the prompt.
fn unconfirmed(store: &Store, project: &ProjectName) -> Result<(Error, Tree), Error> {
    let listed = store.read(&ployz_store::ProjectsQuery {})?;
    let Some(listing) = listed
        .projects
        .iter()
        .find(|listing| &listing.name == project)
    else {
        return Err(Error::not_found(format!("No Project named {project}"))
            .hint(Hint::Next("ployz project ls".into())));
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
    let retry = store.again(&[]);
    let loss = Tree::new(
        format!("Removing Project {project} deletes, for good:"),
        environments
            .iter()
            .map(|inventory| {
                Tree::new(inventory.environment.name.to_string(), inventory.branches())
            })
            .collect(),
    );
    let refusal = Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Removing Project {project} deletes every Environment in it ({}) with its \
             configuration, history, Services and Volumes; this can't be undone. No changes \
             made.",
            super::joined(&listing.environments)
        ),
        json!({ "project": project, "environments": environments }),
    )
    .hint(Hint::Retry(retry));
    Ok((refusal, loss))
}

fn finish(removed: &ProjectRemoved, ran: &[DeploymentSummary]) -> Result<(), Error> {
    let removal = Removal {
        removed,
        deployments: ran,
    };
    crate::ui::finish(&removal, || {
        for deployment in ran {
            if super::teardown::left_on_old_servers(deployment) {
                crate::ui::stream(format_args!(
                    "Left an Environment on old servers: no Server was left to take it off (Deployment #{}).",
                    deployment.number
                ));
            } else {
                crate::ui::stream(format_args!(
                    "Took an Environment off the Servers (Deployment #{}).",
                    deployment.number
                ));
            }
        }
        crate::ui::stream(format_args!(
            "Removed Project {} and its Environments ({}).",
            removed.project.name,
            super::joined(&removed.environments)
        ));
    })
}
