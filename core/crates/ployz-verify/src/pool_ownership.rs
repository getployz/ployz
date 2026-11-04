//! Who owns a Machine's Machine Pool, between the installer that creates a device Pool on
//! a selected disk and the volume plugin that creates a root-backed Pool when it finds
//! none. The installer's named-device path (ployzd installer/storage.rs `prepare_storage`,
//! :40-73) checks the host, runs `zpool create ployz-device` (`create_device_pool`,
//! :96-106), checks the Pool with `existing_device_pool` (:107-112), and only then records
//! ownership with `remember_device_pool` (:113), an atomic write of the CONFIRMED marker
//! (:77-83). `install_at` holds the installation gate's exclusive guard throughout
//! (installer/mod.rs:256-261, :286). The plugin's `VolumeStorage::create`
//! (volume_plugin/pool.rs:97-125) takes that gate's shared guard (`admit_mutation`,
//! :103, volume_plugin/storage.rs:178-188), then the Pool mutation lock (pool.rs:104), then
//! runs `one_usable` (:214), `recover` (:277) and, when both find nothing, `create`
//! (:391), which builds the root-backed `ployz` Pool on a backing file. Copy's `first_pool`
//! takes the same path (volume_plugin/copy.rs:146-160, :175-196). Meanwhile the disk can go
//! missing and return, ZFS imports at boot the Pools its cache file lists, and
//! `local_storage` (ployzd machine/mod.rs:691) reports what `zpool list` shows.
//!
//! How ZFS imports at boot comes from a reboot run on Ubuntu 24.04.4 with ZFS 2.2.2 (PR #3842's
//! Evidence). Neither `zpool create` sets a cachefile, so
//! both Pools land in the default cache file. A boot without the disk fails to import
//! `ployz-device` and leaves the file as it is. A later Pool change rewrites the file from
//! the imported Pools (OpenZFS module/zfs/spa_config.c `spa_write_cachefile`).
//! `zfs-import-cache` imports only what the file lists, and nothing imports a disk attached
//! later.
//!
//! The finding needs a volume plugin installed before the installer creates `ployz-device`.
//! `install_locked` runs `prepare_storage` before it installs the binaries and units and
//! enables the plugin's socket (installer/mod.rs:286, :298-301, installer/host.rs:262), so
//! a fresh host has no plugin until a run completes. An earlier `ployzd install`, whose
//! `--storage` defaults to `none` (ployzd main.rs:108-109), installs both and enables the
//! socket whatever its storage, and a later `ployzd install --storage zfs:<device>` runs
//! `prepare_storage` again (installer/mod.rs:280-286); docs/user/servers/add-a-server.md:207-208
//! names that command. The model starts from either host.
//!
//! Docker's List (volume_plugin/removal.rs:96-104) and Inspect (:48-53) take the Pool
//! mutation lock but not the installation gate, and run `admit`: `one_usable`, which
//! writes CONFIRMED for an imported `ployz-device` (pool.rs:237-246), then `recover`. The
//! model has them as two steps, offered whenever a release is installed, including while
//! the installer runs: `List`, which is `one_usable`, then `ListRecover`, which is `recover`
//! when `one_usable` found nothing. The installer is another process (`ployzd install`),
//! so it can step between any two of a List's reads, not only at an await. In
//! `one_usable`, a `Record` between `zpool list` (:221) and the marker read or write
//! (:239-245) leaves the CONFIRMED marker the List would have written. In `recover`, the
//! `zpool import` scan (:287) never lists `ployz-device` while the installer runs, since
//! the disk is blank or `zpool create` imported it, and the backing-file reads (:278-279)
//! see no file, which the installer never writes. So an installer step between the scan
//! and the marker read (:308) changes only that read: the List refuses with the device
//! imported, as a `ListRecover` placed after `Record` does
//! (`a_list_across_the_install_refuses_beside_the_imported_device`), or, with no `Record`
//! yet, returns `Ok(None)`, as one placed before it does. The import branch (:301-306)
//! needs a labelled disk not imported, which the bounds never pair with a running
//! installer. On an enrolled Machine the daemon's Docker volume rescan
//! calls List every 30 s (ployzd docker/observe.rs:20, attached by
//! `ContainerRuntime::replicating`, :130-143). The model has no clock, so it cannot force
//! that rescan to run, and a rerun of the failed install, which also records the Pool
//! (installer/storage.rs:57-58), is out of its bounds. So on a real host the finding needs
//! the disk missing before any of them runs: an operator's `ployzd install` before
//! `ployz server add`, or a host that crashes right after `zpool create` and boots without
//! the disk.
//!
//! The finding: the installer can stop between `zpool create` and `remember_device_pool`,
//! by a crash, or by `existing_device_pool`'s backing check refusing with `ployz-device`
//! imported (installer/storage.rs:214). The disk then carries a
//! `ployz-device` label and the host has no marker. Once the disk is missing, `recover`
//! finds no importable device Pool and no marker and returns `Ok(None)` (pool.rs:314),
//! `create` passes its marker-only refusal (:392-396), and the plugin builds a root-backed
//! `ployz` beside the labelled disk (:401, :422). That `zpool create` rewrites the cache file
//! from the imported Pools alone, so no boot imports `ployz-device` again, and `admit`
//! returns `ployz` from `one_usable` without reaching `recover` (pool.rs:256-261). And
//! `local_storage` reads Ready while a recorded device Pool's disk is missing
//! (machine/mod.rs:710).
//!
//! Out of scope: a re-run of the installer, the plain `--storage zfs` path and its
//! `reconcile_device_pool_marker`, a legacy or partial marker, a second disk, a disk that
//! vanishes under an imported Pool, a boot during an installation or a Volume create, an
//! operator's own `zpool` commands, and Volume operations other than List, Inspect and
//! creating the first Pool. So is `create`'s cleanup (pool.rs:455-469), which runs when
//! `create`'s `one_usable` finds a Pool other than `ployz`: between `recover` and `create`
//! the gate keeps the installer out, and only a boot or an operator imports `ployz-device`.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// The disk the installer was given (`StoragePreparation::Zfs { device: Some(..) }`,
/// installer/storage.rs `prepare_storage`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Disk {
    /// The host sees the disk.
    present: bool,
    /// The disk carries a `ployz-device` ZFS label.
    label: bool,
    /// `ployz-device` is imported. The model takes a missing disk to mean the host booted
    /// without it, so ZFS lists nothing for it.
    imported: bool,
}

/// The host's release, its Pool files and the root-backed Pool.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Host {
    /// A release is installed with its units, so ployzd and its volume plugin answer
    /// (installer/mod.rs:298-301, installer/host.rs:253-266).
    release: bool,
    /// The root-backed `ployz` Pool exists on `/var/lib/ployz-machine-pool`, imported
    /// (machine_pool.rs `BACKING_FILE`).
    root: bool,
    /// `/var/lib/ployz-machine-pool.device` holds CONFIRMED (machine_pool.rs
    /// `DEVICE_POOL_MARKER`, `CONFIRMED_DEVICE_POOL_MARKER`).
    marker: bool,
}

/// Where the installer is in `prepare_storage` and `create_device_pool`
/// (installer/storage.rs). In every phase but `Start`, `Done` and `Stopped`, `install_at`
/// holds the installation gate's exclusive guard (installer/mod.rs:256-261).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Installer {
    /// `install_at` has not run.
    Start,
    /// The checks passed; `zpool create` (:96) is next.
    Assessed,
    /// `zpool create ployz-device` ran; `existing_device_pool` (:107) is next.
    Created,
    /// `existing_device_pool` confirmed the Pool; `remember_device_pool` (:113) is next.
    Verified,
    /// `install_at` returned, after `prepare_storage` recorded or refused, or at once
    /// when the gate was busy, and dropped its guard.
    Done,
    /// The installer stopped between two steps; the OS released its guard.
    Stopped,
}

impl Installer {
    /// `install_at` holds the installation gate's exclusive guard.
    fn running(self) -> bool {
        !matches!(self, Self::Start | Self::Done | Self::Stopped)
    }
}

/// Where one Volume create is in `VolumeStorage::create` (volume_plugin/pool.rs), or
/// copy's `first_pool`, which takes the same path, or where a List or Inspect is.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Plugin {
    /// No request holds the installation gate's shared guard or the Pool mutation lock.
    Idle,
    /// The request holds both (`admit_mutation`, :103; `lock_mutation`, :104);
    /// `one_usable` (:214) is next.
    Admitting,
    /// `one_usable` found nothing imported; `recover` (:277) is next.
    Recovering,
    /// `recover` returned `Ok(None)`; `PoolStorage::create` (:391) is next.
    Creating,
    /// A List's or Inspect's `one_usable` found nothing imported; its `recover` is next,
    /// under the Pool mutation lock and without the installation gate (pool.rs:256-261,
    /// volume_plugin/removal.rs:96-104, :48-53).
    Listing,
}

impl Plugin {
    /// A Volume create holds the installation gate's shared guard; a List holds only the
    /// Pool mutation lock.
    fn holds_gate(self) -> bool {
        matches!(self, Self::Admitting | Self::Recovering | Self::Creating)
    }
}

/// What `local_storage` (ployzd machine/mod.rs:691-719) answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Observed {
    /// `MachineStorageObservation::Ready`: `zpool list` lists no Pool (:710).
    Ready,
    /// `MachineStorageObservation::Pool`: one usable Pool (:705-709).
    Pool,
    /// `None`: `machine_pool::one_usable` refused two usable Pools (:711,
    /// machine_pool.rs:84-91). Not reached in the bounds.
    Ambiguous,
}

/// The branch the last step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// `admit_mutation`'s `try_mutation` refused: "a Ployz installation or upgrade is
    /// active" (volume_plugin/storage.rs:178-188, ployzd mutation.rs:55-69).
    InstallationActive,
    /// `install_at`'s `try_installation` refused at once while a Volume create holds the
    /// shared guard (ployzd mutation.rs:77-81, installer/mod.rs:256-257). The installer
    /// prints "another Ployz installation is active" (installer/mod.rs:93-94, :342-350).
    InstallerBusy,
    /// `prepare_storage` refuses beside a root-backed Pool: the backing check
    /// (installer/storage.rs:48-53), or `existing_device_pool` finding `ployz`
    /// (:178-187).
    InstallerRefusesRoot,
    /// `create_device_pool`'s `existing_device_pool` refused (installer/storage.rs:107-112):
    /// `machine_pool::one_usable` found two usable Pools (machine_pool.rs:84-91). Not
    /// reached in the bounds.
    VerifyRefuses,
    /// `create_device_pool`'s `remember_device_pool` wrote CONFIRMED
    /// (installer/storage.rs:113).
    Records,
    /// `PoolStorage::one_usable`'s `machine_pool::one_usable` refused two usable Pools
    /// (volume_plugin/pool.rs:225-226, machine_pool.rs:84-91). Not reached in the
    /// bounds.
    Ambiguous,
    /// `PoolStorage::one_usable` refused the backing file beside device ownership
    /// (volume_plugin/pool.rs:229-236). Not reached in the bounds.
    Conflict,
    /// `PoolStorage::one_usable` returned `ployz-device` and wrote CONFIRMED
    /// (volume_plugin/pool.rs:237-246).
    Confirms,
    /// `PoolStorage::one_usable` returned `ployz-device` without writing CONFIRMED: it
    /// was there (volume_plugin/pool.rs:239-242).
    UsesDevice,
    /// `PoolStorage::one_usable` returned `ployz` (volume_plugin/pool.rs:225-249).
    UsesRoot,
    /// `recover` imported the one importable `ployz-device` (volume_plugin/pool.rs:301-306).
    RecoverImports,
    /// `recover` refused: no importable device Pool and the marker exists
    /// (volume_plugin/pool.rs:308-313).
    RefusesOwned,
    /// `recover` returned `Ok(None)` (volume_plugin/pool.rs:314).
    RecoverNone,
    /// `PoolStorage::create` refused on the marker (volume_plugin/pool.rs:392-396). Not
    /// reached in the bounds, since `recover` read the same marker under the same gate and
    /// lock.
    CreateRefusesOwned,
    /// `PoolStorage::create` built `ployz`, and its `one_usable` returned it
    /// (volume_plugin/pool.rs:401-454).
    CreatesRoot,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// The host boots without the disk. ZFS's cache-file import of `ployz-device` fails and
    /// leaves the cache file as it is (verify-pool-ownership, ZFS at boot).
    DiskMissing,
    /// The disk is attached again. Nothing imports it on attach (verify-pool-ownership, ZFS
    /// at boot).
    DiskReturns,
    /// At boot, `zfs-import-cache` imports `ployz-device`, which the cache file still lists
    /// (verify-pool-ownership, ZFS at boot).
    AutoImport,
    /// The installer stops between two steps. It crashes, or, after `zpool create`,
    /// `existing_device_pool`'s backing check refuses with `ployz-device` still imported
    /// (installer/storage.rs:214). The process dies or `install_at` returns,
    /// and the guard goes with it (ployzd mutation.rs:37).
    InstallerStops,
    /// `install_at` tries the installation gate's exclusive guard once
    /// (installer/mod.rs:256-257, ployzd mutation.rs:77-81), then `prepare_storage`'s checks
    /// run, from resolving the device (installer/storage.rs:44) to `empty_device` (:63).
    Assess,
    /// `create_device_pool`'s `zpool create ployz-device <device>` (installer/storage.rs:96-106).
    ZpoolCreate,
    /// `create_device_pool`'s `existing_device_pool` (installer/storage.rs:107-112).
    Verify,
    /// `remember_device_pool` (installer/storage.rs:77-83, :113). `install_locked` then
    /// installs the binaries and units and enables the plugin's socket
    /// (installer/mod.rs:298-301, installer/host.rs:262), which this step includes.
    Record,
    /// Docker asks the plugin to create a Provisioned Volume (`VolumeStorage::create`,
    /// volume_plugin/pool.rs:97), or a copy asks for the first Pool (copy.rs `first_pool`).
    /// The request takes the installation gate's shared guard (`admit_mutation`, pool.rs:103,
    /// copy.rs:146-153), then the Pool mutation lock (pool.rs:104, copy.rs:180).
    Request,
    /// `PoolStorage::one_usable` (volume_plugin/pool.rs:214-250).
    OneUsable,
    /// `PoolStorage::recover(UnlabeledBacking::Remove)` (volume_plugin/pool.rs:277-315).
    Recover,
    /// `PoolStorage::create`: the marker check, the backing file, `fallocate`,
    /// `zpool create ployz`, and its `one_usable` (volume_plugin/pool.rs:392-454).
    CreateRoot,
    /// `local_storage` runs `zpool list` (ployzd machine/mod.rs:691-719), in an installed
    /// ployzd.
    Observe,
    /// Docker lists Volumes (`VolumeStorage::list`, volume_plugin/removal.rs:96-104) or
    /// inspects one (`inspect`, :48-53). Each takes the Pool mutation lock but not the
    /// installation gate, then runs `admit` (pool.rs:256-261). This step is its
    /// `one_usable`; when that finds nothing, `ListRecover` follows. Neither creates a Pool.
    List,
    /// A List's or Inspect's `recover` (pool.rs:259, :277-315). The installer can step
    /// between it and the List's `one_usable`, since neither holds the other out.
    ListRecover,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    disk: Disk,
    /// The disk went missing once; it goes missing at most once.
    went_missing: bool,
    /// ZFS's cache file lists `ployz-device`. Neither `zpool create` sets a cachefile, so
    /// both Pools use the default one (installer/storage.rs:96-106, pool.rs:422-440), and a
    /// Pool change rewrites it from the imported Pools (OpenZFS module/zfs/spa_config.c
    /// `spa_write_cachefile`).
    cache_lists_device: bool,
    host: Host,
    installer: Installer,
    plugin: Plugin,
    /// Volume creates the plugin admitted.
    requests: u8,
    /// What `local_storage` answered, on the step that ran it.
    observed: Option<Observed>,
    took: Option<Branch>,
    last: Option<Action>,
}

#[derive(Clone, Copy, Debug)]
struct PoolOwnership {
    /// How many Volume creates the plugin admits.
    requests: u8,
}

impl PoolOwnership {
    /// The installer's next step from `installer`, if it has one.
    fn installer_step(installer: Installer) -> Option<Action> {
        match installer {
            Installer::Start => Some(Action::Assess),
            Installer::Assessed => Some(Action::ZpoolCreate),
            Installer::Created => Some(Action::Verify),
            Installer::Verified => Some(Action::Record),
            Installer::Done | Installer::Stopped => None,
        }
    }

    fn step(state: &mut State, action: Action) {
        state.took = None;
        state.observed = None;
        match action {
            Action::DiskMissing => {
                state.disk.present = false;
                state.disk.imported = false;
                state.went_missing = true;
            }
            Action::DiskReturns => state.disk.present = true,
            Action::AutoImport => state.disk.imported = true,
            Action::InstallerStops => state.installer = Installer::Stopped,
            Action::Assess => {
                if state.plugin.holds_gate() {
                    state.installer = Installer::Done;
                    state.took = Some(Branch::InstallerBusy);
                } else if state.host.root {
                    state.installer = Installer::Done;
                    state.took = Some(Branch::InstallerRefusesRoot);
                } else {
                    state.installer = Installer::Assessed;
                }
            }
            Action::ZpoolCreate => {
                state.disk.label = true;
                state.disk.imported = true;
                state.cache_lists_device = true;
                state.installer = Installer::Created;
            }
            Action::Verify => {
                state.installer = if !state.disk.imported || state.host.root {
                    state.took = Some(Branch::VerifyRefuses);
                    Installer::Done
                } else {
                    Installer::Verified
                };
            }
            Action::Record => {
                state.host.marker = true;
                state.took = Some(Branch::Records);
                state.host.release = true;
                state.installer = Installer::Done;
            }
            Action::Request => {
                if state.installer.running() {
                    state.took = Some(Branch::InstallationActive);
                } else {
                    state.requests += 1;
                    state.plugin = Plugin::Admitting;
                }
            }
            Action::OneUsable => Self::one_usable(state),
            Action::Recover => Self::recover(state),
            Action::CreateRoot => {
                state.plugin = Plugin::Idle;
                if state.host.marker {
                    state.took = Some(Branch::CreateRefusesOwned);
                } else {
                    state.host.root = true;
                    // The cache file is rewritten from the imported Pools.
                    state.cache_lists_device = state.disk.imported;
                    state.took = Some(Branch::CreatesRoot);
                }
            }
            Action::Observe => Self::observe(state),
            Action::List => {
                Self::one_usable(state);
                if state.plugin == Plugin::Recovering {
                    state.plugin = Plugin::Listing;
                }
            }
            Action::ListRecover => {
                Self::recover(state);
                state.plugin = Plugin::Idle;
            }
        }
    }

    /// `PoolStorage::one_usable` (volume_plugin/pool.rs:214-250): `machine_pool::one_usable`
    /// over `zpool list`, the backing-and-device conflict, then CONFIRMED for an imported
    /// `ployz-device`.
    fn one_usable(state: &mut State) {
        let host = &mut state.host;
        let branch = match (state.disk.imported, host.root) {
            (true, true) => Some(Branch::Ambiguous),
            (true, false) if host.marker => Some(Branch::UsesDevice),
            (true, false) => {
                host.marker = true;
                Some(Branch::Confirms)
            }
            (false, true) if host.marker => Some(Branch::Conflict),
            (false, true) => Some(Branch::UsesRoot),
            (false, false) => None,
        };
        state.plugin = if branch.is_some() {
            Plugin::Idle
        } else {
            Plugin::Recovering
        };
        state.took = branch;
    }

    /// `PoolStorage::recover` (volume_plugin/pool.rs:277-315) with no backing file: only
    /// this plugin creates the root-backed Pool, under its lock, so `one_usable` would have
    /// returned it. With one disk the `zpool import` scan lists at most one `ployz-device`.
    fn recover(state: &mut State) {
        debug_assert!(
            !state.host.root,
            "recover runs only when no Pool is imported"
        );
        state.plugin = Plugin::Idle;
        state.took = Some(
            if state.disk.present && state.disk.label && !state.disk.imported {
                state.disk.imported = true;
                state.cache_lists_device = true;
                state.host.marker = true;
                Branch::RecoverImports
            } else if state.host.marker {
                Branch::RefusesOwned
            } else {
                state.plugin = Plugin::Creating;
                Branch::RecoverNone
            },
        );
    }

    /// `local_storage` (ployzd machine/mod.rs:691-719) over `zpool list`.
    fn observe(state: &mut State) {
        state.observed = Some(match (state.disk.imported, state.host.root) {
            (true, true) => Observed::Ambiguous,
            (true, false) | (false, true) => Observed::Pool,
            (false, false) => Observed::Ready,
        });
    }
}

impl Model for PoolOwnership {
    type State = State;
    type Action = Action;

    /// A fresh host, and a host where an earlier `ployzd install` left a release and ZFS
    /// but no Pool, such as one run with `--storage zfs` and no device
    /// (installer/storage.rs:34-39).
    fn init_states(&self) -> Vec<State> {
        let fresh = State {
            disk: Disk {
                present: true,
                label: false,
                imported: false,
            },
            went_missing: false,
            cache_lists_device: false,
            host: Host {
                release: false,
                root: false,
                marker: false,
            },
            installer: Installer::Start,
            plugin: Plugin::Idle,
            requests: 0,
            observed: None,
            took: None,
            last: None,
        };
        let rerun = State {
            host: Host {
                release: true,
                ..fresh.host
            },
            ..fresh
        };
        vec![fresh, rerun]
    }

    /// The world's steps first, then the installer's, the plugin's and `local_storage`.
    /// The order decides only which of the shortest breaking paths a story shows.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        // A boot ends the installer's run and any request in flight, so the model boots
        // only between them.
        let boots = !state.installer.running() && state.plugin == Plugin::Idle;
        let installed = matches!(state.installer, Installer::Done | Installer::Stopped);
        if state.disk.present && !state.went_missing && installed && boots {
            actions.push(Action::DiskMissing);
        }
        if !state.disk.present {
            actions.push(Action::DiskReturns);
        }
        if state.disk.present
            && state.disk.label
            && !state.disk.imported
            && state.cache_lists_device
            && boots
        {
            actions.push(Action::AutoImport);
        }
        if state.installer.running() {
            actions.push(Action::InstallerStops);
        }
        if let Some(step) = Self::installer_step(state.installer) {
            actions.push(step);
        }
        match state.plugin {
            Plugin::Idle if state.host.release && state.requests < self.requests => {
                actions.push(Action::Request);
            }
            Plugin::Idle => {}
            Plugin::Admitting => actions.push(Action::OneUsable),
            Plugin::Recovering => actions.push(Action::Recover),
            Plugin::Creating => actions.push(Action::CreateRoot),
            Plugin::Listing => actions.push(Action::ListRecover),
        }
        if state.host.release && state.plugin == Plugin::Idle {
            actions.push(Action::List);
        }
        if state.host.release {
            actions.push(Action::Observe);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        Self::step(&mut state, action);
        state.last = Some(action);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(NO_ROOT_BESIDE_DEVICE, |_, state: &State| {
                !(state.host.root && state.disk.label)
            }),
            Property::always(NOTHING_STRANDED, |_, state: &State| {
                let refused = matches!(
                    state.took,
                    Some(Branch::RefusesOwned | Branch::CreateRefusesOwned)
                );
                !(refused && state.disk.present && !state.disk.label)
            }),
            Property::always(READY_ONLY_WITH_THE_DISK, |_, state: &State| {
                !(state.observed == Some(Observed::Ready)
                    && state.disk.label
                    && !state.disk.present)
            }),
        ]
    }
}

const NO_ROOT_BESIDE_DEVICE: &str =
    "no root-backed ployz Pool exists while a disk carries a ployz-device label";
const NOTHING_STRANDED: &str = "the plugin never refuses a root-backed Pool for device ownership while the disk is present and carries no ployz-device label";
const READY_ONLY_WITH_THE_DISK: &str =
    "local_storage never reads Ready while the disk that carries the ployz-device label is missing";

/// `path` in the code's words: each step, then the disk, the cache file, the host's files,
/// the installer and the plugin.
fn story(path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || start(state),
            |(before, action)| reads_as(&before, action, state),
        );
        led_by = action.map(|action| (*state, action));
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    disk: present={} label={} imported={} cached={} | host: release={} marker={} root={} | installer: {:?} | plugin: {:?}",
            state.disk.present,
            state.disk.label,
            state.disk.imported,
            state.cache_lists_device,
            state.host.release,
            state.host.marker,
            state.host.root,
            state.installer,
            state.plugin,
        );
    }
    story
}

/// The host a path starts from (`init_states`).
fn start(state: &State) -> String {
    if state.host.release {
        "start: an earlier ployzd install left a release, its volume plugin and ZFS, and no Pool; the disk is present and empty".to_owned()
    } else {
        "start: a fresh host with no release; the disk is present and empty".to_owned()
    }
}

/// `action` from `before`, read with `after` for the branch it took.
fn reads_as(before: &State, action: Action, after: &State) -> String {
    match action {
        Action::DiskMissing if before.cache_lists_device => {
            "World: the host boots without the disk; ZFS's cache-file import of ployz-device fails and leaves the cache file as it is".to_owned()
        }
        Action::DiskMissing => {
            "World: the host boots without the disk; the cache file does not list ployz-device".to_owned()
        }
        Action::DiskReturns => {
            "World: the disk is attached again; nothing imports ployz-device".to_owned()
        }
        Action::AutoImport => {
            "World: at boot, zfs-import-cache imports ployz-device, which the cache file lists".to_owned()
        }
        Action::InstallerStops if before.installer == Installer::Created => {
            "Installer: stops after zpool create, by a crash or by existing_device_pool's backing check refusing with ployz-device still imported; the installation gate is released (installer/storage.rs existing_device_pool :214; installer/mod.rs install_at)".to_owned()
        }
        Action::InstallerStops => format!(
            "Installer: crashes in {:?}; the installation gate is released (installer/storage.rs prepare_storage, create_device_pool; installer/mod.rs install_at)",
            before.installer
        ),
        Action::Assess => match after.took {
            Some(Branch::InstallerBusy) => {
                "Installer: install_at's try_installation finds a Volume create holding the gate and returns \"another Ployz installation is active\" at once (installer/mod.rs install_at :257, mutation.rs try_installation)".to_owned()
            }
            Some(Branch::InstallerRefusesRoot) => {
                "Installer: install_at takes the installation gate; a file-backed Machine Pool already exists; refuses (installer/storage.rs prepare_storage :48)".to_owned()
            }
            _ => "Installer: install_at takes the installation gate; no backing file, no ployz-device, the device is empty; goes on to create (installer/mod.rs install_at, installer/storage.rs prepare_storage :44-63)".to_owned(),
        },
        Action::ZpoolCreate => {
            "Installer: zpool create ployz-device on the device; the disk is labelled and imported, and the cache file lists it (installer/storage.rs create_device_pool :96)".to_owned()
        }
        Action::Verify => match after.took {
            Some(Branch::VerifyRefuses) => {
                "Installer: existing_device_pool finds two usable Pools and refuses; remember_device_pool never runs (installer/storage.rs create_device_pool :107, machine_pool.rs one_usable)".to_owned()
            }
            _ => "Installer: existing_device_pool finds ployz-device the sole usable Pool (installer/storage.rs create_device_pool :107)".to_owned(),
        },
        Action::Record if before.host.release => {
            "Installer: remember_device_pool writes CONFIRMED atomically (installer/storage.rs :113, :77)".to_owned()
        }
        Action::Record => {
            "Installer: remember_device_pool writes CONFIRMED atomically, then install_locked installs the release and enables the volume plugin's socket (installer/storage.rs :113; installer/mod.rs :298-301)".to_owned()
        }
        Action::Request => match after.took {
            Some(Branch::InstallationActive) => {
                "Plugin: a Volume create's admit_mutation is refused, \"a Ployz installation or upgrade is active\"; no Pool command runs (pool.rs VolumeStorage::create, volume_plugin/storage.rs admit_mutation)".to_owned()
            }
            _ => "Plugin: a Volume create takes the installation gate's shared guard and the Pool mutation lock (pool.rs VolumeStorage::create, volume_plugin/storage.rs admit_mutation, pool.rs lock_mutation)".to_owned(),
        },
        Action::OneUsable => {
            let then = match after.took {
                Some(Branch::Ambiguous) => "two usable Pools: refuses as ambiguous",
                Some(Branch::Conflict) => "ployz beside the device marker: refuses as conflicting",
                Some(Branch::Confirms) => "returns ployz-device and writes CONFIRMED",
                Some(Branch::UsesDevice) => "returns ployz-device",
                Some(Branch::UsesRoot) => "returns ployz",
                _ => "nothing is imported: returns None, so admit calls recover",
            };
            format!("Plugin: one_usable reads zpool list; {then} (pool.rs one_usable)")
        }
        Action::Recover => {
            let then = match after.took {
                Some(Branch::RecoverImports) => {
                    "zpool import lists ployz-device: imports it, and one_usable returns it"
                }
                Some(Branch::RefusesOwned) => {
                    "no importable ployz-device and the marker exists: refuses \"device Machine Pool is unavailable\""
                }
                _ => "no backing file, no importable ployz-device, no marker: returns Ok(None) (:314)",
            };
            format!("Plugin: recover; {then} (pool.rs recover)")
        }
        Action::CreateRoot => match after.took {
            Some(Branch::CreateRefusesOwned) => {
                "Plugin: create finds the marker and refuses (pool.rs create :392)".to_owned()
            }
            _ => format!(
                "Plugin: create finds no marker (:392), creates the backing file (:401), runs zpool create ployz on it (:422){}, and its one_usable returns ployz (pool.rs create :445); the Volume is created on it (pool.rs VolumeStorage::create :113-120)",
                if before.cache_lists_device && !after.cache_lists_device {
                    ", which rewrites the cache file without ployz-device"
                } else {
                    ""
                },
            ),
        },
        Action::List if after.plugin == Plugin::Listing => {
            "Plugin: Docker lists or inspects Volumes, under the Pool mutation lock and without the installation gate (volume_plugin/removal.rs list :96-104, inspect :48-53); admit's one_usable reads zpool list; nothing is imported, so recover is next (pool.rs one_usable)".to_owned()
        }
        Action::List => format!(
            "Plugin: Docker lists or inspects Volumes, under the Pool mutation lock and without the installation gate (volume_plugin/removal.rs list :96-104, inspect :48-53); admit: {}; no Pool is created",
            reads_as(before, Action::OneUsable, after).trim_start_matches("Plugin: ")
        ),
        Action::ListRecover => format!(
            "Plugin: List's {}; no Pool is created",
            reads_as(before, Action::Recover, after).trim_start_matches("Plugin: ")
        ),
        Action::Observe => {
            let what = match after.observed {
                Some(Observed::Ready) => "no Pool listed: Ready, which ployz server show and Cloud print \"Managed volumes available\"",
                Some(Observed::Pool) => "one usable Pool: Pool",
                _ => "two usable Pools: one_usable refuses, so None",
            };
            format!("Machine: local_storage reads zpool list; {what} (machine/mod.rs local_storage)")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stateright::{Checker, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    /// The code as it is, with two Volume creates.
    fn as_is() -> PoolOwnership {
        PoolOwnership { requests: 2 }
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so
    /// no shorter path breaks it, printed as a story under the test's name. The checker's
    /// own discovery is not that path: `check_block` stores every violation it visits,
    /// each overwriting the last, and `finish_when` is read once per 1500-state block, so
    /// a visitor keeps the first. One thread keeps the path the same from run to run.
    fn breaks(model: PoolOwnership, property: &'static str) -> Path {
        let condition = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property")
            .condition;
        let first = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&first);
        model
            .checker()
            .threads(1)
            .visitor(move |path: stateright::Path<State, Action>| {
                let mut first = seen.lock().expect("the first break");
                if first.is_none() && !condition(&model, path.last_state()) {
                    *first = Some(path.into_vec());
                }
            })
            .finish_when(HasDiscoveries::AllOf([property].into()))
            .spawn_bfs()
            .join();
        let path = first
            .lock()
            .expect("the first break")
            .take()
            .expect("a path breaks the property");
        let test = std::thread::current();
        println!(
            "[{}] {property} fails under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            story(&path)
        );
        path
    }

    fn first(path: &Path) -> State {
        path.first().expect("a path has a state").0
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    fn position(path: &Path, action: Action) -> Option<usize> {
        path.iter().position(|(_, taken)| *taken == Some(action))
    }

    fn steps(path: &Path) -> Vec<Action> {
        path.iter().filter_map(|(_, action)| *action).collect()
    }

    /// Steps `actions` from the rerun start under `model`, each one offered where it is
    /// taken, and prints the path as a story under the test's name.
    fn replay(model: PoolOwnership, actions: &[Action]) -> Path {
        let mut state = model
            .init_states()
            .into_iter()
            .find(|state| state.host.release && !state.host.marker)
            .expect("the rerun start");
        let mut path = Vec::new();
        for &action in actions {
            let mut offered = Vec::new();
            model.actions(&state, &mut offered);
            assert!(offered.contains(&action), "{action:?} is offered");
            let next = model.next_state(&state, action).expect("a next state");
            path.push((state, Some(action)));
            state = next;
        }
        path.push((state, None));
        let test = std::thread::current();
        println!(
            "[{}] replay under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            story(&path)
        );
        path
    }

    /// The finding. The installer runs `zpool create ployz-device` and stops before
    /// `remember_device_pool`. The host boots without the disk. A Volume create finds
    /// nothing in `one_usable`, `recover` returns `Ok(None)` with no marker, and `create`
    /// builds a root-backed `ployz` beside the labelled disk, which drops `ployz-device`
    /// from the cache file. This test passes by finding that path, the shortest that
    /// breaks the property.
    #[test]
    fn current_code_builds_a_root_pool_beside_an_unrecorded_device_pool() {
        let path = breaks(as_is(), NO_ROOT_BESIDE_DEVICE);
        assert!(
            first(&path).host.release,
            "an earlier install left a plugin"
        );
        let broken = last(&path);
        assert!(broken.host.root && broken.disk.label && !broken.disk.present);
        assert!(!broken.host.marker, "remember_device_pool never ran");
        assert!(
            !broken.cache_lists_device,
            "zpool create ployz rewrote the cache file"
        );
        assert_eq!(broken.installer, Installer::Stopped);
        assert_eq!(
            steps(&path),
            [
                Action::Assess,
                Action::ZpoolCreate,
                Action::InstallerStops,
                Action::DiskMissing,
                Action::Request,
                Action::OneUsable,
                Action::Recover,
                Action::CreateRoot,
            ]
        );
        let recovered = position(&path, Action::Recover)
            .and_then(|at| path.get(at + 1))
            .expect("recover ran")
            .0;
        assert_eq!(recovered.took, Some(Branch::RecoverNone));
    }

    /// A List straddles the install. Its `one_usable` finds nothing imported; the installer
    /// then creates, checks and records `ployz-device`; the List's `recover` finds no
    /// importable Pool, since `zpool import` lists only Pools not imported, and refuses on
    /// the marker (pool.rs:308-313) while `ployz-device` is imported.
    #[test]
    fn a_list_across_the_install_refuses_beside_the_imported_device() {
        let path = replay(
            as_is(),
            &[
                Action::Assess,
                Action::List,
                Action::ZpoolCreate,
                Action::Verify,
                Action::Record,
                Action::ListRecover,
            ],
        );
        let refused = last(&path);
        assert_eq!(refused.took, Some(Branch::RefusesOwned));
        assert!(refused.disk.imported && refused.host.marker);
    }
}
