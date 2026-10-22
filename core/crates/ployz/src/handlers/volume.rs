//! Authored Volumes in the Config Store: add, list, inspect and remove. Every change
//! is staged in Working State until a Deploy ships it. Mount one into a Service with
//! `volume add --mount` or `set SERVICE.mounts.VOLUME=/PATH`; `unset` detaches it and
//! keeps its data. Only a Deploy of a removed, deployed Volume deletes data, and it
//! asks for `--accept-volume-loss NAME`.
//!
//! Mirror, Sync and Mirror removal are Volume runs in Ployz Cloud; see [`run`].

use clap::{ArgAction, ArgMatches, Command};
use ployz_core::config::VolumeKind;
use ployz_core::{
    MachineObservation, MachineStorageObservation, ProvisionedVolumeMaximumBytes, ServiceName,
};
use ployz_store::{
    CreateVolume, Mount, RemoveVolume, SetVolumeSharedWrites, SetVolumeStorage, VolumeId,
    VolumeListing, VolumeName, VolumeQuery, VolumeStaged, VolumesQuery,
};

use super::store::{self, Next};
use super::{Error, leaf_matches};
use crate::cli::{base, positional, switch, value};
use crate::ui::{Cell, Fields, Hint, Table, Tone};

pub(super) mod run;

pub(super) fn group(command: Command) -> Command {
    base(command, "Manage Volumes")
}

pub(super) fn add_command(command: Command) -> Command {
    storage_flags(store::scoped(
        command.about("Add a Managed volume (or --docker); staged until you deploy"),
    ))
    .arg(positional("name", true).help("Volume name, unique in the Environment"))
    .arg(
        value("mount", None)
            .action(ArgAction::Append)
            .value_name("SERVICE:/PATH")
            .help("Mount it into a Service at an absolute path; repeatable"),
    )
    .arg(shared_writes_flag())
}

pub(super) fn set_command(command: Command) -> Command {
    storage_flags(store::scoped(command.about(
        "Change a Volume's storage before its first deployment, resize a deployed one (Ployz Cloud), or whether it allows shared writes",
    )))
    .arg(positional("volume", true).help("Volume name"))
    .arg(shared_writes_flag())
    .arg(run::wait_flag().conflicts_with_all(["docker", "shared-writes"]))
    .arg(crate::cli::approval().conflicts_with_all(["docker", "shared-writes"]))
    .group(
        clap::ArgGroup::new("volume-change")
            .args(["size", "docker", "shared-writes"])
            .required(true),
    )
}

pub(super) fn inspect_command(command: Command) -> Command {
    store::scoped(command.about("Show a Volume and where it is mounted"))
        .arg(positional("volume", true).help("Volume name"))
}

pub(super) fn list_command(command: Command) -> Command {
    store::scoped(command.about("List Volumes and what the next Deploy does to them"))
}

pub(super) fn rename_command(command: Command) -> Command {
    store::scoped(command.about("Rename a Volume: staged; its data and mounts stay"))
        .arg(positional("volume", true).help("Volume name"))
        .arg(positional("name", true).help("Its new name"))
}

pub(super) fn remove_command(command: Command) -> Command {
    store::scoped(
        command.about("Remove a Volume and detach it; a Deploy of the removal deletes its data"),
    )
    .arg(positional("volume", true).help("Volume name"))
}

pub(super) fn add(root: &ArgMatches) -> Result<(), Error> {
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
        storage_locked: false,
        restored_from: None,
    })?;
    warn_docker(storage);
    if matches!(storage, VolumeKind::Provisioned { .. }) && no_managed_host(matches) {
        crate::ui::warn(NO_MANAGED_HOST);
    }
    staged(matches, &created, "Staged new Volume", "", true)
}

const NO_MANAGED_HOST: &str = "No Server here can host Managed volumes yet, so a Deploy of this one fails until one can: add a Server with Managed volumes, or use --docker instead.";

/// Why `--docker` is not recommended; said when a command asks for one.
const DOCKER_VOLUME: &str = "Docker volume (not recommended): no size limit, and it stays out of backups and Server moves as they arrive.";
const STAGED_NOT_WAITED: &str = "Nothing to wait for: this size is staged for the next Deploy, and only a deployed Volume resizes live.";

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
        VolumeKind::Docker {}
    } else if let Some(maximum_bytes) = matches.get_one::<ProvisionedVolumeMaximumBytes>("size") {
        VolumeKind::Provisioned {
            maximum_bytes: *maximum_bytes,
        }
    } else {
        VolumeKind::provisioned_default()
    }
}

pub(super) fn set(root: &ArgMatches) -> Result<(), Error> {
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
    set_storage(
        root,
        &store::store(root)?,
        volume,
        requested_storage(matches),
    )
}

/// What `volume set --size/--docker` does with the Volume, read off the Store's own listing.
#[derive(Debug, PartialEq)]
enum StorageChange {
    /// A staged `SetVolumeStorage`: before the first Deploy, or a change of kind, which the
    /// Store refuses on a deployed Volume with its own message.
    Stage(VolumeKind),
    /// A deployed Managed Volume asked for a Managed size: a resize run on Ployz Cloud. A
    /// smaller size shrinks it, and a Server refuses one below its floor. The size it already
    /// has runs a settle on Ployz Cloud, which asks every copy to hold it, so a copy a refused
    /// or interrupted shrink left larger is shrunk to it; over a restore of an older size, the
    /// size the Servers hold stages back.
    Resize(ProvisionedVolumeMaximumBytes),
}

fn storage_change(listing: &VolumeListing, asked: VolumeKind) -> StorageChange {
    match (
        listing.storage_locked,
        listing.volume.storage,
        asked,
        listing.recorded_storage,
    ) {
        (
            true,
            VolumeKind::Provisioned {
                maximum_bytes: current,
            },
            VolumeKind::Provisioned { maximum_bytes },
            Some(recorded),
        ) if maximum_bytes != recorded || current == recorded => {
            StorageChange::Resize(maximum_bytes)
        }
        _ => StorageChange::Stage(asked),
    }
}

fn set_storage(
    root: &ArgMatches,
    store: &store::Store<'_>,
    volume: VolumeName,
    asked: VolumeKind,
) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let environment = store::environment(matches)?;
    let view = store.read(&VolumeQuery {
        environment: environment.clone(),
        volume: volume.clone(),
    })?;
    let storage = match storage_change(&view.volume, asked) {
        StorageChange::Stage(storage) => storage,
        StorageChange::Resize(maximum_bytes) => {
            return run::resize(root, store, &view, maximum_bytes);
        }
    };
    let changed = store.write(&SetVolumeStorage {
        environment,
        volume,
        storage,
    })?;
    if matches.get_flag("wait") {
        crate::ui::warn(STAGED_NOT_WAITED);
    }
    warn_docker(storage);
    staged(
        matches,
        &changed,
        "Staged Volume storage",
        "already has that storage",
        true,
    )
}

/// Said once the store took a Docker volume, so a refused change stays quiet.
fn warn_docker(storage: VolumeKind) {
    if matches!(storage, VolumeKind::Docker {}) {
        crate::ui::warn(DOCKER_VOLUME);
    }
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

pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let view = store::store(root)?.read(&VolumesQuery {
        environment: store::environment(matches)?,
    })?;
    let mut table = Table::new(
        [
            "VOLUME",
            "STORAGE",
            "SHARED WRITES",
            "MOUNTS",
            "DEPLOYED",
            "NEXT DEPLOY",
        ],
        format!("No Volumes in {} yet.", view.environment.name),
    );
    for listing in &view.volumes {
        let mounts: Vec<String> = listing
            .mounts
            .iter()
            .map(|mount| format!("{}:{}", mount.service, mount.path))
            .collect();
        table.row([
            Cell::from(listing.volume.name.to_string()),
            Cell::from(storage_word(listing.volume.storage)),
            Cell::from(shared_writes_word(listing.volume.shared_writes)),
            Cell::from(mounts.join(",")),
            if listing.deployed {
                Cell::status("yes", Tone::Good)
            } else {
                Cell::from("no")
            },
            listing.change.as_ref().map_or_else(
                || Cell::from(""),
                |change| Cell::status(super::store::word(change), Tone::Change),
            ),
        ]);
    }
    crate::ui::list(&view, &table)
}

pub(super) fn inspect(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let view = store::store(root)?.read(&VolumeQuery {
        environment: store::environment(matches)?,
        volume: volume.clone(),
    })?;
    let listing = &view.volume;
    let mut record = Fields::new()
        .field("volume", &listing.volume.name)
        .field("storage", storage_word(listing.volume.storage))
        .field(
            "storage settings",
            if listing.storage_locked {
                match listing.volume.storage {
                    VolumeKind::Provisioned { .. } => {
                        "deployed; ployz volume set --size resizes it (Ployz Cloud)"
                    }
                    VolumeKind::Docker {} => "locked after deployment was requested",
                }
            } else {
                "editable before deployment"
            },
        )
        .field(
            "shared writes",
            shared_writes_word(listing.volume.shared_writes),
        )
        .field("deployed", if listing.deployed { "yes" } else { "no" });
    if let Some(change) = &listing.change {
        record.push("next deploy", super::store::word(change));
    }
    for mount in &listing.mounts {
        record.push(
            "mounted by",
            format_args!("{} at {}", mount.service, mount.path),
        );
    }
    crate::ui::fields(&view, &record)
}

pub(super) fn rename(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let name = volume_name(matches, "name")?;
    let renamed = store::store(root)?.write(&ployz_store::RenameVolume {
        environment: store::environment(matches)?,
        volume: volume.clone(),
        name: name.clone(),
    })?;
    staged(
        matches,
        &renamed,
        "Staged rename of Volume",
        "already has that name",
        false,
    )
}

pub(super) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let volume = volume_name(matches, "volume")?;
    let removed = store::store(root)?.write(&RemoveVolume {
        environment: store::environment(matches)?,
        volume: volume.clone(),
    })?;
    staged(matches, &removed, "Staged removal of Volume", "", false)
}

/// Servers were seen and each says it is Docker only; one not
/// answering might host them.
fn none_hosts_managed(machines: &[MachineObservation]) -> bool {
    !machines.is_empty()
        && machines
            .iter()
            .all(|machine| machine.storage == Some(MachineStorageObservation::Stateless))
}

/// A staged Volume change and `ployz deploy` to ship it. A change that was already
/// so says nothing changed: `already` follows the Volume, as in `Volume data already
/// has that name`. `storage`: say the Volume's storage too, when the change set it.
fn staged(
    matches: &ArgMatches,
    result: &VolumeStaged,
    what: &str,
    already: &str,
    storage: bool,
) -> Result<(), Error> {
    if result.staged.is_empty() {
        return crate::ui::done(
            &Next::new(result, None),
            format_args!(
                "Nothing changed in {}/{}; Volume {} {already}.",
                result.environment.project, result.environment.name, result.volume.name
            ),
        );
    }
    let hint = store::next(matches, &["deploy"]);
    crate::ui::finish(&Next::new(result, Some(hint.clone())), || {
        crate::ui::stream(format_args!(
            "{what} {} in {}/{}.",
            result.volume.name, result.environment.project, result.environment.name
        ));
        if storage {
            crate::ui::stream(format_args!(
                "Storage: {}",
                storage_word(result.volume.storage)
            ));
        }
        crate::ui::hint(&Hint::Next(hint));
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
    fn only_a_deployed_managed_volume_asked_for_a_size_resizes() {
        let gb = |size| ProvisionedVolumeMaximumBytes::parse_gb(size).unwrap();
        let listing = |storage_locked, storage, recorded_storage| VolumeListing {
            volume: ployz_store::VolumeSummary {
                id: VolumeId::parse(store::mint()).unwrap(),
                name: VolumeName::parse("data").unwrap(),
                storage,
                shared_writes: false,
            },
            mounts: Vec::new(),
            deployed: storage_locked,
            storage_locked,
            recorded_storage,
            change: None,
        };
        let managed = VolumeKind::Provisioned {
            maximum_bytes: gb("10"),
        };
        let twenty = VolumeKind::Provisioned {
            maximum_bytes: gb("20"),
        };
        let docker = VolumeKind::Docker {};
        let thirty = VolumeKind::Provisioned {
            maximum_bytes: gb("30"),
        };
        for (locked, storage, recorded, asked, change) in [
            (false, managed, None, twenty, StorageChange::Stage(twenty)),
            (
                true,
                managed,
                Some(gb("10")),
                twenty,
                StorageChange::Resize(gb("20")),
            ),
            (
                true,
                managed,
                Some(gb("10")),
                managed,
                StorageChange::Resize(gb("10")),
            ),
            (
                true,
                managed,
                Some(gb("20")),
                managed,
                StorageChange::Resize(gb("10")),
            ),
            (
                true,
                managed,
                Some(gb("10")),
                docker,
                StorageChange::Stage(docker),
            ),
            (
                true,
                twenty,
                Some(gb("20")),
                managed,
                StorageChange::Resize(gb("10")),
            ),
            (true, docker, None, twenty, StorageChange::Stage(twenty)),
            // A restore staged the older size: the size the Servers hold stages back,
            // another size resizes.
            (
                true,
                managed,
                Some(gb("20")),
                twenty,
                StorageChange::Stage(twenty),
            ),
            (
                true,
                managed,
                Some(gb("20")),
                thirty,
                StorageChange::Resize(gb("30")),
            ),
        ] {
            assert_eq!(
                storage_change(&listing(locked, storage, recorded), asked),
                change,
                "locked {locked}, {storage:?} recorded {recorded:?} asked {asked:?}"
            );
        }
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
