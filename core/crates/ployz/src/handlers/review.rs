//! `ployz diff`, `publish` and `discard`: review staged changes in the Config Store,
//! put them in Saved State, or undo them.

use clap::{ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_store::{DiffQuery, Discard, Publish, SettingPath};

use super::store::{Next, environment, next, scoped, store, with_refresh_hint};
use super::{Error, leaf_matches};
use crate::cli::{positional, value};

pub(crate) fn diff_command() -> Command {
    scoped(Command::new("diff").about("Show staged Service, Volume and Config changes"))
}

pub(crate) fn publish_command() -> Command {
    scoped(Command::new("publish").about("Put staged changes in Saved State without deploying"))
        .arg(version())
        .arg(crate::cli::approval())
}

pub(crate) fn discard_command() -> Command {
    scoped(Command::new("discard").about("Undo staged Service, Volume, Config or Setting changes"))
        .arg(
            positional("path", false)
                .help("SERVICE, SERVICE.SETTING, volumes.VOLUME, configs.CONFIG or a `ployz diff` row [default: everything]"),
        )
        .arg(version())
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
    let hint = (!view.changes.is_empty())
        .then(|| next(matches, &["deploy", "--expect-version", &view.version]));
    crate::ui::finish(&Next::new(&view, hint.clone()), || {
        let where_ = format!("{}/{}", view.environment.project, view.environment.name);
        if view.changes.is_empty() {
            crate::ui::note(format_args!("No staged changes in {where_}."));
        }
        // Where a staged change came from, when another Environment sent it.
        let from = |row: Option<&ployz_store::RowId>| {
            view.incoming
                .iter()
                .find(|incoming| Some(incoming.at.row()) == row)
                .map_or_else(String::new, |incoming| format!(" (from {})", incoming.from))
        };
        for change in &view.changes {
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
        for hint in &view.hints {
            match hint.landed {
                ployz_store::Landed::Hint => crate::ui::stream(format_args!(
                    "PR #{} merged {} = {} beside your edit; use it: {}",
                    hint.pull_request,
                    hint.at,
                    hint.value,
                    next(
                        matches,
                        &[
                            "env",
                            "sync",
                            "--take",
                            hint.conditional_sync.as_str(),
                            "--only",
                            &hint.at.to_string()
                        ]
                    )
                )),
                ployz_store::Landed::Staged => {
                    crate::ui::stream(format_args!(
                        "PR #{} staged {} = {}",
                        hint.pull_request, hint.at, hint.value
                    ));
                }
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
