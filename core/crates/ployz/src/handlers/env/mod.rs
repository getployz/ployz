//! `ployz env`: Environments in the Config Store, and Branches of them. `ls`,
//! `default` and `rm` manage them; `rm` is the one teardown path. A Branch
//! copies the Services and Volumes picked (and what they use that its Parent
//! doesn't run) and uses the rest live; `save` stages its changes in its Parent,
//! `update` stages what its Parent deployed since, `copy` turns a Live Node into
//! its own copy, and `keep` keeps it after a Save.

mod branch;
mod pr;

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    CreateEnvironment, DeploymentStatus, DeploymentSummary, EnvironmentId, EnvironmentName,
    EnvironmentRef, EnvironmentRemoved, EnvironmentsQuery, EnvironmentsView, RemoveEnvironment,
    SetDefaultEnvironment,
};
use serde_json::json;

use super::config::expect;
use super::deploy;
use super::store::{self, failed, mint, project, project_arg, store};
use super::teardown::{Inventory, accepted, confirmed, inventory, take_off, unfinished};
use super::{Error, leaf_matches, required};
use crate::cli::{base, positional, repeated, switch, value};
use crate::cloud_account::StoreCallError;
use crate::output::say;
use branch::moving;

pub(crate) fn command() -> Command {
    Command::new("env")
        .about("Manage Environments and Branches")
        .arg_required_else_help(true)
        .subcommand(
            Command::new("new")
                .about("Create an empty Environment")
                .arg(positional("name", true))
                .arg(project_arg()),
        )
        .subcommand(
            Command::new("ls")
                .about("List the Project's Environments")
                .arg(project_arg()),
        )
        .subcommand(
            Command::new("default")
                .about("Make an Environment the Project's Default Environment")
                .arg(positional("name", true))
                .arg(project_arg()),
        )
        .subcommand(deploy::following(
            base(
                "rm",
                "Remove an Environment: from the Servers first, then from Ployz",
            )
            .long_about(
                "Remove an Environment. If anything of it ran, a removal Deployment \
                         takes it off the Servers first, deleting its deployed Volumes once \
                         each is accepted by name; then its configuration and history go. \
                         Type PROJECT/ENV with --confirm; without it the command fails with \
                         confirmation_required, naming what goes and the exact retry.",
            )
            .arg(positional("name", true))
            .arg(project_arg())
            .arg(
                value("confirm", None)
                    .value_name("PROJECT/ENV")
                    .help("PROJECT/ENV, typed to confirm the Environment's removal"),
            )
            .arg(crate::cli::volume_acceptance()),
        ))
        .subcommand(
            Command::new("branch")
                .about("Make a Branch: copies of the nodes picked, the rest used live")
                .arg(positional("name", true).help("Branch name, unique in the Project"))
                .arg(project_arg())
                .arg(
                    value("from", None).value_name("ENV").help(
                        "The Parent to branch from [default: the Project's Default Environment]",
                    ),
                )
                .arg(
                    repeated("copy")
                        .value_name("NODE")
                        .help("A Service or Volume to copy; repeatable"),
                )
                .arg(
                    repeated("live")
                        .value_name("NODE")
                        .help("A node the Branch must use live from its Parent; repeatable"),
                )
                .arg(
                    repeated("setup")
                        .value_name("SERVICE=COMMAND")
                        .help("Run COMMAND in a copied Service before it first deploys"),
                )
                .arg(switch("keep", None).help("Keep it after a Save"))
                .arg(value("fix", None).value_name("DEPLOYMENT").help(
                    "Fix this failed Deployment of the Parent: copies what it failed to apply",
                )),
        )
        .subcommand(
            moving(
                Command::new("save")
                    .about("Stage the Branch's changes in its Parent")
                    .long_about(
                        "Stage the Branch's changes in its Parent's Working State; nothing is \
                         published or deployed, and nothing in the Parent is deleted. --plan \
                         lists them and the version to pass back. A secret the Branch added \
                         moves only when picked `=from`. From a PR Environment it is a \
                         Conditional Save instead: the changes go live in the Destination \
                         with the pull request's merge; --withdraw withdraws it. --take ID \
                         stages a merged pull request's value its Conditional Save left \
                         beside the Environment's own edit (`ployz diff` lists them), even \
                         once its PR Environment is gone.",
                    ),
            )
            .arg(value("into", None).value_name("ENV").help(
                "From a PR Environment: the Destination, when several deploy its target branch",
            ))
            .arg(
                switch("withdraw", None)
                    .help("From a PR Environment: withdraw its Conditional Save")
                    .conflicts_with_all(["only", "plan", "version", "take"]),
            )
            .arg(
                value("take", None)
                    .value_name("ID")
                    .help("Stage the hints (or --only ROW) of this Conditional Save in --env")
                    .conflicts_with_all(["plan", "version", "into"]),
            ),
        )
        .subcommand(moving(Command::new("update").about(
            "Stage what the Branch's Parent deployed since, in the Branch",
        )))
        .subcommand(
            store::scoped(
                Command::new("copy").about(
                    "Make a Live Node the Branch's own copy; a Volume it mounts comes empty",
                ),
            )
            .arg(positional("node", true))
            .arg(expect()),
        )
        .subcommand(
            store::scoped(Command::new("keep").about("Keep the Branch after a Save and when idle"))
                .arg(switch("off", None).help("Stop keeping it")),
        )
        .subcommand(deploy::following(
            base(
                "shutdown",
                "Take an Environment off the Servers, keeping its configuration",
            )
            .long_about(
                "Take an Environment, such as a PR Environment, off the Servers: a removal \
                 Deployment stops its Services and deletes its deployed Volumes once each is \
                 accepted by name. Its configuration, history and Branch stay, and pushes \
                 leave it off; `ployz deploy --env NAME` turns it back on.",
            )
            .arg(positional("name", true))
            .arg(project_arg())
            .arg(crate::cli::volume_acceptance()),
        ))
        .subcommand(
            Command::new("pr")
                .about("Show or change PR Environments for a repository")
                .long_about(
                    "Show the Project's PR plans, or change one. With PR Environments on, \
                     each pull request of the repository gets a Branch of the start-from \
                     Environment named pr-NUMBER, with its own copies of the repository's \
                     Services tracking the pull request's branch, deployed on open and on \
                     each push. Only the flags given change.",
                )
                .arg(
                    positional("repository", false)
                        .help("The repository, like acme/app [default: the Project's only one]"),
                )
                .arg(project_arg())
                .arg(switch("on", None).help("Give its pull requests PR Environments"))
                .arg(
                    switch("off", None)
                        .conflicts_with("on")
                        .help("Stop making PR Environments"),
                )
                .arg(
                    value("from", None)
                        .value_name("ENV")
                        .help("The Environment each PR Environment is a Branch of"),
                )
                .arg(
                    repeated("copy")
                        .value_name("NODE")
                        .help("Also copy this Service or Volume; repeatable, replaces the list"),
                )
                .arg(repeated("setup").value_name("SERVICE=COMMAND").help(
                    "Run COMMAND in a copied Service before it first deploys; repeatable, \
                     replaces the list",
                ))
                .arg(
                    value("remove-on-close", None)
                        .value_name("BOOL")
                        .value_parser(clap::value_parser!(bool))
                        .help("Remove a PR Environment when its pull request closes"),
                )
                .arg(
                    value("bots", None)
                        .value_name("BOOL")
                        .value_parser(clap::value_parser!(bool))
                        .help("Make PR Environments for bots' pull requests too"),
                ),
        )
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "new" => new,
        "ls" => ls,
        "default" => default,
        "rm" => rm,
        "branch" => branch::branch,
        "save" => branch::save,
        "update" => branch::update,
        "copy" => branch::copy,
        "keep" => branch::keep,
        "shutdown" => pr::shutdown,
        "pr" => pr::pr,
        _ => return None,
    })
}

fn new(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let store = store(root)?;
    let create = CreateEnvironment {
        id: EnvironmentId::parse(mint())?,
        project: project(matches)?,
        name,
    };
    let words = ["env", "new", create.name.as_str()];
    let created = store.write(&create).map_err(failed(matches, &words))?;
    crate::output::finish(&created, || {
        say!(
            "Created Environment {} in Project {}.",
            created.environment.name,
            created.environment.project
        );
    })
}

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let listed = store(root)?
        .read(&EnvironmentsQuery {
            project: project(matches)?,
        })
        .map_err(failed(matches, &["env", "ls"]))?;
    crate::output::finish(&listed, || print_environments(&listed))
}

fn default(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let set = SetDefaultEnvironment {
        environment: EnvironmentRef {
            project: project(matches)?,
            environment: Some(name),
        },
    };
    let listed = store(root)?
        .write(&set)
        .map_err(failed(matches, &["env", "default"]))?;
    crate::output::finish(&listed, || print_environments(&listed))
}

fn print_environments(listed: &EnvironmentsView) {
    say!("Environments of Project {}:", listed.project.name);
    for environment in &listed.environments {
        let mut notes = Vec::new();
        if environment.default {
            notes.push("default".to_owned());
        }
        if let Some(parent) = &environment.parent {
            notes.push(format!("branch of {parent}"));
        }
        if let Some(removal) = &environment.removal {
            notes.push(format!(
                "removal #{} {}",
                removal.number,
                store::word(&removal.status)
            ));
        }
        match notes.is_empty() {
            true => say!("  {}", environment.name),
            false => say!("  {} ({})", environment.name, notes.join(", ")),
        }
    }
}

/// What `env rm` removed, and the Deployment that took it off the Servers.
#[derive(serde::Serialize)]
struct Removal<'a> {
    #[serde(flatten)]
    removed: &'a EnvironmentRemoved,
    deployment: Option<&'a DeploymentSummary>,
}

/// Remove an Environment in one path: if anything of it ran, a removal Deployment
/// takes it off the Servers (under the destructive review), then the Store deletes
/// it. A removal that didn't apply leaves the Environment, and running this again
/// retries it.
fn rm(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let at = EnvironmentRef {
        project: project(matches)?,
        environment: Some(name.clone()),
    };
    let store = store(root)?;
    let words = ["env", "rm", name.as_str()];
    let inventory = inventory(matches, &store, &at)?;
    // Typed where it is: PROJECT/ENV.
    let typed = format!("{}/{name}", inventory.environment.project);
    let mut again = vec!["env", "rm", name.as_str(), "--confirm", typed.as_str()];
    if !confirmed(matches, &typed, "Environment")? {
        return Err(unconfirmed(matches, inventory, &again));
    }
    let events = deploy::open_events(matches)?;
    let remove = RemoveEnvironment {
        environment: at.clone(),
    };
    match store.write(&remove) {
        Ok(removed) => return finish_removal(&removed, None),
        Err(StoreCallError::Refused(error))
            if error.details.get("deployed") == Some(&serde_json::Value::Bool(true)) => {}
        Err(error) => return Err(failed(matches, &words)(error)),
    }
    let accept = accepted(matches)?;
    let (view, ran) = take_off(matches, &store, &at, &accept, events, &words, &again)?;
    if view.deployment.status != DeploymentStatus::Applied {
        // Not removed yet: queued, failed, cancelled, or its outcome is unknown. This
        // same command finishes it once the removal applied, or queues it again.
        again.extend(
            accept
                .iter()
                .flat_map(|name| ["--accept-volume-loss", name.as_str()]),
        );
        return unfinished(matches, &view, ran, &again);
    }
    let removed = store.write(&remove).map_err(failed(matches, &words))?;
    finish_removal(&removed, Some(&view.deployment))
}

/// Refuse an unconfirmed `env rm`, naming what goes and the exact retry.
fn unconfirmed(matches: &ArgMatches, inventory: Inventory, again: &[&str]) -> Error {
    let retry = store::next(matches, again);
    Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Removing Environment {} deletes its configuration, history and every Service \
             and Volume in it; this can't be undone. No changes made.\nRetry: {retry}",
            inventory.environment.name
        ),
        json!({
            "environment": inventory.environment,
            "services": inventory.services,
            "volumes": inventory.volumes,
            "next": retry,
        }),
    )
}

fn finish_removal(
    removed: &EnvironmentRemoved,
    deployment: Option<&DeploymentSummary>,
) -> Result<(), Error> {
    let removal = Removal {
        removed,
        deployment,
    };
    crate::output::finish(&removal, || {
        let environment = &removed.environment;
        if let Some(deployment) = deployment {
            say!(
                "Removed {}/{} from the Servers (Deployment #{}).",
                environment.project,
                environment.name,
                deployment.number
            );
        }
        say!(
            "Removed Environment {}/{}.",
            environment.project,
            environment.name
        );
    })
}

/// Nodes by name: `SERVICE`, or `volumes.VOLUME`.
fn node_names(names: &[String]) -> Result<Vec<ployz_store::NodeName>, Error> {
    Ok(names
        .iter()
        .map(|name| ployz_store::NodeName::parse(name.as_str()))
        .collect::<Result<_, _>>()?)
}

/// Items as one line of text.
fn joined<T: ToString>(items: &[T]) -> String {
    items
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
