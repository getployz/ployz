//! `ployz diff`, `publish` and `discard`: review staged changes in the Config Store,
//! put them in Saved State, or undo them.

use clap::{ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_store::{DiffQuery, Discard, Publish, SettingPath};

use super::store::{Next, environment, next, scoped, store, with_refresh_hint};
use super::{Error, leaf_matches};
use crate::cli::{positional, switch, value};

pub(crate) fn diff_command() -> Command {
    scoped(Command::new("diff").about("Show staged Service, Volume and Config changes"))
}

pub(crate) fn publish_command() -> Command {
    scoped(Command::new("publish").about("Put staged changes in Saved State without deploying"))
        .arg(version())
        .arg(value("message", None).help("Message saved with the new revision"))
}

pub(crate) fn history_command() -> Command {
    scoped(Command::new("history").about("Show immutable saved versions"))
        .subcommand_required(false)
        .subcommand(history_action(
            "restore",
            "Stage a saved version in the draft",
        ))
        .subcommand(history_action(
            "undo",
            "Stage the inverse of one saved change",
        ))
}

fn history_action(name: &'static str, about: &'static str) -> Command {
    scoped(Command::new(name).about(about))
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

pub(crate) fn discard_command() -> Command {
    scoped(Command::new("discard").about("Undo staged Service, Volume, Config or Setting changes"))
        .arg(
            positional("path", false)
                .help("SERVICE, SERVICE.SETTING, volumes.VOLUME, configs.CONFIG or a `ployz diff` row [default: everything]"),
        )
        .arg(version())
        .arg(value("target", None).value_parser(["head", "saved", "review"]).default_value("head")
            .help("Head resets deploy changes; saved abandons draft reversals; review discards the displayed combined list"))
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
    let hint = (view.draft_count > 0)
        .then(|| next(matches, &["publish", "--version", &view.version]))
        .or_else(|| {
            (!view.changes.is_empty())
                .then(|| next(matches, &["deploy", "--expect-version", &view.version]))
        });
    crate::ui::finish(&Next::new(&view, hint.clone()), || {
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
                crate::ui::stream(format_args!(
                    "  {}: {} → {}{}",
                    row.path,
                    super::store::shown(&row.before),
                    super::store::shown(&row.after),
                    from(row.row.as_ref())
                ));
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
        for hint in &view.follow_hints {
            crate::ui::stream(format_args!(
                "{} deployed {} = {}, not staged here; use it: {}",
                hint.from,
                hint.at,
                hint.value,
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
        if view.published && !view.changes.is_empty() {
            crate::ui::stream(format_args!(
                "Published as Saved revision {}.",
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

pub(super) fn publish(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let publish = Publish {
        environment: environment(matches)?,
        version: matches.get_one::<String>("version").cloned(),
        message: matches.get_one::<String>("message").cloned(),
        accept_volume_loss: Vec::new(),
    };
    let store = store(root)?;
    let published = store
        .try_write(&publish)
        .map_err(|error| store.fail(with_refresh_hint(error, matches, "diff")))?;
    let where_ = format!(
        "{}/{}",
        published.environment.project, published.environment.name
    );
    let Some(saved) = published.saved else {
        return crate::ui::done(
            &Next::new(&published, None),
            format_args!("Nothing staged in {where_}; nothing to publish."),
        );
    };
    let hint = next(matches, &["deploy"]);
    crate::ui::finish(&Next::new(&published, Some(hint.clone())), || {
        if published.created {
            crate::ui::stream(format_args!(
                "Published {where_} as Saved revision {saved}."
            ));
        } else {
            crate::ui::stream(format_args!(
                "Saved revision {saved} already holds {where_}."
            ));
        }
        crate::ui::hint(&crate::ui::Hint::Next(hint));
    })
}

pub(super) fn discard(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let path = matches
        .get_one::<String>("path")
        .map(|path| SettingPath::parse(path))
        .transpose()?;
    let discard = Discard {
        environment: environment(matches)?,
        target: match matches.get_one::<String>("target").map(String::as_str) {
            Some("saved") => ployz_store::DiscardTarget::Saved,
            Some("review") => ployz_store::DiscardTarget::Review,
            _ => ployz_store::DiscardTarget::Head,
        },
        path: path.clone(),
        version: matches.get_one::<String>("version").cloned(),
    };
    let store = store(root)?;
    let discarded = store
        .try_write(&discard)
        .map_err(|error| store.fail(with_refresh_hint(error, matches, "diff")))?;
    let where_ = format!(
        "{}/{}",
        discarded.environment.project, discarded.environment.name
    );
    if !discarded.changed {
        return crate::ui::done(
            &Next::new(&discarded, None),
            format_args!(
                "Nothing staged{} in {where_}; nothing to discard.",
                path.map_or_else(String::new, |path| format!(" at {path}"))
            ),
        );
    }
    let hint = next(matches, &["diff"]);
    crate::ui::finish(&Next::new(&discarded, Some(hint.clone())), || {
        crate::ui::stream(format_args!(
            "Discarded {} in {where_} (revision {}).",
            path.map_or_else(|| "every staged change".to_owned(), |path| path.to_string()),
            discarded.environment.revision
        ));
        crate::ui::hint(&crate::ui::Hint::Inspect(hint));
    })
}

pub(super) fn history(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let environment = environment(matches)?;
    let action = root
        .subcommand()
        .and_then(|(_, history)| history.subcommand_name());
    let Some(action) = action else {
        let view = store.read(&ployz_store::HistoryQuery { environment })?;
        return crate::ui::finish(&view, || {
            if view.revisions.is_empty() {
                crate::ui::note("No saved versions.");
            }
            for revision in &view.revisions {
                crate::ui::stream(format_args!(
                    "#{}  {}",
                    revision.revision,
                    revision.message.as_deref().unwrap_or("Saved version")
                ));
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
                        row.path,
                        super::store::shown(&row.before),
                        super::store::shown(&row.after)
                    ));
                }
            }
            for path in &preview.overwritten {
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
            version: matches
                .get_one::<String>("version")
                .cloned()
                .unwrap_or(preview.version),
            accept_overwrite: matches.get_flag("accept-overwrite"),
        })
        .map_err(|error| store.fail(with_refresh_hint(error, matches, "history")))?;
    crate::ui::done(
        &staged,
        format_args!(
            "{} Saved revision {} in the draft. Review before saving or deploying.",
            if action == ployz_store::HistoryAction::Restore {
                "Restored"
            } else {
                "Undid"
            },
            revision
        ),
    )
}
