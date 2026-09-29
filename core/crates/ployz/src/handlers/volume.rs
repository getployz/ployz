//! Authored Volumes in the Config Store: add, list, inspect and remove. Every change
//! is staged in Working State until a Deploy ships it. Mount one into a Service with
//! `volume add --mount` or `set SERVICE.mounts.VOLUME=/PATH`; `unset` detaches it and
//! keeps its data. Only a Deploy of a removed, deployed Volume deletes data, and it
//! asks for `--accept-volume-loss NAME`.

use clap::{ArgAction, ArgMatches, Command};
use ployz_core::ServiceName;
use ployz_store::{
    CreateVolume, Mount, RemoveVolume, VolumeId, VolumeQuery, VolumeStaged, VolumesQuery,
};

use super::store::{self, Next};
use super::{Error, leaf_matches};
use crate::cli::{base, positional, value};
use crate::failure::USAGE_EXIT;
use crate::output::{self, say};

pub(crate) fn command() -> Command {
    base("volume", "Manage Volumes")
        .arg_required_else_help(true)
        .subcommand(
            store::scoped(Command::new("add").about("Add a Volume; it is staged until a Deploy"))
                .arg(positional("name", true).help("Volume name, unique in the Environment"))
                .arg(
                    value("mount", None)
                        .action(ArgAction::Append)
                        .value_name("SERVICE:/PATH")
                        .help("Mount it into a Service at an absolute path; repeatable"),
                ),
        )
        .subcommand(
            store::scoped(Command::new("inspect").about("Show a Volume and where it is mounted"))
                .arg(positional("volume", true)),
        )
        .subcommand(store::scoped(
            Command::new("ls").about("List Volumes and what the next Deploy does to them"),
        ))
        .subcommand(
            store::scoped(
                Command::new("rm").about(
                    "Remove a Volume and detach it; a Deploy of the removal deletes its data",
                ),
            )
            .arg(positional("volume", true)),
        )
}

pub(super) fn handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "add" => add,
        "inspect" => inspect,
        "ls" => list,
        "rm" => remove,
        _ => return None,
    })
}

fn add(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = volume_name(matches, "name")?;
    let mounts = matches
        .get_many::<String>("mount")
        .into_iter()
        .flatten()
        .map(|mount| {
            let (service, path) = mount
                .split_once(':')
                .and_then(|(service, path)| Some((ServiceName::parse(service).ok()?, path)))
                .ok_or_else(|| {
                    Error::usage("Expected --mount SERVICE:/PATH, like web:/var/lib/data")
                        .with_exit(USAGE_EXIT)
                })?;
            Ok(Mount {
                service,
                path: path.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let mut words = vec!["volume", "add", name.as_str()];
    for _ in &mounts {
        words.extend(["--mount", "SERVICE:/PATH"]);
    }
    let created = store::store(root)?
        .create_volume(&CreateVolume {
            id: VolumeId::parse(store::mint())?,
            environment: store::environment(matches)?,
            name: name.clone(),
            mounts,
        })
        .map_err(store::failed(matches, &words))?;
    staged(matches, &created, "Staged new Volume")
}

fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store::store(root)?
        .volumes(&VolumesQuery {
            environment: store::environment(matches)?,
        })
        .map_err(store::failed(matches, &["volume", "ls"]))?;
    output::finish(&view, || {
        say!("VOLUME\tMOUNTS\tDEPLOYED\tNEXT DEPLOY");
        for listing in &view.volumes {
            let mounts: Vec<String> = listing
                .mounts
                .iter()
                .map(|mount| format!("{}:{}", mount.service, mount.path))
                .collect();
            say!(
                "{}\t{}\t{}\t{}",
                listing.volume.name,
                if mounts.is_empty() {
                    "-".to_owned()
                } else {
                    mounts.join(",")
                },
                if listing.deployed { "yes" } else { "no" },
                listing
                    .change
                    .as_ref()
                    .map_or("-".to_owned(), super::store::word)
            );
        }
    })
}

fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let view = store::store(root)?
        .volume(&VolumeQuery {
            environment: store::environment(matches)?,
            volume: volume.clone(),
        })
        .map_err(store::failed(
            matches,
            &["volume", "inspect", volume.as_str()],
        ))?;
    output::finish(&view, || {
        let listing = &view.volume;
        say!("Volume {} ({})", listing.volume.name, listing.volume.id);
        say!("Deployed: {}", if listing.deployed { "yes" } else { "no" });
        if let Some(change) = &listing.change {
            say!("Next Deploy: {}", super::store::word(change));
        }
        for mount in &listing.mounts {
            say!("Mounted by {} at {}", mount.service, mount.path);
        }
    })
}

fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let removed = store::store(root)?
        .remove_volume(&RemoveVolume {
            environment: store::environment(matches)?,
            volume: volume.clone(),
        })
        .map_err(store::failed(matches, &["volume", "rm", volume.as_str()]))?;
    staged(matches, &removed, "Staged removal of Volume")
}

/// A staged Volume change and `ployz diff` to review it.
fn staged(matches: &ArgMatches, result: &VolumeStaged, what: &str) -> Result<(), Error> {
    let hint = store::next(matches, &["diff"]);
    output::finish(&Next::new(result, Some(hint)), || {
        say!(
            "{what} {} in {}/{} (revision {}).",
            result.volume.name,
            result.environment.project,
            result.environment.name,
            result.environment.revision
        );
    })
}

use super::store::volume_name;
