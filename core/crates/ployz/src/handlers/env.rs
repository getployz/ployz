//! `ployz env`: Environments in the Config Store, and Branches of them. `ls`,
//! `default` and `rm` manage them; `rm` is the one teardown path. A Branch
//! copies the Services and Volumes picked (and what they use that its Parent
//! doesn't run) and uses the rest live; `update` stages what its Parent deployed
//! since, `copy` turns a Live Node into its own copy, and `keep` keeps it after a Save.

use clap::{ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_core::RpcErrorCode;
use ployz_store::{
    Admit, Branched, CopyNode, CreateBranch, CreateEnvironment, DeploymentId, DeploymentStatus,
    DeploymentSummary, EnvironmentId, EnvironmentName, EnvironmentRef, EnvironmentRemoved,
    EnvironmentsQuery, EnvironmentsView, KeepBranch, RemoveEnvironment, ServicesQuery,
    SetDefaultEnvironment, SetupCommand, UpdateBranch, VolumeName, VolumesQuery,
};
use serde_json::json;

use super::config::{expect, expected};
use super::deploy;
use super::store::{self, Next, Store, failed, mint, project, project_arg, store};
use super::{Error, leaf_matches, required};
use crate::cli::{positional, repeated, switch, value};
use crate::cloud_account::StoreCallError;
use crate::failure::USAGE_EXIT;
use crate::output::say;

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
        .subcommand(
            deploy::following(
                Command::new("rm")
                    .about("Remove an Environment: from the Servers first, then from Ployz")
                    .long_about(
                        "Remove an Environment. If anything of it ran, a removal Deployment \
                         takes it off the Servers first, deleting its deployed Volumes once \
                         each is accepted by name; then its configuration and history go. \
                         Type its name with --confirm; without it the command fails with \
                         confirmation_required, naming what goes and the exact retry.",
                    )
                    .arg(positional("name", true))
                    .arg(project_arg())
                    .arg(
                        value("confirm", None)
                            .value_name("ENV")
                            .help("The Environment's name, typed to confirm its removal"),
                    )
                    .arg(crate::cli::volume_acceptance()),
            ),
        )
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
            store::scoped(
                Command::new("update")
                    .about("Stage what the Branch's Parent deployed since, in the Branch"),
            )
            .arg(expect()),
        )
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
}

pub(super) fn handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "new" => (new, Supported),
        "ls" => (ls, Supported),
        "default" => (default, Supported),
        "rm" => (rm, Supported),
        "branch" => (branch, Supported),
        "update" => (update, Supported),
        "copy" => (copy, Supported),
        "keep" => (keep, Supported),
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
    let created = store
        .create_environment(&create)
        .map_err(failed(matches, &words))?;
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
        .environments(&EnvironmentsQuery {
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
        .set_default_environment(&set)
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
            let status = serde_json::to_value(removal.status).unwrap_or_default();
            notes.push(format!(
                "removal #{} {}",
                removal.number,
                status.as_str().unwrap_or_default()
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
    let mut again = vec!["env", "rm", name.as_str(), "--confirm", name.as_str()];
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name.as_str() => {}
        Some(typed) => {
            return Err(Error::usage(format!(
                "--confirm {} does not match Environment {name}. No changes made.",
                typed.escape_debug()
            ))
            .with_exit(USAGE_EXIT));
        }
        None => return Err(unconfirmed(matches, &store, &at, &again)?),
    }
    let events = deploy::open_events(matches)?;
    let remove = RemoveEnvironment {
        environment: at.clone(),
    };
    match store.remove_environment(&remove) {
        Ok(removed) => return finish_removal(&removed, None),
        Err(StoreCallError::Refused(error))
            if error.details.get("deployed") == Some(&serde_json::Value::Bool(true)) => {}
        Err(error) => return Err(failed(matches, &words)(error)),
    }
    let accept = matches
        .get_many::<String>("accept-volume-loss")
        .into_iter()
        .flatten()
        .map(|name| VolumeName::parse(name.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    // The in-process Store trusts this CLI to observe the Servers; Cloud observes them itself.
    let volumes = match store.local() {
        Some(_) => deploy::observe(matches, &store, &at, true)?,
        None => None,
    };
    let admitted = store
        .admit(
            &Admit {
                id: DeploymentId::parse(mint())?,
                environment: at,
                services: Vec::new(),
                version: None,
                upload: None,
                retry: None,
                remove: true,
                accept_volume_loss: accept.clone(),
            },
            volumes,
        )
        .map_err(|error| failed(matches, &words)(deploy::accepting(error, matches, &again)))?;
    let (view, ran, _) = deploy::execute(matches, &store, &admitted, None, events, &words)?;
    if view.deployment.status != DeploymentStatus::Applied {
        // Not removed yet: queued, failed, cancelled, or its outcome is unknown. This
        // same command finishes it once the removal applied, or queues it again.
        again.extend(
            accept
                .iter()
                .flat_map(|name| ["--accept-volume-loss", name.as_str()]),
        );
        deploy::finish_view(&view, Some(store::next(matches, &again)))?;
        return ran.and_then(|()| match matches.get_flag("detach") {
            true => Ok(()),
            false => Err(Error::partial()),
        });
    }
    let removed = store
        .remove_environment(&remove)
        .map_err(failed(matches, &words))?;
    finish_removal(&removed, Some(&view.deployment))
}

/// Refuse an unconfirmed `env rm`, naming what goes and the exact retry.
fn unconfirmed(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
    again: &[&str],
) -> Result<Error, Error> {
    let words = ["env", "rm"];
    let services = store
        .services(&ServicesQuery {
            environment: at.clone(),
        })
        .map_err(failed(matches, &words))?;
    let volumes = store
        .volumes(&VolumesQuery {
            environment: at.clone(),
        })
        .map_err(failed(matches, &words))?;
    let retry = store::next(matches, again);
    let name = &services.environment.name;
    Ok(Error::detailed(
        RpcErrorCode::ConfirmationRequired,
        format!(
            "Removing Environment {name} deletes its configuration, history and every Service \
             and Volume in it; this can't be undone. No changes made.\nRetry: {retry}"
        ),
        json!({
            "environment": services.environment,
            "services": services.services.iter().map(|listing| &listing.service.name).collect::<Vec<_>>(),
            "volumes": volumes.volumes.iter().map(|listing| json!({
                "name": listing.volume.name,
                "deployed": listing.deployed,
            })).collect::<Vec<_>>(),
            "next": retry,
        }),
    ))
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
        say!("Removed Environment {}/{}.", environment.project, environment.name);
    })
}

fn branch(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let from = matches
        .get_one::<String>("from")
        .map(|from| EnvironmentName::parse(from.as_str()))
        .transpose()?;
    let nodes = |flag: &str| -> Vec<String> {
        matches
            .get_many::<String>(flag)
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    };
    let setup = nodes("setup")
        .iter()
        .map(|setup| {
            setup
                .split_once('=')
                .and_then(|(service, command)| {
                    Some(SetupCommand {
                        service: ServiceName::parse(service).ok()?,
                        command: command.to_owned(),
                    })
                })
                .ok_or_else(|| {
                    Error::usage("Expected --setup SERVICE=COMMAND, like web='pnpm db:seed'")
                        .with_exit(USAGE_EXIT)
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let fix = matches
        .get_one::<String>("fix")
        .map(|id| {
            DeploymentId::parse(id.as_str()).map_err(|_| {
                Error::usage("Expected --fix DEPLOYMENT to be a Deployment ID")
                    .with_exit(USAGE_EXIT)
            })
        })
        .transpose()?;
    let create = CreateBranch {
        id: EnvironmentId::parse(mint())?,
        from: EnvironmentRef {
            project: project(matches)?,
            environment: from,
        },
        name,
        copy: nodes("copy"),
        live: nodes("live"),
        setup,
        keep: matches.get_flag("keep"),
        fix,
    };
    let mut words = vec!["env", "branch", create.name.as_str()];
    if let Some(from) = &create.from.environment {
        words.extend(["--from", from.as_str()]);
    }
    let made = store(root)?
        .create_branch(&create)
        .map_err(failed(matches, &words))?;
    let deploy = store::next(matches, &["deploy", "--env", create.name.as_str()]);
    finish(&made, Some(deploy), "Made Branch")
}

fn update(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let update = UpdateBranch {
        environment: store::environment(matches)?,
        expect: expected(matches)?,
    };
    let updated = store(root)?
        .update_branch(&update)
        .map_err(|error| stale(error, matches))
        .map_err(failed(matches, &["env", "update"]))?;
    finish(
        &updated,
        Some(store::next(matches, &["deploy"])),
        "Updated Branch",
    )
}

fn copy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let node = ServiceName::parse(required(matches, "node")?)?;
    let copy = CopyNode {
        environment: store::environment(matches)?,
        node,
        expect: expected(matches)?,
    };
    let words = ["env", "copy", copy.node.as_str()];
    let copied = store(root)?
        .copy_node(&copy)
        .map_err(|error| stale(error, matches))
        .map_err(failed(matches, &words))?;
    finish(
        &copied,
        Some(store::next(matches, &["deploy"])),
        "Copied into Branch",
    )
}

fn keep(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let keep = KeepBranch {
        environment: store::environment(matches)?,
        kept: !matches.get_flag("off"),
    };
    let kept = store(root)?
        .keep_branch(&keep)
        .map_err(failed(matches, &["env", "keep"]))?;
    let what = if keep.kept {
        "Keeping Branch"
    } else {
        "Not keeping Branch"
    };
    finish(&kept, None, what)
}

/// A refused `--expect` names the read that shows the fresh revision; other
/// conflicts keep the Store's own next step.
fn stale(error: StoreCallError, matches: &ArgMatches) -> StoreCallError {
    if matches.get_one::<String>("expect").is_some() {
        store::with_refresh_hint(error, matches, "get")
    } else {
        error
    }
}

/// A Branch after a change, and `deploy` when it staged something.
fn finish(result: &Branched, deploy: Option<String>, what: &str) -> Result<(), Error> {
    let next = deploy.filter(|_| !result.staged.is_empty());
    crate::output::finish(&Next::new(result, next), || {
        let branch = &result.branch;
        say!(
            "{what} {}/{} of {}.",
            branch.environment.project,
            branch.environment.name,
            branch.parent
        );
        if !result.staged.is_empty() {
            say!("Staged: {}", result.staged.join(", "));
        }
        for live in &branch.live {
            match &live.owner {
                Some(owner) => say!("Uses {} live from {owner}.", live.name),
                None => say!("Uses {} live, but nothing runs it.", live.name),
            }
        }
        if !branch.update.is_empty() {
            say!("Its Parent deployed changes: ployz env update.");
        }
    })
}
