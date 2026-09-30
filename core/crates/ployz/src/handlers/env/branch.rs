//! Branches: create one, move changes between it and its Parent (Save, Update,
//! withdraw, take), copy a Live Node into it, keep it.

use clap::{ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_store::{
    Branched, ConditionalSaveId, CopyNode, CreateBranch, DeploymentId, EnvironmentId,
    EnvironmentName, EnvironmentRef, EnvironmentSummary, KeepBranch, Move, MovePick, MoveQuery,
    MoveView, Moved, PickChoice, Save, SaveState, SetupCommand, Take, Update, When,
};
use serde_json::json;

use super::super::config::expected;
use super::super::store::{self, Next, mint, project, store};
use super::super::{Error, leaf_matches, required};
use crate::cli::{repeated, switch, value};
use crate::cloud_account::StoreCallError;
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(super) fn branch(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = EnvironmentName::parse(required(matches, "name")?)?;
    let from = matches
        .get_one::<String>("from")
        .map(|from| EnvironmentName::parse(from.as_str()))
        .transpose()?;
    let nodes = |flag: &str| super::super::string_values(matches, flag);
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
        copy: super::node_names(&nodes("copy"))?,
        live: super::node_names(&nodes("live"))?,
        setup,
        keep: matches.get_flag("keep"),
        fix,
    };
    let mut args = vec![create.name.as_str()];
    if let Some(from) = &create.from.environment {
        args.extend(["--from", from.as_str()]);
    }
    let made = store(root)?.args(args).write(&create)?;
    let deploy = store::next(matches, &["deploy", "--env", create.name.as_str()]);
    finish(&made, Some(deploy), "Made Branch")
}

/// `env save` and `env update`: the Branch in scope, the changes picked, the guard.
pub(super) fn moving(command: Command) -> Command {
    store::scoped(command)
        .arg(
            repeated("only")
                .value_name("ROW[=CHOICE]")
                .help("Move only this change, or every change under it (web, web.env); a variable may say how it lands: from, parent or leave_out"),
        )
        .arg(value("version", None).help("Refuse unless this is still the version --plan showed"))
        .arg(switch("plan", None).help("List the changes and the version; move nothing"))
}

pub(super) fn save(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let here = store::environment(matches)?;
    if matches.get_one::<String>("take").is_some() {
        return shift(root, Shift::Take, (EnvironmentRef::default(), Some(here)));
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
    let shift_as = match matches.get_flag("withdraw") {
        true => Shift::Withdraw,
        false => Shift::Save,
    };
    shift(root, shift_as, (here, into))
}

pub(super) fn update(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let into = Some(store::environment(matches)?);
    shift(root, Shift::Update, (EnvironmentRef::default(), into))
}

/// What `env save` or `env update` does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shift {
    Save,
    Update,
    /// `env save --withdraw`: withdraw a Conditional Save.
    Withdraw,
    /// `env save --take`: take a retained Conditional Save's value.
    Take,
}

impl Shift {
    /// The `env` subcommand it runs as.
    fn command(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Save | Self::Withdraw | Self::Take => "save",
        }
    }
}

/// A Save, Update, withdrawal or take: with `--plan` its changes, else the Move itself.
fn shift(
    root: &ArgMatches,
    shift: Shift,
    (from, into): (EnvironmentRef, Option<EnvironmentRef>),
) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let command = shift.command();
    if matches.get_flag("plan") {
        let sides = match shift {
            Shift::Update | Shift::Take => MoveQuery::Update {
                into: into.unwrap_or_default(),
            },
            Shift::Save | Shift::Withdraw => MoveQuery::Save {
                from,
                into,
                when: None,
            },
        };
        let view = store.read(&sides)?;
        return plan(matches, command, &view);
    }
    let picks = match shift {
        Shift::Withdraw => Some(Vec::new()),
        Shift::Save | Shift::Update | Shift::Take => matches
            .get_many::<String>("only")
            .map(|only| only.map(|only| pick(only)).collect::<Result<Vec<_>, _>>())
            .transpose()?,
    };
    let version = matches.get_one::<String>("version").cloned();
    let request = match shift {
        Shift::Take => {
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
        Shift::Update => Move::Update(Update {
            into: into.unwrap_or_default(),
            picks,
            version,
        }),
        Shift::Save | Shift::Withdraw => Move::Save(Save {
            from,
            into,
            picks,
            version,
            when: (shift == Shift::Withdraw).then_some(When::AtMerge),
        }),
    };
    let moved = store
        .try_write(&request)
        .map_err(|error| store.fail(reviewed(error, matches, command)))?;
    moved_out(matches, shift, &moved)
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
    store::with_next(
        error,
        |refusal| refusal.details.get("version").is_some(),
        || store::next(matches, &["env", verb, "--plan"]),
    )
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
                notes.push(format!("lands as {}", store::word(&choice.default)));
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
fn moved_out(matches: &ArgMatches, shift: Shift, moved: &Moved) -> Result<(), Error> {
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
    let close = match (&moved.branch, shift, at_merge) {
        (Some(branch), Shift::Save, None) if !branch.kept => {
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
        match (shift, at_merge, &moved.conditional_save) {
            (Shift::Withdraw, ..) => say!("Withdrew {from}'s Conditional Save into {into}."),
            (_, Some(save), _) => say!(
                "Saved for PR #{}'s merge into {into}: {}.",
                save.pull_request,
                save.rows.join(", ")
            ),
            (Shift::Take, _, Some(save)) => {
                say!("Took PR #{}'s value into {into}.", save.pull_request);
            }
            _ => say!("Moved {from} → {into}."),
        }
        if !moved.staged.is_empty() {
            say!("Staged: {}", super::joined(&moved.staged));
        }
        if let Some(close) = &out.close {
            say!("Close the Branch when done: {close}");
        }
    })
}

pub(super) fn copy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let node = store::service_name(matches, "node")?;
    let copy = CopyNode {
        environment: store::environment(matches)?,
        node,
        expect: expected(matches)?,
    };
    let store = store(root)?.args([copy.node.as_str()]);
    let copied = store
        .try_write(&copy)
        .map_err(|error| store.fail(stale(error, matches)))?;
    finish(
        &copied,
        Some(store::next(matches, &["deploy"])),
        "Copied into Branch",
    )
}

pub(super) fn keep(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let keep = KeepBranch {
        environment: store::environment(matches)?,
        kept: !matches.get_flag("off"),
    };
    let kept = store(root)?.write(&keep)?;
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
            say!("Staged: {}", super::joined(&result.staged));
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
pub(super) fn setups(values: &[String]) -> Result<Vec<SetupCommand>, Error> {
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
