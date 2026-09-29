//! `ployz project`: Projects in the Config Store. `rm` removes a Project through
//! the same teardown path as `env rm`, one Environment at a time.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    CreateProject, DeploymentStatus, DeploymentSummary, EnvironmentId, EnvironmentName,
    EnvironmentRef, ProjectId, ProjectName, ProjectRemoved, RemovalsQuery, RemoveProject,
};
use serde_json::json;

use super::env::{accepted, confirmed, inventory, take_off, unfinished};
use super::store::{self, Store, failed, mint, store};
use super::{Error, deploy, leaf_matches, required};
use crate::cli::{base, positional, value};
use crate::cloud_account::StoreCallError;
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
            .arg(crate::cli::volume_acceptance()),
        ))
}

pub(super) fn handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "new" => (new, Supported),
        "ls" => (ls, Supported),
        "rm" => (rm, Supported),
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

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let listed = store(root)?
        .projects()
        .map_err(failed(matches, &["project", "ls"]))?;
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
    let store = store(root)?;
    let words = ["project", "rm", name.as_str()];
    let accept = accepted(matches)?;
    let mut again = vec!["project", "rm", name.as_str(), "--confirm", name.as_str()];
    if !confirmed(matches, name.as_str(), "Project")? {
        return Err(unconfirmed(matches, &store, &name, &again)?);
    }
    again.extend(
        accept
            .iter()
            .flat_map(|name| ["--accept-volume-loss", name.as_str()]),
    );
    let remove = RemoveProject {
        project: name.clone(),
    };
    let mut events = deploy::open_events(matches)?;
    let mut ran: Vec<DeploymentSummary> = Vec::new();
    loop {
        let environment = match store.remove_project(&remove) {
            Ok(removed) => return finish(&removed, &ran),
            Err(StoreCallError::Refused(error))
                if error.details.get("deployed") == Some(&json!(true)) =>
            {
                EnvironmentName::parse(
                    error
                        .details
                        .get("environment")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                )?
            }
            Err(error) => return Err(failed(matches, &words)(error)),
        };
        let at = EnvironmentRef {
            project: Some(name.clone()),
            environment: Some(environment),
        };
        // Each removal accepts only the Volumes it deletes; a name may recur across Environments.
        let deletes = store
            .removals(&RemovalsQuery {
                environment: at.clone(),
                remove: true,
            })
            .map_err(failed(matches, &words))?;
        let accept: Vec<_> = accept
            .iter()
            .filter(|name| deletes.volumes.iter().any(|volume| &volume.name == *name))
            .cloned()
            .collect();
        let writer = events
            .as_mut()
            .map(|writer| writer.get_ref().try_clone())
            .transpose()?
            .map(std::io::BufWriter::new);
        let (view, outcome) = take_off(matches, &store, &at, &accept, writer, &words, &again)?;
        if view.deployment.status != DeploymentStatus::Applied {
            return unfinished(matches, &view, outcome, &again);
        }
        ran.push(view.deployment);
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
    let words = ["project", "rm"];
    let listed = store.projects().map_err(failed(matches, &words))?;
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
            inventory(matches, store, &at)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let retry = store::next(matches, again);
    Ok(Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Removing Project {project} deletes every Environment in it ({}) with its \
             configuration, history, Services and Volumes; this can't be undone. No changes \
             made.\nRetry: {retry}",
            listing
                .environments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
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
            removed
                .environments
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    })
}
