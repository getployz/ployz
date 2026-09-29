//! `ployz diff`, `publish` and `discard`: review staged changes in the Config Store,
//! put them in Saved State, or undo them.

use clap::{ArgMatches, Command};
use ployz_store::{DiffQuery, Discard, Publish, Query, View, Written};

use super::config::{Next, environment, next, scoped, stale, store};
use super::{Error, leaf_matches};
use crate::cli::{positional, value};
use crate::output::say;

pub(crate) fn diff_command() -> Command {
    scoped(Command::new("diff").about("Show staged changes, grouped by Service"))
}

pub(crate) fn publish_command() -> Command {
    scoped(Command::new("publish").about("Put staged changes in Saved State without deploying"))
        .arg(version())
}

pub(crate) fn discard_command() -> Command {
    scoped(
        Command::new("discard").about("Undo staged changes: all, one Service's, or one Setting's"),
    )
    .arg(positional("path", false).help("SERVICE or SERVICE.SETTING [default: everything]"))
    .arg(version())
}

fn version() -> clap::Arg {
    value("version", None).help("Refuse unless this is still the latest `ployz diff` version")
}

pub(super) fn diff(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (store, actor) = store()?;
    let query = Query::Diff(DiffQuery {
        environment: environment(matches)?,
    });
    let View::Diff(view) = store.read(&actor, &query)? else {
        unreachable!("a diff reads a diff");
    };
    let hint = (!view.changes.is_empty() && !view.published)
        .then(|| next(matches, &["publish", "--version", &view.version]));
    crate::output::finish(&Next::new(&view, hint.clone()), || {
        let where_ = format!("{}/{}", view.environment.project, view.environment.name);
        if view.changes.is_empty() {
            say!("No staged changes in {where_}.");
        }
        for change in &view.changes {
            say!("{} ({:?})", change.name, change.lifecycle);
            for row in &change.settings {
                say!("  {}: {} -> {}", row.path, row.before, row.after);
            }
        }
        if view.published && !view.changes.is_empty() {
            say!(
                "Published as Saved revision {}.",
                view.saved.map_or(0, |saved| saved.0)
            );
        }
        if let Some(hint) = &hint {
            say!("next: {hint}");
        }
    })
}

pub(super) fn publish(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (store, actor) = store()?;
    let written = store.write(
        &actor,
        ployz_store::Command::Publish(Publish {
            environment: environment(matches)?,
            version: matches.get_one::<String>("version").cloned(),
        }),
    );
    let Written::Published(published) = written.map_err(|error| stale(error, matches, "diff"))?
    else {
        unreachable!("a publish writes a publish");
    };
    crate::output::finish(&published, || {
        let where_ = format!(
            "{}/{}",
            published.environment.project, published.environment.name
        );
        if published.created {
            say!("Published {where_} as Saved revision {}.", published.saved);
        } else {
            say!("Saved revision {} already holds {where_}.", published.saved);
        }
    })
}

pub(super) fn discard(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (store, actor) = store()?;
    let path = matches.get_one::<String>("path").cloned();
    let written = store.write(
        &actor,
        ployz_store::Command::Discard(Discard {
            environment: environment(matches)?,
            path: path.clone(),
            version: matches.get_one::<String>("version").cloned(),
        }),
    );
    let Written::Discarded(discarded) = written.map_err(|error| stale(error, matches, "diff"))?
    else {
        unreachable!("a discard writes a discard");
    };
    let hint = Some(next(matches, &["diff"]));
    crate::output::finish(&Next::new(&discarded, hint), || {
        say!(
            "Discarded {} in {}/{} (revision {}).",
            path.as_deref().unwrap_or("every staged change"),
            discarded.environment.project,
            discarded.environment.name,
            discarded.environment.revision
        );
    })
}
