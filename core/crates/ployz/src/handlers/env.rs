//! `ployz env`: Environments in the Config Store, and Branches of them. `ls`,
//! `default` and `rm` manage them; `rm` is the one teardown path. A Branch
//! copies the Services and Volumes picked (and what they use that its Parent
//! doesn't run) and uses the rest live; `update` stages what its Parent deployed
//! since, `copy` turns a Live Node into its own copy, and `keep` keeps it after a Save.

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_core::ServiceName;
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
    let again = vec!["env", "rm", name.as_str(), "--confirm", name.as_str()];
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
    let remove = RemoveEnvironment {
        environment: at.clone(),
    };
    match store.remove_environment(&remove) {
        Ok(removed) => return finish_removal(&removed, None),
        Err(StoreCallError::Refused(error))
            if error.details.get("deployed") == Some(&serde_json::Value::Bool(true)) => {}
        Err(error) => return Err(failed(matches, &words)(error)),
    }
    let Some(view) = take_off(matches, &store, at, &words, &again)? else {
        return Ok(());
    };
    let removed = store
        .remove_environment(&remove)
        .map_err(failed(matches, &words))?;
    finish_removal(&removed, Some(&view.deployment))
}

/// Take an Environment off the Servers with a removal Deployment, under the
/// destructive review, and follow it. None once it reported a removal that didn't
/// apply (queued, failed, cancelled or unknown), whose retry is `again`.
fn take_off(
    matches: &ArgMatches,
    store: &Store,
    at: EnvironmentRef,
    words: &[&str],
    again: &[&str],
) -> Result<Option<ployz_store::DeploymentView>, Error> {
    let accept = matches
        .get_many::<String>("accept-volume-loss")
        .into_iter()
        .flatten()
        .map(|name| VolumeName::parse(name.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    let events = deploy::open_events(matches)?;
    // The in-process Store trusts this CLI to observe the Servers; Cloud observes them itself.
    let volumes = match store.local() {
        Some(_) => deploy::observe(matches, store, &at, true)?,
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
        .map_err(|error| failed(matches, words)(deploy::accepting(error, matches, again)))?;
    let deploy::Shipped { view, ran, .. } =
        deploy::execute(matches, store, &admitted, None, events, words)?;
    if view.deployment.status != DeploymentStatus::Applied {
        // Not removed yet: queued, failed, cancelled, or its outcome is unknown. This
        // same command finishes it once the removal applied, or queues it again.
        let mut again: Vec<&str> = again.to_vec();
        again.extend(
            accept
                .iter()
                .flat_map(|name| ["--accept-volume-loss", name.as_str()]),
        );
        deploy::finish_view(&view, Some(store::next(matches, &again)))?;
        return ran.and_then(|()| match matches.get_flag("detach") {
            true => Ok(None),
            false => Err(Error::partial()),
        });
    }
    Ok(Some(view))
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
    let Some(view) = take_off(matches, &store, at, &words, &words)? else {
        return Ok(());
    };
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
