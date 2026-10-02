//! Branches: create one, sync changes between two Environments of a Project (or undo a
//! Sync, or take a hint), copy a Live Node into it, keep it.

use std::collections::BTreeSet;

use clap::ArgMatches;
use ployz_core::ServiceName;
use ployz_store::{
    Branched, ConditionalSyncId, CopyNode, CreateBranch, DeploymentId, DiffQuery, EnvironmentId,
    EnvironmentName, EnvironmentRef, HintSource, KeepBranch, NamedRow, RowId, RowRef, SetupCommand,
    SyncChanges, SyncId, SyncQuery, SyncView, Synced, SyncedWhen, Take, Taken, UndoSync, Undone,
    When,
};

use super::super::config::expected;
use super::super::store::{self, Next, mint, project, store};
use super::super::{Error, leaf_matches, required};
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
    let made = store(root)?.write(&create)?;
    let deploy = store::next(matches, &["deploy", "--env", create.name.as_str()]);
    finish(&made, Some(deploy), "Made Branch")
}

/// `env sync`: with `--plan` the rows and their version, else the Sync of the rows
/// `--only` and `--skip` name, at the version read first unless `--version`.
pub(super) fn sync(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let named = |flag: &str| {
        matches
            .get_one::<String>(flag)
            .map(|name| {
                Ok::<_, Error>(EnvironmentRef {
                    project: project(matches)?,
                    environment: Some(EnvironmentName::parse(name.as_str())?),
                })
            })
            .transpose()
    };
    let here = store::environment(matches)?;
    if let Some(source) = matches.get_one::<String>("take") {
        return take(root, source, here);
    }
    if let Some(sync) = matches.get_one::<String>("undo") {
        return undo(root, sync);
    }
    // Omitted, the Store decides: at the merge from a PR Environment into a Destination.
    let when = match (matches.get_flag("at-merge"), matches.get_flag("close")) {
        (true, _) => Some(When::AtMerge),
        (false, true) => Some(When::Now { close_after: true }),
        (false, false) => None,
    };
    let query = match named("from")? {
        Some(from) => SyncQuery {
            from,
            into: Some(here),
            when,
        },
        None => SyncQuery {
            from: here,
            into: named("to")?,
            when,
        },
    };
    let store = store(root)?;
    let words = sync_words(matches);
    let view = store.read(&query)?;
    if matches.get_flag("plan") {
        return sync_plan(matches, &words, &view);
    }
    // Names resolve against the rows read now; a stale --version is refused anyway.
    let plan_next = || store::next(matches, &[words.as_slice(), &["--plan"]].concat());
    let refused = |error| {
        store.fail(store::with_next(
            StoreCallError::Refused(error),
            |_| true,
            plan_next,
        ))
    };
    let refs = |flag: &str| -> Vec<RowRef> {
        super::super::string_values(matches, flag)
            .iter()
            .map(|asked| asked.as_str().into())
            .collect()
    };
    let (only, skip) = (refs("only"), refs("skip"));
    // The Store resolves names; skipping, the picks are what's left once the CLI
    // resolved the skipped ones among the rows read, as it does.
    let picks = match skip.is_empty() {
        true => (!only.is_empty()).then_some(only),
        false => {
            let named: Vec<NamedRow> = view.rows.iter().map(|row| row.at.clone()).collect();
            let resolve = |asked: &[RowRef]| -> Result<BTreeSet<RowId>, Error> {
                let mut rows = BTreeSet::new();
                for asked in asked {
                    rows.extend(ployz_store::resolve(asked, &named).map_err(refused)?);
                }
                Ok(rows)
            };
            let left = resolve(&skip)?;
            let base = match only.is_empty() {
                true => view
                    .rows
                    .iter()
                    .filter(|row| row.ticked)
                    .map(|row| row.at.row.clone())
                    .collect(),
                false => resolve(&only)?,
            };
            Some(base.difference(&left).cloned().map(RowRef::from).collect())
        }
    };
    let values = secret_values(matches)?
        .into_iter()
        .map(|(asked, value)| (asked.as_str().into(), value))
        .collect();
    let request = SyncChanges {
        from: query.from,
        into: query.into,
        when,
        version: matches
            .get_one::<String>("version")
            .cloned()
            .unwrap_or(view.version),
        picks,
        values,
    };
    let synced = store.try_write(&request).map_err(|error| {
        // Stale, nothing to sync, or no such row: the plan shows what there is now.
        store.fail(store::with_next(
            error,
            |refusal| {
                refusal.details.get("version").is_some()
                    || matches!(
                        refusal.code,
                        ployz_core::RpcErrorCode::NotFound | ployz_core::RpcErrorCode::Ambiguous
                    )
            },
            plan_next,
        ))
    })?;
    synced_out(matches, &synced)
}

/// `--value ROW` names, each with its secret value: one line of stdin per name, in
/// order, as `set --secret` reads one.
// ponytail: a line per value, so a value can't hold a newline; read an env file if one must.
fn secret_values(matches: &ArgMatches) -> Result<Vec<(String, String)>, Error> {
    let rows = super::super::string_values(matches, "value");
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let input = std::io::read_to_string(std::io::stdin())?;
    let mut lines = input.lines();
    rows.into_iter()
        .map(|row| match lines.next() {
            Some(value) => Ok((row, value.to_owned())),
            None => Err(Error::usage(format!(
                "Expected a line of stdin for each --value: none for {row}"
            ))
            .with_exit(USAGE_EXIT)),
        })
        .collect()
}

/// `env sync --undo SYNC`: undo a Sync, or withdraw its Conditional Sync.
fn undo(root: &ArgMatches, sync: &str) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let request = UndoSync {
        sync: SyncId::parse(sync).map_err(|_| {
            Error::usage("Expected --undo SYNC to be the ID a Sync printed").with_exit(USAGE_EXIT)
        })?,
    };
    let undone: Undone = store(root)?.write(&request)?;
    let into = &undone.into;
    let next = in_project(matches, &["diff", "--env", into.name.as_str()]);
    crate::output::finish(&Next::new(&undone, Some(next.clone())), || {
        say!(
            "Undid Sync {} in {}/{}.",
            request.sync,
            into.project,
            into.name
        );
        say!("next: {next}");
    })
}

/// `env sync --take ID`: stage the hints (or those `--only` names) that the Parent
/// named `ID`, or Conditional Sync `ID`, left in `into`.
fn take(root: &ArgMatches, source: &str, into: EnvironmentRef) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let from = ConditionalSyncId::parse(source)
        .map(HintSource::ConditionalSync)
        .or_else(|_| EnvironmentName::parse(source).map(HintSource::Parent))
        .map_err(|_| {
            Error::usage("Expected --take ID to be the Parent's name or a Conditional Sync ID")
                .with_exit(USAGE_EXIT)
        })?;
    let rows: Vec<RowRef> = super::super::string_values(matches, "only")
        .iter()
        .map(|asked| asked.as_str().into())
        .collect();
    let store = store(root)?;
    // The hints were read at this version, else at the diff's now.
    let version = match matches.get_one::<String>("version") {
        Some(version) => version.clone(),
        None => {
            store
                .read(&DiffQuery {
                    environment: into.clone(),
                })?
                .version
        }
    };
    let request = Take {
        from,
        into: Some(into),
        rows: (!rows.is_empty()).then_some(rows),
        version,
    };
    let taken = store
        .try_write(&request)
        .map_err(|error| store.fail(store::with_refresh_hint(error, matches, "diff")))?;
    taken_out(matches, &taken)
}

/// What a take staged, and `deploy` of where it landed.
fn taken_out(matches: &ArgMatches, taken: &Taken) -> Result<(), Error> {
    let into = &taken.into;
    let next = (!taken.staged.is_empty())
        .then(|| in_project(matches, &["deploy", "--env", into.name.as_str()]));
    crate::output::finish(&Next::new(taken, next.clone()), || {
        let into = format!("{}/{}", into.project, into.name);
        match &taken.conditional_sync {
            Some(sync) => say!("Took PR #{}'s value into {into}.", sync.pull_request),
            None => say!("Took {}'s value into {into}.", taken.from.name),
        }
        if !taken.staged.is_empty() {
            say!("Staged: {}", crate::handlers::joined(&taken.staged));
        }
        if let Some(next) = &next {
            say!("next: {next}");
        }
    })
}

/// `env sync --to [ENV]` or `env sync --from ENV`, and `--at-merge`, as given.
fn sync_words(matches: &ArgMatches) -> Vec<&str> {
    let mut words = match matches.get_one::<String>("from") {
        Some(from) => vec!["env", "sync", "--from", from.as_str()],
        None => {
            let mut words = vec!["env", "sync", "--to"];
            words.extend(matches.get_one::<String>("to").map(String::as_str));
            words
        }
    };
    if matches.get_flag("at-merge") {
        words.push("--at-merge");
    }
    words
}

fn sync_plan(matches: &ArgMatches, words: &[&str], view: &SyncView) -> Result<(), Error> {
    let next = (!view.rows.is_empty()).then(|| {
        store::next(
            matches,
            &[words, &["--version", view.version.as_str()]].concat(),
        )
    });
    crate::output::finish(&Next::new(view, next), || {
        let when = view
            .at_merge
            .map(|number| format!(" at #{number}'s merge"))
            .unwrap_or_default();
        say!(
            "{} → {}{when} (version {}):",
            view.from.name,
            view.into.name,
            view.version
        );
        if view.rows.is_empty() {
            say!("  nothing to sync");
        }
        for row in &view.rows {
            let mut notes = Vec::new();
            if !row.ticked {
                notes.push("left out unless picked".to_owned());
            }
            match row.change {
                ployz_store::SyncChange::Conflict => {
                    notes.push(format!("{} changed it too", view.into.name));
                }
                ployz_store::SyncChange::New => notes.push("new".to_owned()),
                ployz_store::SyncChange::Changed => {}
            }
            if row.secret.as_ref().is_some_and(|secret| secret.needs_value) {
                notes.push(format!(
                    "{} lacks this secret: it arrives without a value",
                    view.into.name
                ));
            }
            let notes = match notes.is_empty() {
                true => String::new(),
                false => format!(" ({})", notes.join(", ")),
            };
            say!(
                "  {}: {} → {}{notes}",
                row.at.label(),
                store::shown(&row.into),
                store::shown(&row.from)
            );
        }
        for row in &view.never_synced {
            say!(
                "  {}: never synced (marked in {})",
                row.at.label(),
                crate::handlers::joined(&row.marked_in)
            );
        }
    })
}

/// What a Sync did, and `deploy` of where it landed when it staged something.
fn synced_out(matches: &ArgMatches, synced: &Synced) -> Result<(), Error> {
    let into = &synced.into;
    let next = matches!(&synced.when, SyncedWhen::Now { staged, .. } if !staged.is_empty())
        .then(|| in_project(matches, &["deploy", "--env", into.name.as_str()]));
    crate::output::finish(&Next::new(synced, next.clone()), || {
        let (from, into) = (&synced.from.name, format!("{}/{}", into.project, into.name));
        let undo = in_project(matches, &["env", "sync", "--undo", synced.sync.as_str()]);
        match &synced.when {
            SyncedWhen::AtMerge { conditional_sync } => {
                say!(
                    "Goes live in {into} with PR #{}'s merge: {}.",
                    conditional_sync.pull_request,
                    crate::handlers::joined(
                        &conditional_sync
                            .rows
                            .iter()
                            .map(ployz_store::NamedRow::label)
                            .collect::<Vec<_>>()
                    )
                );
                say!("Undo it: {undo}");
            }
            SyncedWhen::Now { staged, closing } => {
                say!("Synced {from} → {into}.");
                say!("Undo it: {undo}");
                if !staged.is_empty() {
                    say!("Staged: {}", crate::handlers::joined(staged));
                }
                if *closing {
                    say!("Closing {from}.");
                }
            }
        }
        if let Some(next) = &next {
            say!("next: {next}");
        }
    })
}

/// `ployz WORDS…` in the same Project as this command, whatever its Environment.
fn in_project(matches: &ArgMatches, words: &[&str]) -> String {
    let mut words: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    if let Ok(Some(project)) = matches.try_get_one::<String>("project") {
        words.extend(["--project".to_owned(), project.clone()]);
    }
    shell_words::join(std::iter::once("ployz".to_owned()).chain(words))
}

pub(super) fn copy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let node = store::service_name(matches, "node")?;
    let copy = CopyNode {
        environment: store::environment(matches)?,
        node,
        expect: expected(matches)?,
    };
    let store = store(root)?;
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
            say!("Staged: {}", crate::handlers::joined(&result.staged));
        }
        for live in &branch.live {
            match &live.owner {
                Some(owner) => say!("Uses {} live from {owner}.", live.name),
                None => say!("Uses {} live, but nothing runs it.", live.name),
            }
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
