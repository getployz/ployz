//! `ployz env`: Environments in the Config Store, and Branches of them. A Branch
//! copies the Services and Volumes picked (and what they use that its Parent
//! doesn't run) and uses the rest live; `update` stages what its Parent deployed
//! since, `copy` turns a Live Node into its own copy, and `keep` keeps it after a Save.

use clap::{ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_store::{
    Branched, CopyNode, CreateBranch, CreateEnvironment, DeploymentId, EnvironmentId,
    EnvironmentName, EnvironmentRef, KeepBranch, SetupCommand, UpdateBranch,
};

use super::config::{expect, expected};
use super::store::{self, Next, failed, mint, project, project_arg, store};
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
