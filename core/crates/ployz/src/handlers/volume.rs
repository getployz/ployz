//! Authored Volumes in the Config Store: add, list, inspect and remove. Every change
//! is staged in Working State until a Deploy ships it. Mount one into a Service with
//! `volume add --mount` or `set SERVICE.mounts.VOLUME=/PATH`; `unset` detaches it and
//! keeps its data. Only a Deploy of a removed, deployed Volume deletes data, and it
//! asks for `--accept-volume-loss NAME`.

use clap::{ArgAction, ArgMatches, Command};
use ployz_core::config::VolumeKind;
use ployz_core::{ProvisionedVolumeMaximumBytes, ServiceName};
use ployz_store::{
    CreateVolume, Mount, RemoveVolume, SetVolumeStorage, VolumeId, VolumeQuery, VolumeStaged,
    VolumesQuery,
};

use super::store::{self, Next};
use super::{Error, leaf_matches};
use crate::cli::{base, positional, switch, value};
use crate::failure::USAGE_EXIT;
use crate::output::{self, say};

pub(crate) fn command() -> Command {
    base("volume", "Manage Volumes")
        .arg_required_else_help(true)
        .subcommand(
            storage_flags(store::scoped(
                Command::new("add")
                    .about("Add a Volume with managed storage; staged until you deploy"),
            ))
            .arg(positional("name", true).help("Volume name, unique in the Environment"))
            .arg(
                value("mount", None)
                    .action(ArgAction::Append)
                    .value_name("SERVICE:/PATH")
                    .help("Mount it into a Service at an absolute path; repeatable"),
            ),
        )
        .subcommand(
            storage_flags(store::scoped(
                Command::new("set").about("Change a Volume's storage before its first deployment"),
            ))
            .arg(positional("volume", true))
            .arg(
                switch("managed", None)
                    .help("Use managed storage with the default 5 GB limit")
                    .conflicts_with("docker"),
            )
            .group(
                clap::ArgGroup::new("storage-change")
                    .args(["size", "docker", "managed"])
                    .required(true),
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
        "set" => set,
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
            storage: requested_storage(matches),
            mounts,
        })
        .map_err(store::failed(matches, &words))?;
    staged(matches, &created, "Staged new Volume")
}

fn storage_flags(command: Command) -> Command {
    command
        .arg(
            value("size", None)
                .value_name("SIZE")
                .value_parser(parse_size)
                .help("Storage limit, such as 500MB or 10GB; new managed Volumes default to 5GB"),
        )
        .arg(
            switch("docker", None)
                .conflicts_with("size")
                .help("Advanced: use a Docker volume without an enforced storage limit"),
        )
}

fn parse_size(value: &str) -> Result<ProvisionedVolumeMaximumBytes, String> {
    let units = [
        ("GiB", 1_073_741_824),
        ("MiB", 1_048_576),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("B", 1),
    ];
    let (number, multiplier) = units
        .iter()
        .find_map(|(suffix, multiplier)| {
            value
                .strip_suffix(suffix)
                .map(|number| (number, *multiplier))
        })
        .unwrap_or((value, 1));
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .and_then(|bytes| ProvisionedVolumeMaximumBytes::try_from(bytes).ok())
        .ok_or_else(|| "Use a positive whole-number size, such as 500MB, 5GB or 10GiB".into())
}

fn requested_storage(matches: &ArgMatches) -> VolumeKind {
    if matches.get_flag("docker") {
        VolumeKind::Local {}
    } else if let Some(maximum_bytes) = matches.get_one::<ProvisionedVolumeMaximumBytes>("size") {
        VolumeKind::Provisioned {
            maximum_bytes: *maximum_bytes,
        }
    } else {
        VolumeKind::managed_default()
    }
}

fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let changed = store::store(root)?
        .set_volume_storage(&SetVolumeStorage {
            environment: store::environment(matches)?,
            volume: volume.clone(),
            storage: requested_storage(matches),
            expect: None,
        })
        .map_err(store::failed(matches, &["volume", "set", volume.as_str()]))?;
    staged(matches, &changed, "Staged Volume storage")
}

fn storage_word(storage: VolumeKind) -> String {
    match storage {
        VolumeKind::Local {} => "Docker (no enforced storage limit)".into(),
        VolumeKind::Provisioned { maximum_bytes } => format!(
            "Managed ({} GB limit)",
            maximum_bytes.get() as f64 / 1_000_000_000.0
        ),
    }
}

fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store::store(root)?
        .volumes(&VolumesQuery {
            environment: store::environment(matches)?,
        })
        .map_err(store::failed(matches, &["volume", "ls"]))?;
    output::finish(&view, || {
        say!("VOLUME\tSTORAGE\tMOUNTS\tDEPLOYED\tNEXT DEPLOY");
        for listing in &view.volumes {
            let mounts: Vec<String> = listing
                .mounts
                .iter()
                .map(|mount| format!("{}:{}", mount.service, mount.path))
                .collect();
            say!(
                "{}\t{}\t{}\t{}\t{}",
                listing.volume.name,
                storage_word(listing.volume.storage),
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
        say!("Storage: {}", storage_word(listing.volume.storage));
        say!(
            "Storage settings: {}",
            if listing.storage_locked {
                "locked after deployment was requested"
            } else {
                "editable before deployment"
            }
        );
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
        say!("Storage: {}", storage_word(result.volume.storage));
    })
}

use super::store::volume_name;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn storage_sizes_are_positive_exact_and_cannot_overflow() {
        assert_eq!(parse_size("5GB").unwrap().get(), 5_000_000_000);
        assert_eq!(parse_size("500MB").unwrap().get(), 500_000_000);
        assert_eq!(parse_size("1GiB").unwrap().get(), 1_073_741_824);
        for value in ["0", "-1GB", "1.5GB", "unknown", "18446744073709551615GB"] {
            assert!(parse_size(value).is_err());
        }
    }
}
