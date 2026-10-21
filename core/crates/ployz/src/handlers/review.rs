//! `ployz diff` and `discard`: review staged changes in the Config Store, or undo
//! them.

use clap::{ArgMatches, Command};
use ployz_core::config::{EnvironmentNodeType, ReviewLifecycleKind};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{DiffQuery, DiffView, Discard, Discarded, SettingPath};
use serde::Serialize;

use super::store::{Next, environment, next, scoped, store, with_next, with_refresh_hint};
use super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};
use crate::ui::Table;

pub(super) fn diff_command(command: Command) -> Command {
    scoped(command.about("Show staged Service, Volume and Config changes"))
}

pub(super) fn history_command(command: Command) -> Command {
    // Global, so `--project`/`--env` reach the action before or after its name.
    scoped(command.about("Show saved versions to undo or roll back to"))
        .mut_arg("project", |arg| arg.global(true))
        .mut_arg("env", |arg| arg.global(true))
        .subcommand_required(false)
}

pub(super) fn restore_command(command: Command) -> Command {
    history_action(
        command,
        "Roll back to a saved version's settings. Deploy to apply.",
    )
}

pub(super) fn undo_command(command: Command) -> Command {
    history_action(
        command,
        "Undo one saved version's changes. Deploy to apply.",
    )
}

fn history_action(command: Command, about: &'static str) -> Command {
    command
        .about(about)
        .arg(positional("revision", true).value_parser(clap::value_parser!(u64)))
        .arg(version())
        .arg(
            switch("preview", None)
                .help("Show changes and overwritten draft fields without writing"),
        )
        .arg(
            switch("accept-overwrite", None)
                .help("Accept the reviewed draft fields being overwritten"),
        )
}

pub(super) fn discard_command(command: Command) -> Command {
    scoped(command.about("Undo staged Service, Volume, Config or Setting changes"))
        .arg(
            positional("path", false)
                .help("SERVICE, SERVICE.SETTING, volumes.VOLUME, configs.CONFIG or a `ployz diff` row [default: everything]"),
        )
        .arg(version())
        .arg(value("target", None).value_parser(["head", "saved", "review"])
            .help("Head resets deploy changes; saved abandons draft reversals; review returns everything to the latest save [default: head, or saved for a path only saved lists]"))
}

/// `ployz diff --json`: the review, plus each staged domain change as people read it.
#[derive(Serialize)]
struct Diffed<'v> {
    #[serde(flatten)]
    view: &'v DiffView,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    domain_changes: Vec<super::store::DomainLine>,
}

fn version() -> clap::Arg {
    value("version", None).help("Refuse unless this is still the latest `ployz diff` version")
}

pub(super) fn diff(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let query = DiffQuery {
        environment: environment(matches)?,
    };
    let view = store(root)?.read(&query)?;
    let changes = view.review_changes();
    // A draft reversal alone changes nothing on the Servers, so only a Save records it.
    let hint = if !view.changes.is_empty() {
        Some(next(matches, &["deploy"]))
    } else {
        (view.draft_count > 0).then(|| next(matches, &["deploy", "--skip-deploy"]))
    };
    let shown = Diffed {
        domain_changes: super::store::domain_changes(
            changes.iter().flat_map(|change| &change.settings),
        ),
        view: &view,
    };
    crate::ui::finish(&Next::new(&shown, hint.clone()), || {
        let where_ = format!("{}/{}", view.environment.project, view.environment.name);
        if changes.is_empty() {
            crate::ui::note(format_args!("No staged changes in {where_}."));
        }
        // Where a staged change came from, when another Environment sent it.
        let from = |row: Option<&ployz_store::RowId>| {
            view.incoming
                .iter()
                .find(|incoming| Some(incoming.at.row()) == row)
                .map_or_else(String::new, |incoming| format!(" (from {})", incoming.from))
        };
        for change in &changes {
            crate::ui::stream(format_args!(
                "{} ({}){}",
                change.name,
                super::store::word(&change.lifecycle),
                from(Some(&change.row))
            ));
            for row in &change.settings {
                for line in super::store::changed(row, &change.settings) {
                    crate::ui::stream(format_args!("  {line}{}", from(row.row.as_ref())));
                }
            }
            restarts(change);
            match change.data {
                Some(ployz_store::DataEffect::Deleted) => crate::ui::stream(format_args!(
                    "  deletes this Volume's data on the Servers: deploy asks to accept it by name"
                )),
                Some(ployz_store::DataEffect::Kept) => {
                    crate::ui::stream(format_args!("  a detached Volume keeps its data"));
                }
                None => {}
            }
        }
        for setup in &view.setup {
            crate::ui::stream(format_args!(
                "setup.{} = {} (runs before {} first deploys)",
                setup.service,
                serde_json::Value::String(setup.command.clone()),
                setup.service
            ));
        }
        for hint in &view.follow_hints {
            crate::ui::stream(format_args!(
                "{} deployed {} = {}, not staged here; use it: {}",
                hint.from,
                hint.at,
                super::store::shown(&hint.value),
                next(
                    matches,
                    &[
                        "env",
                        "sync",
                        "--take",
                        hint.from.as_str(),
                        "--only",
                        &hint.at.to_string()
                    ]
                )
            ));
        }
        for reference in &view.broken {
            crate::ui::stream(format_args!(
                "{reference}; deploys skip it until you change it: {}",
                super::store::fix_broken(matches, reference)
            ));
        }
        if view.published && !view.changes.is_empty() {
            crate::ui::stream(format_args!(
                "Saved as version #{}. Not deployed.",
                view.saved.map_or(0, |saved| saved.0)
            ));
        }
        if let Some(hint) = &hint {
            crate::ui::hint(&crate::ui::Hint::Next(hint.clone()));
        }
    })
}

/// The Services a Config change restarts, under its rows.
pub(super) fn restarts(change: &ployz_store::NodeChange) {
    if !change.restarts.is_empty() {
        let services: Vec<&str> = change.restarts.iter().map(ServiceName::as_str).collect();
        crate::ui::stream(format_args!("  restarts {}", services.join(", ")));
    }
}

pub(super) fn discard(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let path = matches
        .get_one::<String>("path")
        .map(|path| SettingPath::parse(path))
        .transpose()?;
    let discard = Discard {
        environment: environment(matches)?,
        target: matches
            .get_one::<String>("target")
            .map(|target| match target.as_str() {
                "saved" => ployz_store::DiscardTarget::Saved,
                "review" => ployz_store::DiscardTarget::Review,
                _ => ployz_store::DiscardTarget::Head,
            }),
        path: path.clone(),
        version: matches.get_one::<String>("version").cloned(),
    };
    let store = store(root)?;
    let staged = store
        .read(&DiffQuery {
            environment: discard.environment.clone(),
        })
        .map(|view| view.review_changes())
        .unwrap_or_default();
    let shown = |paths: &[String]| ployz_store::NodeChange::shown_paths(&staged, paths);
    let discarded = store
        .try_write(&discard)
        .map_err(|error| store.fail(with_refresh_hint(error, matches, "diff")))?;
    let where_ = format!(
        "{}/{}",
        discarded.environment.project, discarded.environment.name
    );
    let backs: Vec<Back> = if path.is_none() {
        discarded
            .reversed
            .iter()
            .map(|reversal| Back::new(matches, reversal))
            .collect()
    } else {
        Vec::new()
    };
    let saved = discarded.saved.map_or(0, |saved| saved.0);
    let result = DiscardResult {
        discarded: &discarded,
        comes_back: &backs,
    };
    if !discarded.changed {
        let runs = match &path {
            Some(path) if backs.is_empty() => source_of_create(&store, matches, path)?,
            _ => None,
        };
        return crate::ui::finish(&Next::new(&result, None), || {
            if let (Some(path), Some(service)) = (&path, &runs) {
                crate::ui::stream(format_args!(
                    "{path} is what the new service {service} runs; it has no Discard of its own. Discard the service: {}",
                    next(matches, &["discard", service])
                ));
            } else if backs.is_empty() {
                crate::ui::stream(format_args!(
                    "Nothing staged{} in {where_}; nothing to discard.",
                    path.map_or_else(String::new, |path| format!(
                        " at {}",
                        shown(&[path.to_string()]).concat()
                    ))
                ));
            } else {
                crate::ui::stream(format_args!(
                    "Nothing differs from what is deployed in {where_}."
                ));
            }
            comes_back(&backs, saved);
        });
    }
    let hint = next(matches, &["diff"]);
    crate::ui::finish(&Next::new(&result, Some(hint.clone())), || {
        match (&path, discarded.dropped.as_slice()) {
            (Some(path), _) => crate::ui::stream(format_args!(
                "Discarded {} in {where_}.",
                shown(&[path.to_string()]).concat()
            )),
            (None, []) => {
                crate::ui::stream(format_args!("Discarded every staged change in {where_}."))
            }
            (None, dropped) => crate::ui::stream(format_args!(
                "Discarded {} staged {} in {where_}: {}.",
                dropped.len(),
                if dropped.len() == 1 {
                    "change"
                } else {
                    "changes"
                },
                shown(dropped).join(", ")
            )),
        }
        comes_back(&backs, saved);
        crate::ui::hint(&crate::ui::Hint::Inspect(hint));
    })
}

/// The new Service whose source row `path` names: a create's image, repository or
/// branch row, which only discarding the Service takes.
fn source_of_create(
    store: &super::store::Store,
    matches: &ArgMatches,
    path: &SettingPath,
) -> Result<Option<String>, Error> {
    let view = store.read(&DiffQuery {
        environment: environment(matches)?,
    })?;
    let path = path.to_string();
    Ok(view
        .changes
        .iter()
        .find(|change| {
            change.lifecycle == ReviewLifecycleKind::Create
                && change.node.node_type == EnvironmentNodeType::Service
                && change
                    .settings
                    .iter()
                    .any(|row| row.path == path && !row.can_restore && row.before.is_null())
        })
        .map(|change| change.name.clone()))
}

#[derive(Serialize)]
struct DiscardResult<'a> {
    #[serde(flatten)]
    discarded: &'a Discarded,
    comes_back: &'a [Back<'a>],
}

/// A saved change the draft no longer holds, and the command that brings it back.
#[derive(Serialize)]
struct Back<'a> {
    #[serde(skip)]
    change: &'a ployz_store::NodeChange,
    name: &'a str,
    command: String,
    drops_unsaved: bool,
}

impl<'a> Back<'a> {
    fn new(matches: &ArgMatches, reversal: &'a ployz_store::Reversal) -> Self {
        let command = match &reversal.way_back.path {
            Some(path) => next(matches, &["discard", &path.to_string()]),
            None => next(matches, &["discard", "--target", "saved"]),
        };
        Self {
            change: &reversal.change,
            name: &reversal.change.name,
            command,
            drops_unsaved: reversal.way_back.drops_unsaved,
        }
    }
}

/// One line per saved change the draft no longer holds, with the discard that brings it back.
fn comes_back(backs: &[Back<'_>], saved: u64) {
    for back in backs {
        let (name, command) = (back.name, &back.command);
        let (what, again) = match back.change.lifecycle {
            ReviewLifecycleKind::Delete => (
                format!("is in saved #{saved} but was never deployed, so discarding removes it"),
                "Bring it back",
            ),
            ReviewLifecycleKind::Create => (
                format!("is removed in saved #{saved} but still deployed, so discarding keeps it"),
                "Remove it again",
            ),
            ReviewLifecycleKind::Update => (
                format!(
                    "has changes in saved #{saved} that were never deployed, so discarding undoes them"
                ),
                "Bring them back",
            ),
        };
        if back.drops_unsaved {
            crate::ui::stream(format_args!(
                "{name} {what}. Another {kind} is named {name} too, so return everything to saved #{saved}, which also drops your unsaved changes: {command}",
                kind = match back.change.node.node_type {
                    EnvironmentNodeType::Service => "Service",
                    EnvironmentNodeType::Volume => "Volume",
                    EnvironmentNodeType::Config => "Config",
                }
            ));
        } else {
            crate::ui::stream(format_args!("{name} {what}. {again}: {command}"));
        }
    }
}

/// What a saved version changed, as a reader picks one: each changed Setting, or a
/// node made or removed, cut after a few.
fn summary(revision: &ployz_store::SavedRevision) -> String {
    const SHOWN: usize = 3;
    if revision.predecessor.is_none() {
        return "first version".to_owned();
    }
    let changed: Vec<String> = revision
        .changes
        .iter()
        .flat_map(|change| {
            if change.lifecycle == ReviewLifecycleKind::Update && !change.settings.is_empty() {
                change
                    .settings
                    .iter()
                    .map(|row| change.shown_path(row))
                    .collect()
            } else {
                vec![format!(
                    "{} ({})",
                    change.name,
                    super::store::word(&change.lifecycle)
                )]
            }
        })
        .collect();
    match changed.get(..SHOWN) {
        _ if changed.is_empty() => "nothing".to_owned(),
        Some(shown) if changed.len() > SHOWN => {
            format!("{} +{} more", shown.join(", "), changed.len() - SHOWN)
        }
        Some(_) | None => changed.join(", "),
    }
}

pub(super) fn history(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let environment = environment(matches)?;
    let action = root
        .subcommand()
        .and_then(|(_, history)| history.subcommand_name());
    let Some(action) = action else {
        let view = store.read(&ployz_store::HistoryQuery {
            environment,
            limit: None,
        })?;
        let mut table = Table::new(
            ["VERSION", "SAVED", "BY", "CHANGES", "MESSAGE"],
            "No saved versions.",
        );
        let now = chrono::Utc::now();
        for revision in &view.revisions {
            table.row([
                match view.live == Some(revision.revision) {
                    true => format!("#{} (live)", revision.revision),
                    false => format!("#{}", revision.revision),
                },
                revision
                    .saved_at
                    .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
                    .map_or_else(|| "-".to_owned(), |at| crate::checkin::ago(at, now)),
                revision
                    .saved_by
                    .as_ref()
                    .map_or("-", ployz_store::Principal::as_str)
                    .to_owned(),
                summary(revision),
                revision.message.clone().unwrap_or_else(|| "-".to_owned()),
            ]);
        }
        return crate::ui::finish(&view, || {
            crate::ui::rows(&table);
            if view.more {
                crate::ui::note("Older versions not shown.");
            }
        });
    };
    let action = match action {
        "restore" => ployz_store::HistoryAction::Restore,
        "undo" => ployz_store::HistoryAction::Undo,
        _ => unreachable!("history command admits only Restore and Undo"),
    };
    let revision = ployz_store::Revision(
        *matches
            .get_one::<u64>("revision")
            .expect("revision is required"),
    );
    let preview = store.read(&ployz_store::HistoryPreviewQuery {
        environment: environment.clone(),
        revision,
        action,
        change: None,
    })?;
    if matches.get_flag("preview") {
        return crate::ui::finish(&preview, || {
            for change in &preview.changes {
                crate::ui::stream(format_args!(
                    "{} ({})",
                    change.name,
                    super::store::word(&change.lifecycle)
                ));
                for row in &change.settings {
                    crate::ui::stream(format_args!(
                        "  {}: {} → {}",
                        change.shown_path(row),
                        super::store::shown(&row.before),
                        super::store::shown(&row.after)
                    ));
                }
            }
            for path in ployz_store::NodeChange::shown_paths(&preview.changes, &preview.overwritten)
            {
                crate::ui::stream(format_args!("  overwrites draft {path}"));
            }
            crate::ui::stream(format_args!("Review version: {}", preview.version));
        });
    }
    let staged = store
        .try_write(&ployz_store::StageHistory {
            environment,
            revision,
            action,
            change: None,
            version: matches
                .get_one::<String>("version")
                .cloned()
                .unwrap_or(preview.version),
            accept_overwrite: matches.get_flag("accept-overwrite"),
        })
        .map_err(|error| {
            let error = with_next(
                error,
                |refused| refused.code == RpcErrorCode::ConfirmationRequired,
                || store.again(&[&revision.0.to_string(), "--accept-overwrite"]),
            );
            store.fail(with_refresh_hint(error, matches, "history"))
        })?;
    if !staged.changed {
        return crate::ui::done(
            &staged,
            format_args!(
                "{} Nothing staged.",
                if action == ployz_store::HistoryAction::Restore {
                    format!("Already matches #{revision}.")
                } else {
                    format!("Nothing to undo in #{revision}.")
                }
            ),
        );
    }
    crate::ui::finish(&staged, || {
        crate::ui::stream(format_args!(
            "Staged {} #{revision}. Deploy to apply.",
            if action == ployz_store::HistoryAction::Restore {
                "a rollback to"
            } else {
                "an undo of"
            }
        ));
        for variable in &staged.not_set {
            crate::ui::stream(format_args!(
                "  {}.env.{} came back Not set. Deploys leave it out until you set it.",
                variable.service, variable.key
            ));
        }
    })
}
