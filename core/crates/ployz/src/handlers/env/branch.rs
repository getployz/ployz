//! Branches: create one, sync changes between two Environments of a Project (or undo a
//! Sync, or take a hint), copy a Live Node into it, keep it.

use clap::ArgMatches;
use ployz_core::ServiceName;
use ployz_core::config::{ChangeKind, ReviewLifecycleKind, ServiceSettingChange};
use ployz_store::{
    Branched, CopyNode, CreateBranch, DeploymentId, DiffQuery, EnvironmentId, EnvironmentName,
    EnvironmentRef, HintSource, KeepBranch, NodeName, NotSetVariable, ProjectName, RowRef,
    ServicesQuery, SetupCommand, SourceKind, SyncChange, SyncChanges, SyncId, SyncQuery, SyncRow,
    SyncView, Synced, Take, Taken, UndoSync, Undone,
};

use super::super::config::expected;
use super::super::store::{self, Next, Store, mint, project, store};
use super::super::{Error, leaf_matches, required};
use crate::cloud_account::StoreCallError;

pub(crate) fn branch(root: &ArgMatches) -> Result<(), Error> {
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
            DeploymentId::parse(id.as_str())
                .map_err(|_| Error::usage("Expected --fix DEPLOYMENT to be a Deployment ID"))
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
    let store = store(root)?;
    let made = store.write(&create)?;
    let branch = &made.branch.environment;
    let ship = ship(
        &store,
        EnvironmentRef {
            project: Some(branch.project.clone()),
            environment: Some(branch.name.clone()),
        },
    );
    let deploy = store::next(matches, &[ship, "--env", create.name.as_str()]);
    finish(&made, Some(deploy), "Made Branch")
}

/// `deploy`, or `up` when a Service the next Deploy ships has no source, as a copy of
/// one `ployz up` uploaded has: only a new upload gives it something to run.
fn ship(store: &Store<'_>, environment: EnvironmentRef) -> &'static str {
    let sourceless = store
        .try_read(&ServicesQuery { environment })
        .is_ok_and(|view| {
            view.services
                .iter()
                .any(|listing| listing.source == SourceKind::Empty && listing.change.is_some())
        });
    if sourceless { "up" } else { "deploy" }
}

/// `env sync`: with `--plan` the rows and their version, else the Sync of the rows
/// `--only` and `--skip` name, at the version read first unless `--version`.
pub(crate) fn sync(root: &ArgMatches) -> Result<(), Error> {
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
    let secrets = !super::super::string_values(matches, "value").is_empty();
    if secrets && matches.get_one::<String>("approval").is_some() {
        return Err(Error::usage(
            "Expected --approval without --value: an approval can't carry secrets given again \
             on stdin. Run the sync without --approval and without --json: it waits for a new \
             approval.",
        ));
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
    let words = sync_words(matches);
    let mut retried = sync_target(matches);
    retried.extend(repeated(matches, &["--only", "--skip", "--value"]));
    if matches.get_flag("close") {
        retried.push("--close");
    }
    let store = store(root)?.args(retried).reads_secrets(secrets);
    let view = store.read(&query)?;
    if matches.get_flag("plan") {
        return sync_plan(matches, &words, &view);
    }
    // Names resolve against the rows read now; a stale --version is refused anyway.
    let plan_next = || store::next(matches, &[words.as_slice(), &["--plan"]].concat());
    let refs = |flag: &str| -> Vec<RowRef> {
        super::super::string_values(matches, flag)
            .iter()
            .map(|asked| asked.as_str().into())
            .collect()
    };
    let values = secret_values(matches)?
        .into_iter()
        .map(|(asked, value)| (asked.as_str().into(), value.into()))
        .collect();
    let request = SyncChanges {
        from: query.from,
        into: query.into,
        close_after: matches.get_flag("close"),
        version: matches
            .get_one::<String>("version")
            .cloned()
            .unwrap_or(view.version),
        picks: Some(refs("only")).filter(|only| !only.is_empty()),
        skip: refs("skip"),
        values,
        // One receipt for this run: a retry of the same request replays, not repeats.
        id: Some(SyncId::parse(uuid::Uuid::new_v4().to_string())?),
    };
    // Stale, nothing to sync, or no such row: the plan shows what there is now.
    // Sealing resolves the rows and version as the write does, so it fails the same.
    let fail = |error| {
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
    };
    let request = store.seal_sync_values(request, fail)?;
    let synced = store.try_write(&request).map_err(fail)?;
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
            ))),
        })
        .collect()
}

/// `env sync --undo SYNC`: undo a Sync, or withdraw its offer.
fn undo(root: &ArgMatches, sync: &str) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let request = UndoSync {
        sync: SyncId::parse(sync)
            .map_err(|_| Error::usage("Expected --undo SYNC to be the ID a Sync printed"))?,
    };
    let undone: Undone = store(root)?.args(["--undo", sync]).write(&request)?;
    let into = &undone.into;
    let next = in_project(matches, &["diff", "--env", into.name.as_str()]);
    crate::ui::finish(&Next::new(&undone, Some(next.clone())), || {
        crate::ui::stream(format_args!(
            "Undid Sync {} in {}/{}.",
            request.sync, into.project, into.name
        ));
        crate::ui::hint(&crate::ui::Hint::Next(next.clone()));
    })
}

/// `env sync --take PARENT`: stage the hints (or those `--only` names) that the
/// Parent named `PARENT` left in `into`.
fn take(root: &ArgMatches, source: &str, into: EnvironmentRef) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let from = EnvironmentName::parse(source)
        .map(HintSource::Parent)
        .map_err(|_| Error::usage("Expected --take PARENT to be the Parent's name"))?;
    let rows: Vec<RowRef> = super::super::string_values(matches, "only")
        .iter()
        .map(|asked| asked.as_str().into())
        .collect();
    let store = store(root)?
        .args(["--take", source])
        .args(repeated(matches, &["--only"]));
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
    crate::ui::finish(&Next::new(taken, next.clone()), || {
        let into = format!("{}/{}", into.project, into.name);
        crate::ui::stream(format_args!(
            "Took {}'s value into {into}.",
            taken.from.name
        ));
        if !taken.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Staged: {}",
                crate::handlers::joined(&taken.staged)
            ));
        }
        if let Some(next) = &next {
            crate::ui::hint(&crate::ui::Hint::Next(next.clone()));
        }
    })
}

/// Each value of each repeatable flag in `flags`, after its flag. `--value` names only a
/// row: its secret comes on stdin, so a retry never repeats it.
fn repeated<'m>(matches: &'m ArgMatches, flags: &[&'static str]) -> Vec<&'m str> {
    let mut words = Vec::new();
    for flag in flags {
        let id = flag.trim_start_matches("--");
        for value in matches.get_many::<String>(id).into_iter().flatten() {
            words.extend([*flag, value.as_str()]);
        }
    }
    words
}

/// `env sync --to [ENV]` or `env sync --from ENV`, as given.
fn sync_words(matches: &ArgMatches) -> Vec<&str> {
    [&["env", "sync"][..], &sync_target(matches)].concat()
}

/// `--to [ENV]` or `--from ENV`, as given.
fn sync_target(matches: &ArgMatches) -> Vec<&str> {
    match matches.get_one::<String>("from") {
        Some(from) => vec!["--from", from.as_str()],
        None => {
            let mut words = vec!["--to"];
            words.extend(matches.get_one::<String>("to").map(String::as_str));
            words
        }
    }
}

fn sync_plan(matches: &ArgMatches, words: &[&str], view: &SyncView) -> Result<(), Error> {
    let next = (!view.rows.is_empty()).then(|| {
        store::next(
            matches,
            &[words, &["--version", view.version.as_str()]].concat(),
        )
    });
    crate::ui::finish(&Next::new(view, next), || {
        crate::ui::stream(format_args!(
            "{} → {} (version {}):",
            view.from.name, view.into.name, view.version
        ));
        if view.rows.is_empty() {
            crate::ui::stream(format_args!("  nothing to sync"));
        }
        for (node, rows) in by_node(&view.rows) {
            let whole = rows.iter().find(|row| row.at.name().is_none());
            let lifecycle = match whole {
                Some(row) if row.into.is_null() => ReviewLifecycleKind::Create,
                Some(row) if row.from.is_null() => ReviewLifecycleKind::Delete,
                _ => ReviewLifecycleKind::Update,
            };
            // A node it creates is new throughout, as its header says.
            let created = lifecycle == ReviewLifecycleKind::Create;
            crate::ui::stream(format_args!(
                "  {} ({}){}",
                node_name(node),
                store::word(&lifecycle),
                whole.map_or_else(String::new, |row| sync_notes(view, row, created))
            ));
            // A node it creates lands as its introduction, so `diff` shows only its source;
            // a row in it still names what it warns of.
            if let (true, Some(whole)) = (created, whole) {
                for line in created_lines(whole) {
                    crate::ui::stream(format_args!("    {line}"));
                }
                for row in rows.iter().filter(|row| row.at.name().is_some()) {
                    let notes = sync_notes(view, row, true);
                    if !notes.is_empty() {
                        crate::ui::stream(format_args!(
                            "    {}{notes}",
                            crate::ui::escaped(&row.at.to_string())
                        ));
                    }
                }
                continue;
            }
            for row in rows.iter().filter(|row| row.at.name().is_some()) {
                let notes = sync_notes(view, row, false);
                for line in sync_lines(row) {
                    crate::ui::stream(format_args!("    {line}{notes}"));
                }
            }
        }
        for row in &view.never_synced {
            let sides: std::collections::BTreeSet<_> = row
                .marks
                .iter()
                .map(|mark| mark.environment.as_str())
                .collect();
            crate::ui::stream(format_args!(
                "  {}: never synced (marked in {})",
                row.at,
                Vec::from_iter(sides).join(", ")
            ));
        }
    })
}

/// `rows` under the node each is in, in the order the nodes first appear.
fn by_node(rows: &[SyncRow]) -> Vec<(&NodeName, Vec<&SyncRow>)> {
    let mut nodes: Vec<(&NodeName, Vec<&SyncRow>)> = Vec::new();
    for row in rows {
        match nodes.iter_mut().find(|(node, _)| *node == row.at.node()) {
            Some((_, rows)) => rows.push(row),
            None => nodes.push((row.at.node(), vec![row])),
        }
    }
    nodes
}

/// A node by its name alone, as `diff` heads its changes.
fn node_name(node: &NodeName) -> String {
    match node {
        NodeName::Service(service) => service.to_string(),
        NodeName::Volume(volume) => volume.to_string(),
        NodeName::Config(config) => config.to_string(),
    }
}

/// What a row's line adds in brackets: whether it syncs, and what it overwrites or lacks.
fn sync_notes(view: &SyncView, row: &SyncRow, created: bool) -> String {
    let mut notes = Vec::new();
    if !row.ticked {
        notes.push("left out unless picked".to_owned());
    }
    match row.change {
        SyncChange::Conflict => notes.push(format!("{} changed it too", view.into.name)),
        SyncChange::New if !created => notes.push("new".to_owned()),
        SyncChange::New | SyncChange::Changed => {}
    }
    if row.secret.is_some() {
        notes.push(format!(
            "{} lacks this secret: it arrives Not set",
            view.into.name
        ));
    }
    match notes.is_empty() {
        true => String::new(),
        false => format!(" ({})", notes.join(", ")),
    }
}

/// A Sync row as `diff` reads the change it stages in the receiver, a Config's fields by name.
fn sync_lines(row: &SyncRow) -> Vec<String> {
    match (row.at.node(), row.at.name()) {
        (NodeName::Config(_), Some(field)) => {
            vec![store::config_field_changed(field, &row.into, &row.from)]
        }
        _ => store::changed(
            &setting_change(row.at.to_string(), row.into.clone(), row.from.clone()),
            &[],
        ),
    }
}

/// What `diff` shows under a node a Sync creates once it lands: a Service's source.
fn created_lines(whole: &SyncRow) -> Vec<String> {
    let NodeName::Service(service) = whole.at.node() else {
        return Vec::new();
    };
    let lineage = whole.at.row().lineage();
    let Ok(source) = ployz_core::config::arriving_source(lineage, &whole.from) else {
        return Vec::new();
    };
    ployz_store::introduced_source(service.as_str(), lineage, &source)
        .iter()
        .flat_map(|row| store::changed(row, &[]))
        .collect()
}

/// The Setting change that moves `before` to `after` at `path`.
fn setting_change(
    path: String,
    before: serde_json::Value,
    after: serde_json::Value,
) -> ServiceSettingChange {
    let kind = match (before.is_null(), after.is_null()) {
        (true, _) => ChangeKind::Add,
        (_, true) => ChangeKind::Remove,
        _ => ChangeKind::Update,
    };
    ServiceSettingChange {
        path,
        config_name: None,
        hostnames: None,
        kind,
        before,
        after,
        can_restore: false,
        row: None,
    }
}

/// What a Sync did, and `deploy` of where it landed when it staged something.
fn synced_out(matches: &ArgMatches, synced: &Synced) -> Result<(), Error> {
    let into = &synced.into;
    let next = (!synced.staged.is_empty())
        .then(|| in_project(matches, &["deploy", "--env", into.name.as_str()]));
    // The proposal is the Dashboard's to Remove; here a Sync is undone by its id.
    let mut json = serde_json::to_value(synced).expect("a Sync serializes");
    if let Some(json) = json.as_object_mut() {
        json.remove("proposal");
    }
    crate::ui::finish(&Next::new(&json, next.clone()), || {
        let (from, into) = (&synced.from.name, format!("{}/{}", into.project, into.name));
        let undo = in_project(matches, &["env", "sync", "--undo", synced.sync.as_str()]);
        crate::ui::stream(format_args!("Synced {from} → {into}."));
        if !synced.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Staged: {}",
                crate::handlers::joined(&synced.staged)
            ));
        }
        if synced.closing {
            crate::ui::stream(format_args!("Closing {from}."));
        }
        crate::ui::hint(&crate::ui::Hint::Undo(undo));
        if let Some(next) = &next {
            crate::ui::hint(&crate::ui::Hint::Next(next.clone()));
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

pub(crate) fn copy(root: &ArgMatches) -> Result<(), Error> {
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

pub(crate) fn keep(root: &ArgMatches) -> Result<(), Error> {
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
fn stale(error: StoreCallError, matches: &ArgMatches) -> store::Refusal {
    if matches.get_one::<String>("expect").is_some() {
        store::with_refresh_hint(error, matches, "get")
    } else {
        error.into()
    }
}

/// A Branch after a change, and `deploy` when it staged something.
fn finish(result: &Branched, deploy: Option<String>, what: &str) -> Result<(), Error> {
    let next = deploy.filter(|_| !result.staged.is_empty());
    crate::ui::finish(&Next::new(result, next.clone()), || {
        let branch = &result.branch;
        crate::ui::stream(format_args!(
            "{what} {}/{} of {}.",
            branch.environment.project, branch.environment.name, branch.parent
        ));
        if !result.staged.is_empty() {
            crate::ui::stream(format_args!(
                "Staged: {}",
                crate::handlers::joined(&result.staged)
            ));
        }
        if let Some(note) = not_set_note(
            &branch.environment.project,
            &branch.environment.name,
            &result.not_set,
        ) {
            crate::ui::stream(crate::ui::escaped(&note));
        }
        for live in &branch.live {
            match &live.owner {
                Some(owner) => {
                    crate::ui::stream(format_args!("Uses {} live from {owner}.", live.name))
                }
                None => crate::ui::stream(format_args!(
                    "Uses {} live, but nothing runs it.",
                    live.name
                )),
            }
        }
        if let Some(next) = next {
            crate::ui::hint(&crate::ui::Hint::Next(next));
        }
    })
}

/// How many secrets a Branch holds without a value, which they are, and how to set
/// one: a copy never carries a secret's value.
fn not_set_note(
    project: &ProjectName,
    branch: &EnvironmentName,
    not_set: &[NotSetVariable],
) -> Option<String> {
    let first = not_set.first()?;
    let path = |variable: &NotSetVariable| format!("{}.env.{}", variable.service, variable.key);
    let paths: Vec<String> = not_set.iter().map(path).collect();
    let (secrets, them, each) = match not_set.len() {
        1 => ("secret", "it", "it"),
        _ => ("secrets", "them", "each"),
    };
    let set = shell_words::join([
        "ployz",
        "set",
        &path(first),
        "--secret",
        "--project",
        project.as_str(),
        "--env",
        branch.as_str(),
    ]);
    Some(format!(
        "{} {secrets} not set in {branch}: {}. Deploys leave {them} out until you set {each}: {set}",
        not_set.len(),
        paths.join(", "),
    ))
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
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ployz_core::ServiceName;
    use ployz_store::{EnvironmentName, NotSetVariable, ProjectName};

    use super::not_set_note;

    fn secret(service: &str, key: &str) -> NotSetVariable {
        NotSetVariable {
            service: ServiceName::parse(service).unwrap(),
            key: key.to_owned(),
        }
    }

    #[test]
    fn a_branch_with_every_secret_set_says_nothing() {
        let shop = ProjectName::parse("shop").unwrap();
        let feat = EnvironmentName::parse("feat").unwrap();
        assert_eq!(not_set_note(&shop, &feat, &[]), None);
    }

    #[test]
    fn a_branch_counts_its_secrets_without_a_value_and_says_how_to_set_one() {
        let shop = ProjectName::parse("shop").unwrap();
        let feat = EnvironmentName::parse("feat").unwrap();
        assert_eq!(
            not_set_note(&shop, &feat, &[secret("web", "TOKEN")]).as_deref(),
            Some(
                "1 secret not set in feat: web.env.TOKEN. Deploys leave it out until you \
                 set it: ployz set web.env.TOKEN --secret --project shop --env feat"
            )
        );
        assert_eq!(
            not_set_note(
                &shop,
                &feat,
                &[secret("web", "TOKEN"), secret("worker", "API_KEY")]
            )
            .as_deref(),
            Some(
                "2 secrets not set in feat: web.env.TOKEN, worker.env.API_KEY. Deploys leave \
                 them out until you set each: ployz set web.env.TOKEN --secret --project shop \
                 --env feat"
            )
        );
    }
}
