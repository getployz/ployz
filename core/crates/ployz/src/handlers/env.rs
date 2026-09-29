//! `ployz env`: Environments in the Config Store, and Branches of them. `ls`,
//! `default` and `rm` manage them; `rm` is the one teardown path. A Branch
//! copies the Services and Volumes picked (and what they use that its Parent
//! doesn't run) and uses the rest live; `save` stages its changes in its Parent,
//! `update` stages what its Parent deployed since, `copy` turns a Live Node into
//! its own copy, and `keep` keeps it after a Save.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_core::ServiceName;
use ployz_store::{
    Admit, Branched, ConditionalSaveId, CopyNode, CreateBranch, CreateEnvironment, DeploymentId,
    DeploymentStatus, DeploymentSummary, DeploymentView, EnvironmentId, EnvironmentName,
    EnvironmentRef, EnvironmentRemoved, EnvironmentSummary, EnvironmentsQuery, EnvironmentsView,
    KeepBranch, Move, MovePick, MoveQuery, MoveView, Moved, PickChoice, RemoveEnvironment, Save,
    SaveState, ServicesQuery, SetDefaultEnvironment, SetupCommand, Take, Update, VolumeName,
    VolumesQuery, When,
};
use serde_json::json;

use super::config::{expect, expected};
use super::deploy;
use super::store::{self, Next, Store, failed, mint, project, project_arg, store};
use super::{Error, leaf_matches, required};
use crate::cli::{base, positional, repeated, switch, value};
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

pub(super) fn handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "new" => (new, Supported),
        "ls" => (ls, Supported),
        "default" => (default, Supported),
        "rm" => (rm, Supported),
        "branch" => (branch, Supported),
        "save" => (save, Supported),
        "update" => (update, Supported),
        "copy" => (copy, Supported),
        "keep" => (keep, Supported),
        "shutdown" => (shutdown, Supported),
        "pr" => (pr, Supported),
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
    match store.remove_environment(&remove) {
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
    let removed = store
        .remove_environment(&remove)
        .map_err(failed(matches, &words))?;
    finish_removal(&removed, Some(&view.deployment))
}

/// Whether `--confirm` typed `name`; a different name is a usage error.
pub(super) fn confirmed(matches: &ArgMatches, name: &str, what: &str) -> Result<bool, Error> {
    match matches.get_one::<String>("confirm") {
        Some(typed) if typed == name => Ok(true),
        Some(typed) => Err(Error::usage(format!(
            "--confirm {} does not match {what} {name}. No changes made.",
            typed.escape_debug()
        ))
        .with_exit(USAGE_EXIT)),
        None => Ok(false),
    }
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

/// What removing an Environment deletes: its Services and Volumes, by name.
#[derive(serde::Serialize)]
pub(super) struct Inventory {
    environment: EnvironmentSummary,
    services: Vec<ServiceName>,
    volumes: Vec<serde_json::Value>,
}

pub(super) fn inventory(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
) -> Result<Inventory, Error> {
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
    Ok(Inventory {
        environment: services.environment,
        services: services
            .services
            .into_iter()
            .map(|listing| listing.service.name)
            .collect(),
        volumes: volumes
            .volumes
            .iter()
            .map(|listing| json!({ "name": listing.volume.name, "deployed": listing.deployed }))
            .collect(),
    })
}

/// Every `--accept-volume-loss` name.
pub(super) fn accepted(matches: &ArgMatches) -> Result<Vec<VolumeName>, Error> {
    matches
        .get_many::<String>("accept-volume-loss")
        .into_iter()
        .flatten()
        .map(|name| {
            VolumeName::parse(name.as_str()).map_err(|_| {
                Error::usage("Expected Volume names: lowercase letters, digits and -")
                    .with_exit(USAGE_EXIT)
            })
        })
        .collect()
}

/// Take Environment `at` off the Servers: admit a removal Deployment under the
/// destructive review, accepting the loss of `accept`, then run or follow it as
/// `deploy` does. `again` is the command that retries the whole removal.
pub(super) fn take_off(
    matches: &ArgMatches,
    store: &Store,
    at: &EnvironmentRef,
    accept: &[VolumeName],
    events: Option<std::io::BufWriter<std::fs::File>>,
    words: &[&str],
    again: &[&str],
) -> Result<(DeploymentView, Result<(), Error>), Error> {
    // The in-process Store trusts this CLI to observe the Servers; Cloud observes them itself.
    let volumes = match store.local() {
        Some(_) => deploy::observe(matches, store, at, true)?,
        None => None,
    };
    let admitted = store
        .admit(
            &Admit {
                id: DeploymentId::parse(mint())?,
                environment: at.clone(),
                services: Vec::new(),
                version: None,
                upload: None,
                retry: None,
                remove: true,
                accept_volume_loss: accept.to_vec(),
            },
            volumes,
        )
        .map_err(|error| failed(matches, words)(deploy::accepting(error, matches, again)))?;
    let deploy::Shipped { view, ran, .. } =
        deploy::execute(matches, store, &admitted, None, events, words)?;
    Ok((view, ran))
}

/// Report a removal Deployment that didn't apply (yet), naming `again` to finish
/// it: exit 3, or 0 when `--detach` asked not to wait.
pub(super) fn unfinished(
    matches: &ArgMatches,
    view: &DeploymentView,
    ran: Result<(), Error>,
    again: &[&str],
) -> Result<(), Error> {
    deploy::finish_view(view, Some(store::next(matches, again)))?;
    ran.and_then(|()| match matches.get_flag("detach") {
        true => Ok(()),
        false => Err(Error::partial()),
    })
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
    let setup = setups(&nodes("setup"))?;
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

/// `env save` and `env update`: the Branch in scope, the changes picked, the guard.
fn moving(command: Command) -> Command {
    store::scoped(command)
        .arg(
            repeated("only")
                .value_name("ROW[=CHOICE]")
                .help("Move only this change, or every change under it (web, web.env); a variable may say how it lands: from, parent or leave_out"),
        )
        .arg(value("version", None).help("Refuse unless this is still the version --plan showed"))
        .arg(switch("plan", None).help("List the changes and the version; move nothing"))
}

fn save(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let here = store::environment(matches)?;
    if matches.get_one::<String>("take").is_some() {
        return shift(root, "take", (EnvironmentRef::default(), Some(here)));
    }
    let into = matches
        .get_one::<String>("into")
        .map(|name| {
            Ok::<_, Error>(EnvironmentRef {
                project: project(matches)?,
                environment: Some(EnvironmentName::parse(name.as_str())?),
            })
        })
        .transpose()?;
    let verb = match matches.get_flag("withdraw") {
        true => "withdraw",
        false => "save",
    };
    shift(root, verb, (here, into))
}

fn update(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let into = Some(store::environment(matches)?);
    shift(root, "update", (EnvironmentRef::default(), into))
}

/// A Save, Update, withdrawal or take: with `--plan` its changes, else the Move itself.
fn shift(
    root: &ArgMatches,
    verb: &str,
    (from, into): (EnvironmentRef, Option<EnvironmentRef>),
) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let command = match verb {
        "update" => "update",
        _ => "save",
    };
    let words = ["env", command];
    if matches.get_flag("plan") {
        let sides = match verb {
            "update" | "take" => MoveQuery::Update {
                into: into.unwrap_or_default(),
            },
            _ => MoveQuery::Save {
                from,
                into,
                when: None,
            },
        };
        let view = store.move_view(&sides).map_err(failed(matches, &words))?;
        return plan(matches, command, &view);
    }
    let picks = match verb {
        "withdraw" => Some(Vec::new()),
        _ => matches
            .get_many::<String>("only")
            .map(|only| only.map(|only| pick(only)).collect::<Result<Vec<_>, _>>())
            .transpose()?,
    };
    let version = matches.get_one::<String>("version").cloned();
    let request = match verb {
        "take" => {
            let save = matches
                .try_get_one::<String>("take")
                .ok()
                .flatten()
                .map(|id| ConditionalSaveId::parse(id.as_str()))
                .transpose()?
                .ok_or_else(|| Error::usage("Name the Conditional Save to take from"))?;
            Move::Take(Take {
                from: save,
                into,
                rows: picks.map(|picks| picks.into_iter().map(|pick| pick.row).collect()),
                version,
            })
        }
        "update" => Move::Update(Update {
            into: into.unwrap_or_default(),
            picks,
            version,
        }),
        _ => Move::Save(Save {
            from,
            into,
            picks,
            version,
            when: (verb == "withdraw").then_some(When::AtMerge),
        }),
    };
    let moved = store
        .move_changes(&request)
        .map_err(|error| failed(matches, &words)(reviewed(error, matches, command)))?;
    moved_out(matches, verb, &moved)
}

/// `--only ROW[=CHOICE]`.
fn pick(only: &str) -> Result<MovePick, Error> {
    let (row, choice) = match only.split_once('=') {
        Some((row, choice)) => {
            let choice: PickChoice = serde_json::from_value(json!(choice)).map_err(|_| {
                Error::usage(format!(
                    "Expected --only {row}=CHOICE with from, parent or leave_out"
                ))
                .with_exit(USAGE_EXIT)
            })?;
            (row, Some(choice))
        }
        None => (only, None),
    };
    Ok(MovePick {
        row: row.to_owned(),
        choice,
    })
}

/// A stale version names the read that shows the changes again.
fn reviewed(error: StoreCallError, matches: &ArgMatches, verb: &str) -> StoreCallError {
    let StoreCallError::Refused(mut error) = error else {
        return error;
    };
    if let Some(details) = error.details.as_object_mut()
        && details.contains_key("version")
    {
        details.insert(
            "next".into(),
            json!(store::next(matches, &["env", verb, "--plan"])),
        );
    }
    StoreCallError::Refused(error)
}

fn plan(matches: &ArgMatches, verb: &str, view: &MoveView) -> Result<(), Error> {
    let next = (!view.rows.is_empty())
        .then(|| store::next(matches, &["env", verb, "--version", view.version.as_str()]));
    crate::output::finish(&Next::new(view, next), || {
        say!(
            "{} → {} (version {}):",
            view.from.name,
            view.into.name,
            view.version
        );
        if view.rows.is_empty() {
            say!("  nothing to move");
        }
        for row in &view.rows {
            let mut notes = Vec::new();
            if row.conflict {
                notes.push(format!("{} changed it too", view.into.name));
            }
            if let Some(choice) = &row.choice {
                let default = serde_json::to_value(choice.default).unwrap_or_default();
                notes.push(format!("lands as {}", default.as_str().unwrap_or_default()));
            }
            let notes = match notes.is_empty() {
                true => String::new(),
                false => format!(" ({})", notes.join(", ")),
            };
            say!("  {}: {} → {}{notes}", row.row, row.into, row.from);
        }
    })
}

/// What a Move did: `deploy` of where it landed, and after a Save of a Branch not
/// kept, the command that closes it. A Conditional Save stages nothing.
fn moved_out(matches: &ArgMatches, verb: &str, moved: &Moved) -> Result<(), Error> {
    #[derive(serde::Serialize)]
    struct Out<'a> {
        #[serde(flatten)]
        moved: &'a Moved,
        #[serde(skip_serializing_if = "Option::is_none")]
        next: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        close: Option<String>,
    }
    let scoped = |words: &[&str]| {
        let mut words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
        if let Ok(Some(project)) = matches.try_get_one::<String>("project") {
            words.extend(["--project".to_owned(), project.clone()]);
        }
        shell_words::join(std::iter::once("ployz".to_owned()).chain(words))
    };
    let into: &EnvironmentSummary = &moved.into;
    let at_merge = moved
        .conditional_save
        .as_ref()
        .filter(|save| save.state == SaveState::Standing);
    let close = match (&moved.branch, verb, at_merge) {
        (Some(branch), "save", None) if !branch.kept => {
            let name = branch.environment.name.as_str();
            let typed = format!("{}/{name}", branch.environment.project);
            Some(scoped(&["env", "rm", name, "--confirm", &typed]))
        }
        _ => None,
    };
    let out = Out {
        moved,
        next: (!moved.staged.is_empty()).then(|| scoped(&["deploy", "--env", into.name.as_str()])),
        close,
    };
    crate::output::finish(&out, || {
        let (from, into) = (&moved.from.name, format!("{}/{}", into.project, into.name));
        match (verb, at_merge, &moved.conditional_save) {
            ("withdraw", ..) => say!("Withdrew {from}'s Conditional Save into {into}."),
            (_, Some(save), _) => say!(
                "Saved for PR #{}'s merge into {into}: {}.",
                save.pull_request,
                save.rows.join(", ")
            ),
            ("take", _, Some(save)) => {
                say!("Took PR #{}'s value into {into}.", save.pull_request);
            }
            _ => say!("Moved {from} → {into}."),
        }
        if !moved.staged.is_empty() {
            say!("Staged: {}", moved.staged.join(", "));
        }
        if let Some(close) = &out.close {
            say!("Close the Branch when done: {close}");
        }
    })
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

/// `--setup SERVICE=COMMAND` values.
fn setups(values: &[String]) -> Result<Vec<SetupCommand>, Error> {
    values
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
        .collect()
}

/// Take an Environment off the Servers and keep everything else of it.
fn shutdown(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let at = EnvironmentRef {
        project: project(matches)?,
        environment: Some(name.clone()),
    };
    let store = store(root)?;
    let words = ["env", "shutdown", name.as_str()];
    let accept = accepted(matches)?;
    let events = deploy::open_events(matches)?;
    let (view, ran) = take_off(matches, &store, &at, &accept, events, &words, &words)?;
    if view.deployment.status != DeploymentStatus::Applied {
        let mut again: Vec<&str> = words.to_vec();
        again.extend(
            accept
                .iter()
                .flat_map(|name| ["--accept-volume-loss", name.as_str()]),
        );
        return unfinished(matches, &view, ran, &again);
    }
    let on = store::next(matches, &["deploy", "--env", name.as_str()]);
    deploy::finish_view(&view, Some(on))
}

/// Show the Project's PR plans, or change one.
fn pr(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let list = |flag: &str| -> Option<Vec<String>> {
        matches
            .get_many::<String>(flag)
            .map(|values| values.cloned().collect())
    };
    let enabled = match (matches.get_flag("on"), matches.get_flag("off")) {
        (true, _) => Some(true),
        (_, true) => Some(false),
        _ => None,
    };
    let start_from = matches
        .get_one::<String>("from")
        .map(|from| EnvironmentName::parse(from.as_str()))
        .transpose()?;
    let setup = list("setup").map(|values| setups(&values)).transpose()?;
    let mut set = ployz_store::SetPrPlan {
        project: project(matches)?,
        repository: matches
            .get_one::<String>("repository")
            .cloned()
            .unwrap_or_default(),
        enabled,
        start_from,
        copy: list("copy"),
        setup,
        remove_on_close: matches.get_one::<bool>("remove-on-close").copied(),
        include_bots: matches.get_one::<bool>("bots").copied(),
    };
    let store = store(root)?;
    let words = ["env", "pr"];
    let query = ployz_store::PrPlansQuery {
        project: set.project.clone(),
    };
    let changing = set.enabled.is_some()
        || set.start_from.is_some()
        || set.copy.is_some()
        || set.setup.is_some()
        || set.remove_on_close.is_some()
        || set.include_bots.is_some();
    let view = match changing {
        false => store.pr_plans(&query).map_err(failed(matches, &words))?,
        true => {
            if set.repository.is_empty() {
                let plans = store.pr_plans(&query).map_err(failed(matches, &words))?;
                match plans.plans.as_slice() {
                    [only] => set.repository.clone_from(&only.repository),
                    _ => {
                        return Err(Error::usage(
                            "Name the repository: `ployz env pr` lists the Project's",
                        )
                        .with_exit(USAGE_EXIT));
                    }
                }
            }
            store.set_pr_plan(&set).map_err(failed(matches, &words))?
        }
    };
    let mut json = serde_json::to_value(&view).expect("PR plans are JSON");
    if let (true, Some(fields)) = (changing, json.as_object_mut()) {
        fields.insert("immediate".to_owned(), json!(true));
    }
    crate::output::finish(&json, || {
        say!("PR Environments of Project {}:", view.project.name);
        if view.plans.is_empty() {
            say!("  No Service deploys from a GitHub repository through the GitHub App.");
        }
        for plan in &view.plans {
            let mut words = vec![if plan.enabled { "on" } else { "off" }.to_owned()];
            match &plan.start_from {
                Some(from) => words.push(format!("from {from}")),
                None if plan.enabled => words.push("pick --from to start".to_owned()),
                None => {}
            }
            if !plan.copy.is_empty() {
                words.push(format!("also copies {}", plan.copy.join(", ")));
            }
            for setup in &plan.setup {
                words.push(format!("then {}: {}", setup.service, setup.command));
            }
            if !plan.remove_on_close {
                words.push("kept after close".to_owned());
            }
            if plan.include_bots {
                words.push("bots too".to_owned());
            }
            say!("  {}: {}", plan.repository, words.join(" · "));
        }
    })
}
