//! `ployz env`: Environments in the Config Store, and Branches of them. `ls`,
//! `default` and `rm` manage them; `rm` is the one teardown path. A Branch
//! copies the Services and Volumes picked (and what they use that its Parent
//! doesn't run) and uses the rest live; `sync` stages one Environment's changes in
//! another, `copy` turns a Live Node into its own copy, and `keep` keeps it after
//! syncing into its Parent.
//! `never-sync` marks settings an Environment keeps as its own.

mod branch;
mod pr;

use clap::{ArgMatches, Command};
use ployz_core::RpcErrorCode;
use ployz_store::{
    CreateEnvironment, DeploymentSummary, EnvironmentId, EnvironmentName, EnvironmentRef,
    EnvironmentRemoved, EnvironmentsQuery, EnvironmentsView, NeverSync, RemoveEnvironment,
    SetDefaultEnvironment,
};
use serde_json::json;

use super::config::expect;
use super::deploy;
use super::store::{self, mint, project, project_arg, store};
use super::teardown::{Inventory, confirmed, inventory, remove_all};
use super::{Error, leaf_matches, required};
use crate::cli::{base, positional, repeated, switch, value};
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
            store::scoped(Command::new("setup").about(
                "Set the Setup Commands a new Branch of an Environment runs when it names none",
            ))
            .arg(
                repeated("setup")
                    .value_name("SERVICE=COMMAND")
                    .required_unless_present("clear")
                    .help("Run COMMAND in the Branch's copy of SERVICE; repeatable"),
            )
            .arg(
                switch("clear", None)
                    .conflicts_with("setup")
                    .help("Run none by default"),
            ),
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
            .arg(crate::cli::volume_acceptance())
            .arg(crate::cli::reviewed_version()),
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
                .arg(switch("keep", None).help("Keep it after syncing into its Parent"))
                .arg(value("fix", None).value_name("DEPLOYMENT").help(
                    "Fix this failed Deployment of the Parent: copies what it failed to apply",
                )),
        )
        .subcommand(
            store::scoped(
                Command::new("sync")
                    .about("Stage one Environment's changes in another of the Project")
                    .long_about(
                        "Stage the Environment's changes in another Environment of the \
                         Project (--to), or another's in it (--from), as the receiver's \
                         changes to deploy: the sender's Working State, deployed or not. \
                         Nothing is deleted, published or deployed, and sizing, domains, \
                         generated addresses, the Git branch and Volume data stay each \
                         Environment's own. --plan lists the changes and the version to pass \
                         back. A change left out is offered again next time, as is one the \
                         receiver discards before it deploys. A change the receiver made too \
                         since the two last shared is overwritten. Unless a Branch syncs into \
                         its own Parent, what it only got from its Parent is left out unless \
                         picked. A Branch follows its Parent on its own: what the Parent \
                         deploys is staged in it, but where the Branch changed a setting \
                         too, or discarded the Parent's change, the Parent's value is a \
                         hint `ployz diff` lists; --take PARENT stages it. With --at-merge, \
                         from a PR Environment into one of its Destinations, it is a \
                         Conditional Sync: the changes go live there with the pull request's \
                         merge. A secret the receiver lacks arrives without its value unless \
                         --value gives it one. --undo SYNC undoes a Sync while what it staged \
                         is undeployed and unchanged, or withdraws its Conditional Sync. A \
                         merged pull request's value its Conditional Sync left beside the \
                         Destination's own edit is a hint too; --take ID stages it, even \
                         once its PR Environment is gone. Example: ployz env sync --to \
                         --env fix-api --skip api.env.DEBUG --close",
                    ),
            )
            .arg(
                value("to", None)
                    .value_name("ENV")
                    .num_args(0..=1)
                    .required_unless_present_any(["from", "take", "undo"])
                    .conflicts_with("from")
                    .help("Sync into ENV; with no value, the Branch's Parent, or a PR Environment's only Destination"),
            )
            .arg(
                value("from", None)
                    .value_name("ENV")
                    .help("Sync ENV's changes into this Environment"),
            )
            .arg(
                repeated("only").value_name("ROW").help(
                    "Sync only this row (web.image), or every row under a prefix (web, web.env); repeatable",
                ),
            )
            .arg(
                repeated("skip")
                    .value_name("ROW")
                    .help("Leave out this row, or every row under a prefix; repeatable"),
            )
            .arg(
                repeated("value").value_name("ROW").help(
                    "Give a secret the receiver lacks its value, read as one line of stdin per --value, in order; repeatable",
                ),
            )
            .arg(
                value("version", None)
                    .help("Refuse unless this is still the version --plan showed"),
            )
            .arg(switch("plan", None).help("List the changes and the version; sync nothing"))
            .arg(
                switch("close", None)
                    .help("Close the Branch once its changes landed in its Parent; refused for a kept Branch and for any other --to")
                    .conflicts_with("plan"),
            )
            .arg(
                switch("at-merge", None)
                    .help("Go live in --to with the pull request's merge; the default from a PR Environment into a Destination")
                    .conflicts_with("close"),
            )
            .arg(
                value("undo", None)
                    .value_name("SYNC")
                    .help("Undo the Sync a sync printed, or withdraw its Conditional Sync")
                    .conflicts_with_all(["from", "only", "skip", "value", "plan", "version", "close", "at-merge"]),
            )
            .arg(
                value("take", None)
                    .value_name("ID")
                    .help("Stage the hints `ployz diff` lists from ID (the Parent, or a Conditional Sync) in --env; --only picks them")
                    .conflicts_with_all(["to", "from", "skip", "value", "plan", "close", "undo", "at-merge"]),
            ),
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
            store::scoped(
                Command::new("keep")
                    .about("Keep the Branch after syncing into its Parent and when idle"),
            )
            .arg(switch("off", None).help("Stop keeping it")),
        )
        .subcommand(
            store::scoped(
                Command::new("never-sync")
                    .about("Mark settings Never sync: Sync never carries them into or out of the Environment")
                    .long_about(
                        "Mark settings of the Environment Never sync: a Sync never carries \
                         them from it and never changes them in it. A Branch of the \
                         Environment still gets its value; the mark doesn't carry into \
                         Branches. --off syncs them again. Example: ployz env never-sync \
                         web.env.APP_ENV web.env.STRIPE_PUBLISHABLE_KEY --env staging",
                    ),
            )
            .arg(
                positional("path", true)
                    .num_args(1..)
                    .action(clap::ArgAction::Append)
                    .value_name("ROW")
                    .help("A row as this Environment names it (web.env.KEY), a prefix for every row under it (web.env), or its RowId"),
            )
            .arg(switch("off", None).help("Sync them again")),
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
            .arg(crate::cli::volume_acceptance())
            .arg(crate::cli::reviewed_version()),
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
        "setup" => setup,
        "rm" => rm,
        "branch" => branch::branch,
        "sync" => branch::sync,
        "copy" => branch::copy,
        "keep" => branch::keep,
        "never-sync" => never_sync,
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
    let created = store.write(&create)?;
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
    let listed = store(root)?.read(&EnvironmentsQuery {
        project: project(matches)?,
    })?;
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
    let listed = store(root)?.write(&set)?;
    crate::output::finish(&listed, || print_environments(&listed))
}

fn setup(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let set = ployz_store::SetBranchSetup {
        environment: store::environment(matches)?,
        setup: branch::setups(&super::string_values(matches, "setup"))?,
    };
    let listed = store(root)?.write(&set)?;
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
        for setup in &environment.branch_setup {
            say!(
                "    new Branches run in {}: {}",
                setup.service,
                setup.command
            );
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
    let store = store(root)?.args([name.as_str()]);
    let inventory = inventory(&store, &at)?;
    // Typed where it is: PROJECT/ENV.
    let typed = format!("{}/{name}", inventory.environment.project);
    // Not removed yet (queued, failed, cancelled, or its outcome unknown): this same
    // command finishes it once the removal applied, or queues it again.
    let store = store.args(["--confirm", typed.as_str()]);
    if !confirmed(matches, &typed, "Environment")? {
        return Err(unconfirmed(inventory, &store.again(&[])));
    }
    let project = inventory.environment.project.clone();
    let remove = RemoveEnvironment { environment: at };
    let events = deploy::open_events(matches)?;
    match remove_all(matches, &store, &remove, &project, events)? {
        Some((removed, ran)) => finish_removal(&removed, ran.last()),
        None => Ok(()),
    }
}

/// Refuse an unconfirmed `env rm`, naming what goes and the exact retry.
fn unconfirmed(inventory: Inventory, retry: &str) -> Error {
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
        match deployment {
            Some(deployment) if super::teardown::left_on_old_servers(deployment) => say!(
                "Left {}/{} on old servers: no Server was left to take it off (Deployment #{}).",
                environment.project,
                environment.name,
                deployment.number
            ),
            Some(deployment) => say!(
                "Removed {}/{} from the Servers (Deployment #{}).",
                environment.project,
                environment.name,
                deployment.number
            ),
            None => {}
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

/// `env never-sync`: mark rows Never sync, or with `--off` sync them again. The Store
/// resolves the names in this Environment.
fn never_sync(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let asked = super::string_values(matches, "path");
    let environment = store::environment(matches)?;
    let off = matches.get_flag("off");
    let store = store(root)?;
    let request = NeverSync {
        environment,
        rows: asked.iter().map(|asked| asked.as_str().into()).collect(),
        off,
    };
    let marked = store.write(&request)?;
    crate::output::finish(&marked, || {
        let paths = super::joined(&asked);
        let environment = &marked.environment;
        match request.off {
            true => say!(
                "Syncing {paths} again in {}/{}.",
                environment.project,
                environment.name
            ),
            false => say!(
                "Never syncing {paths} in {}/{}.",
                environment.project,
                environment.name
            ),
        }
    })
}
