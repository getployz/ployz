//! Authored Volumes in the Config Store: add, list, inspect and remove. Every change
//! is staged in Working State until a Deploy ships it. Mount one into a Service with
//! `volume add --mount` or `set SERVICE.mounts.VOLUME=/PATH`; `unset` detaches it and
//! keeps its data. Only a Deploy of a removed, deployed Volume deletes data, and it
//! asks for `--accept-volume-loss NAME`.

use clap::{ArgAction, ArgMatches, Command};
use ployz_core::config::VolumeKind;
use ployz_core::{
    MachineObservation, MachineStorageObservation, ProvisionedVolumeMaximumBytes, ServiceName,
};
use ployz_store::{
    CreateVolume, Mount, RemoveVolume, SetVolumeSharedWrites, SetVolumeStorage, VolumeId,
    VolumeQuery, VolumeStaged, VolumesQuery,
};

use super::store::{self, Next};
use super::{Error, leaf_matches};
use crate::cli::{base, positional, switch, value};

pub(crate) fn command() -> Command {
    base("volume", "Manage Volumes")
        .arg_required_else_help(true)
        .subcommand(
            storage_flags(store::scoped(
                Command::new("add")
                    .about("Add a Managed volume (or --docker); staged until you deploy"),
            ))
            .arg(positional("name", true).help("Volume name, unique in the Environment"))
            .arg(
                value("mount", None)
                    .action(ArgAction::Append)
                    .value_name("SERVICE:/PATH")
                    .help("Mount it into a Service at an absolute path; repeatable"),
            )
            .arg(shared_writes_flag()),
        )
        .subcommand(
            storage_flags(store::scoped(
                Command::new("set").about(
                    "Change a Volume's storage before its first deployment, or whether it allows shared writes",
                ),
            ))
            .arg(positional("volume", true))
            .arg(shared_writes_flag())
            .group(
                clap::ArgGroup::new("volume-change")
                    .args(["size", "docker", "shared-writes"])
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
                Command::new("rename").about("Rename a Volume: staged; its data and mounts stay"),
            )
            .arg(positional("volume", true))
            .arg(positional("name", true)),
        )
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
        "rename" => rename,
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
                })?;
            Ok(Mount {
                service,
                path: path.to_owned(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let storage = requested_storage(matches);
    let created = store::store(root)?.write(&CreateVolume {
        id: VolumeId::parse(store::mint())?,
        environment: store::environment(matches)?,
        name: name.clone(),
        storage,
        mounts,
        shared_writes: matches.get_one("shared-writes") == Some(&true),
    })?;
    if matches!(storage, VolumeKind::Provisioned { .. }) && no_managed_host(matches) {
        crate::ui::warn(NO_MANAGED_HOST);
    }
    staged(matches, &created, "Staged new Volume", true)
}

const NO_MANAGED_HOST: &str = "No Server here can host Managed volumes yet, so a Deploy of this one fails until one can: add a Server with Managed volumes, or use --docker instead.";

/// Why `--docker` is not recommended; said when a command asks for one.
const DOCKER_VOLUME: &str = "Docker volume (not recommended): no size limit, and it stays out of backups and Server moves as they arrive.";

/// Best effort, bounded: whether every Server this reaches reports it can't host
/// managed Volumes. Unreachable or unknown answers `false`; the Deploy still checks.
fn no_managed_host(matches: &ArgMatches) -> bool {
    let Ok(runtime) = super::runtime() else {
        return false;
    };
    let context = matches.try_get_one::<String>("context").ok().flatten();
    runtime.block_on(async {
        let observe = async {
            let mut client = super::server::connect(matches, context.map(String::as_str))
                .await
                .ok()?;
            let mut machines = client.machines().await.ok()?;
            client.observe_machine_storage(&mut machines).await;
            Some(none_hosts_managed(&machines))
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), observe)
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
    })
}

/// `--shared-writes[=true|false]`: let more than one container write the Volume.
fn shared_writes_flag() -> clap::Arg {
    value("shared-writes", None)
        .num_args(0..=1)
        .require_equals(true)
        .default_missing_value("true")
        .value_parser(clap::value_parser!(bool))
        .help("Let more than one container write it: replicas of one Service, or several Services. =false refuses a second writer")
}

fn storage_flags(command: Command) -> Command {
    command
        .arg(
            value("size", None)
                .value_name("GB")
                .value_parser(|size: &str| {
                    ProvisionedVolumeMaximumBytes::parse_gb(size)
                        .map_err(|_| "Use a positive size in GB, such as 5GB or 0.5GB")
                })
                .help("Managed volume size limit in GB, such as 10GB; new Volumes default to 5GB"),
        )
        .arg(
            switch("docker", None)
                .conflicts_with("size")
                .help("Use a plain Docker volume, with no size limit (not recommended)"),
        )
}

fn requested_storage(matches: &ArgMatches) -> VolumeKind {
    if matches.get_flag("docker") {
        crate::ui::warn(DOCKER_VOLUME);
        VolumeKind::Docker {}
    } else if let Some(maximum_bytes) = matches.get_one::<ProvisionedVolumeMaximumBytes>("size") {
        VolumeKind::Provisioned {
            maximum_bytes: *maximum_bytes,
        }
    } else {
        VolumeKind::provisioned_default()
    }
}

fn set(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    if let Some(shared_writes) = matches.get_one::<bool>("shared-writes") {
        let changed = store::store(root)?.write(&SetVolumeSharedWrites {
            environment: store::environment(matches)?,
            volume,
            shared_writes: *shared_writes,
        })?;
        return crate::ui::finish(&changed, || {
            crate::ui::stream(format_args!(
                "Shared writes {} for Volume {} in {}/{}; this applies now.",
                shared_writes_word(changed.volume.shared_writes),
                changed.volume.name,
                changed.environment.project,
                changed.environment.name,
            ));
        });
    }
    let changed = store::store(root)?.write(&SetVolumeStorage {
        environment: store::environment(matches)?,
        volume: volume.clone(),
        storage: requested_storage(matches),
    })?;
    staged(matches, &changed, "Staged Volume storage", true)
}

fn shared_writes_word(shared_writes: bool) -> &'static str {
    if shared_writes { "on" } else { "off" }
}

fn storage_word(storage: VolumeKind) -> String {
    match storage {
        VolumeKind::Docker {} => "Docker volume (no size limit)".into(),
        VolumeKind::Provisioned { maximum_bytes } => {
            format!("Managed volume ({maximum_bytes} limit)")
        }
    }
}

fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store::store(root)?.read(&VolumesQuery {
        environment: store::environment(matches)?,
    })?;
    crate::ui::finish(&view, || {
        crate::ui::stream(format_args!(
            "VOLUME\tSTORAGE\tSHARED WRITES\tMOUNTS\tDEPLOYED\tNEXT DEPLOY"
        ));
        for listing in &view.volumes {
            let mounts: Vec<String> = listing
                .mounts
                .iter()
                .map(|mount| format!("{}:{}", mount.service, mount.path))
                .collect();
            crate::ui::stream(format_args!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                listing.volume.name,
                storage_word(listing.volume.storage),
                shared_writes_word(listing.volume.shared_writes),
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
            ));
        }
    })
}

fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let view = store::store(root)?.read(&VolumeQuery {
        environment: store::environment(matches)?,
        volume: volume.clone(),
    })?;
    crate::ui::finish(&view, || {
        let listing = &view.volume;
        crate::ui::stream(format_args!(
            "Volume {} ({})",
            listing.volume.name, listing.volume.id
        ));
        crate::ui::stream(format_args!(
            "Storage: {}",
            storage_word(listing.volume.storage)
        ));
        crate::ui::stream(format_args!(
            "Storage settings: {}",
            if listing.storage_locked {
                "locked after deployment was requested"
            } else {
                "editable before deployment"
            }
        ));
        crate::ui::stream(format_args!(
            "Shared writes: {}",
            shared_writes_word(listing.volume.shared_writes)
        ));
        crate::ui::stream(format_args!(
            "Deployed: {}",
            if listing.deployed { "yes" } else { "no" }
        ));
        if let Some(change) = &listing.change {
            crate::ui::stream(format_args!("Next Deploy: {}", super::store::word(change)));
        }
        for mount in &listing.mounts {
            crate::ui::stream(format_args!(
                "Mounted by {} at {}",
                mount.service, mount.path
            ));
        }
    })
}

fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let name = volume_name(matches, "name")?;
    let renamed = store::store(root)?.write(&ployz_store::RenameVolume {
        environment: store::environment(matches)?,
        volume: volume.clone(),
        name: name.clone(),
    })?;
    staged(matches, &renamed, "Staged rename of Volume", false)
}

fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let removed = store::store(root)?.write(&RemoveVolume {
        environment: store::environment(matches)?,
        volume: volume.clone(),
    })?;
    staged(matches, &removed, "Staged removal of Volume", false)
}

/// A staged Volume change and `ployz diff` to review it.
/// Servers were seen and each says it is Docker only; one not
/// answering might host them.
fn none_hosts_managed(machines: &[MachineObservation]) -> bool {
    !machines.is_empty()
        && machines
            .iter()
            .all(|machine| machine.storage == Some(MachineStorageObservation::Stateless))
}

/// `storage`: say the Volume's storage too, when the change set it.
fn staged(
    matches: &ArgMatches,
    result: &VolumeStaged,
    what: &str,
    storage: bool,
) -> Result<(), Error> {
    let hint = store::next(matches, &["diff"]);
    crate::ui::finish(&Next::new(result, Some(hint)), || {
        crate::ui::stream(format_args!(
            "{what} {} in {}/{} (revision {}).",
            result.volume.name,
            result.environment.project,
            result.environment.name,
            result.environment.revision
        ));
        if storage {
            crate::ui::stream(format_args!(
                "Storage: {}",
                storage_word(result.volume.storage)
            ));
        }
    })
}

use super::store::volume_name;

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{Machine, MembershipObservation, WireGuardPublicKey};

    fn server(seed: u8, storage: Option<MachineStorageObservation>) -> MachineObservation {
        let mut observed = MachineObservation::new(
            Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: char::from(b'a' + seed)
                    .to_string()
                    .repeat(32)
                    .parse()
                    .unwrap(),
                name: format!("node-{seed}").parse().unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            MembershipObservation::Up,
        );
        observed.storage = storage;
        observed
    }

    #[test]
    fn only_servers_that_all_say_docker_only_warn() {
        let stateless = Some(MachineStorageObservation::Stateless);
        assert!(none_hosts_managed(&[
            server(0, stateless),
            server(1, stateless)
        ]));
        assert!(!none_hosts_managed(&[]));
        assert!(!none_hosts_managed(&[
            server(0, stateless),
            server(1, None)
        ]));
        assert!(!none_hosts_managed(&[
            server(0, stateless),
            server(1, Some(MachineStorageObservation::Ready)),
        ]));
    }
}
