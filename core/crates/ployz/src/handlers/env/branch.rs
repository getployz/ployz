//! Branches: create one, sync changes between two Environments of a Project (or withdraw a
//! Conditional Sync, or take a hint), copy a Live Node into it, keep it.

use clap::ArgMatches;
use ployz_core::ServiceName;
use ployz_core::config::covers;
use ployz_store::{
    Branched, ConditionalSyncId, CopyNode, CreateBranch, DeploymentId, DiffQuery, EnvironmentId,
    EnvironmentName, EnvironmentRef, HintSource, KeepBranch, SetupCommand, SyncChanges, SyncQuery,
    SyncView, Synced, Take, Taken, When,
};
use serde_json::json;

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

/// `env sync`: with `--plan` the changes and their version, else the Sync, its rows
/// picked by `--only` and `--skip` against the changes read first.
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
    let query = match named("from")? {
        Some(from) => SyncQuery {
            from,
            into: Some(here),
        },
        None => SyncQuery {
            from: here,
            into: named("to")?,
        },
    };
    let store = store(root)?;
    let words = sync_words(matches);
    if matches.get_flag("withdraw") {
        let request = SyncChanges {
            from: query.from,
            into: query.into,
            when: Some(When::Withdraw),
            ..SyncChanges::default()
        };
        let synced = store
            .try_write(&request)
            .map_err(|error| store.fail(error))?;
        return synced_out(matches, &synced);
    }
    if matches.get_flag("plan") {
        let view = store.read(&query)?;
        return sync_plan(matches, &words, &view);
    }
    let (only, skip) = (
        super::super::string_values(matches, "only"),
        super::super::string_values(matches, "skip"),
    );
    let mut version = matches.get_one::<String>("version").cloned();
    let picks = match only.is_empty() && skip.is_empty() {
        true => None,
        false => {
            let view = store.read(&query)?;
            // The rows were picked from this view: sync exactly them.
            let picks = picked(matches, &words, &view, (&only, &skip))?;
            version.get_or_insert(view.version);
            Some(picks)
        }
    };
    let request = SyncChanges {
        from: query.from,
        into: query.into,
        picks,
        version,
        close_after: matches.get_flag("close"),
        when: None,
    };
    let synced = store.try_write(&request).map_err(|error| {
        // Stale, or nothing to sync: the plan shows what there is now.
        store.fail(store::with_next(
            error,
            |refusal| refusal.details.get("version").is_some(),
            || store::next(matches, &[words.as_slice(), &["--plan"]].concat()),
        ))
    })?;
    synced_out(matches, &synced)
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
    let only = super::super::string_values(matches, "only");
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
        rows: (!only.is_empty()).then_some(only),
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

/// `env sync --to [ENV]` or `env sync --from ENV`, as given.
fn sync_words(matches: &ArgMatches) -> Vec<&str> {
    match matches.get_one::<String>("from") {
        Some(from) => vec!["env", "sync", "--from", from.as_str()],
        None => {
            let mut words = vec!["env", "sync", "--to"];
            words.extend(matches.get_one::<String>("to").map(String::as_str));
            words
        }
    }
}

/// The keys of `view`'s rows `--only` names (else those ticked by default) less
/// those `--skip` names; a name matching no row is refused.
fn picked(
    matches: &ArgMatches,
    words: &[&str],
    view: &SyncView,
    (only, skip): (&[String], &[String]),
) -> Result<Vec<String>, Error> {
    if let Some(unknown) = only.iter().chain(skip).find(|asked| {
        !view
            .rows
            .iter()
            .any(|row| covers(&row.path.to_string(), asked))
    }) {
        let paths: Vec<String> = view.rows.iter().map(|row| row.path.to_string()).collect();
        return Err(Error::detailed(
            ployz_core::RpcErrorCode::NotFound,
            format!("No change named {unknown} syncs"),
            json!({
                "valid_children": paths,
                "next": store::next(matches, &[words, &["--plan"]].concat()),
            }),
        ));
    }
    Ok(view
        .rows
        .iter()
        .filter(|row| match only.is_empty() {
            true => row.ticked,
            false => only
                .iter()
                .any(|asked| covers(&row.path.to_string(), asked)),
        })
        .filter(|row| {
            !skip
                .iter()
                .any(|asked| covers(&row.path.to_string(), asked))
        })
        .map(|row| row.key.clone())
        .collect())
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
            if row.changed {
                notes.push(format!("{} changed it too", view.into.name));
            }
            if row.new {
                notes.push("new".to_owned());
            }
            let notes = match notes.is_empty() {
                true => String::new(),
                false => format!(" ({})", notes.join(", ")),
            };
            say!(
                "  {}: {} → {}{notes}",
                row.label,
                store::shown(&row.into),
                store::shown(&row.from)
            );
        }
        for row in &view.never_synced {
            say!(
                "  {}: never synced (marked in {})",
                row.label,
                crate::handlers::joined(&row.marked_in)
            );
        }
    })
}

/// What a Sync did, and `deploy` of where it landed when it staged something.
fn synced_out(matches: &ArgMatches, synced: &Synced) -> Result<(), Error> {
    let into = &synced.into;
    let next = (!synced.staged.is_empty())
        .then(|| in_project(matches, &["deploy", "--env", into.name.as_str()]));
    let withdrew = matches.get_flag("withdraw");
    crate::output::finish(&Next::new(synced, next.clone()), || {
        let (from, into) = (&synced.from.name, format!("{}/{}", into.project, into.name));
        match &synced.conditional_sync {
            _ if withdrew => say!("Withdrew {from}'s Conditional Sync into {into}."),
            Some(sync) => say!(
                "Goes live in {into} with PR #{}'s merge: {}.",
                sync.pull_request,
                sync.rows.join(", ")
            ),
            None => say!("Synced {from} → {into}."),
        }
        if !synced.staged.is_empty() {
            say!("Staged: {}", crate::handlers::joined(&synced.staged));
        }
        if synced.closing {
            say!("Closing {}.", synced.from.name);
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
