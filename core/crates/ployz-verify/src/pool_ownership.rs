//! Who owns a Machine's Machine Pool, between the installer that creates a device Pool on
//! a selected disk and the volume plugin that creates a root-backed Pool when it finds
//! none. The installer's named-device path (ployzd installer/storage.rs `prepare_storage`,
//! :40-73) checks the host, runs `zpool create ployz-device` (`create_device_pool`,
//! :96-106), checks the Pool with `existing_device_pool` (:107-112), and only then records
//! ownership with `remember_device_pool` (:113), an atomic write of the CONFIRMED marker
//! (:77-83). `install_at` holds the installation gate's exclusive guard throughout
//! (installer/mod.rs:259-264, :286). The plugin's `VolumeStorage::create`
//! (volume_plugin/pool.rs:96-124) takes that gate's shared guard (`admit_mutation`,
//! :102, volume_plugin/storage.rs:178-188), then the Pool mutation lock (pool.rs:103), then
//! runs `one_usable` (:213), `recover` (:276) and, when both find nothing, `create`
//! (:391), which builds the root-backed `ployz` Pool on a backing file. Copy's `first_pool`
//! takes the same path (volume_plugin/copy.rs:146-160, :175-196). Meanwhile the disk can go
//! missing and return, ZFS imports at boot the Pools its cache file lists, and
//! `local_storage` (ployzd machine/mod.rs:689) reports what `zpool list` shows.
//!
//! How ZFS imports at boot comes from a reboot run on Ubuntu 24.04.4 with ZFS 2.2.2 (the
//! verify-pool-ownership skill's ZFS at boot). Neither `zpool create` sets a cachefile, so
//! both Pools land in the default cache file. A boot without the disk fails to import
//! `ployz-device` and leaves the file as it is. A later Pool change rewrites the file from
//! the imported Pools (OpenZFS module/zfs/spa_config.c `spa_write_cachefile`).
//! `zfs-import-cache` imports only what the file lists, and nothing imports a disk attached
//! later.
//!
//! The finding needs a volume plugin installed before the installer creates `ployz-device`.
//! `install_locked` runs `prepare_storage` before it installs the binaries and units and
//! enables the plugin's socket (installer/mod.rs:289, :298-301, installer/host.rs:270), so
//! a fresh host has no plugin until a run completes. An earlier `ployzd install`, whose
//! `--storage` defaults to `none` (ployzd main.rs:108-109), installs both and enables the
//! socket whatever its storage, and a later `ployzd install --storage zfs:<device>` runs
//! `prepare_storage` again (installer/mod.rs:283-289); docs/user/servers/add-a-server.md:207-208
//! names that command. The model starts from either host.
//!
//! Docker's List (volume_plugin/removal.rs:96-104) and Inspect (:48-53) take the Pool
//! mutation lock but not the installation gate, and run `admit`: `one_usable`, which
//! writes CONFIRMED for an imported `ployz-device` (pool.rs:236-245), then `recover`. The
//! model has them as two steps, offered whenever a release is installed, including while
//! the installer runs: `List`, which is `one_usable`, then `ListRecover`, which is `recover`
//! when `one_usable` found nothing. The installer is another process (`ployzd install`),
//! so it can step between any two of a List's reads, not only at an await. In
//! `one_usable`, a `Record` between `zpool list` (:220) and the marker read or write
//! (:238-244) leaves the CONFIRMED marker the List would have written. In `recover`, the
//! `zpool import` scan (:286) never lists `ployz-device` while the installer runs, since
//! the disk is blank or `zpool create` imported it, and the backing-file reads (:277-278)
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
//! finds no importable device Pool and no marker and returns `Ok(None)` (pool.rs:313),
//! `create` passes its marker-only refusal (:391-395), and the plugin builds a root-backed
//! `ployz` beside the labelled disk (:400, :421). That `zpool create` rewrites the cache file
//! from the imported Pools alone, so no boot imports `ployz-device` again, and `admit`
//! returns `ployz` from `one_usable` without reaching `recover` (pool.rs:255-260). And
//! `local_storage` reads Ready while a recorded device Pool's disk is missing
//! (machine/mod.rs:708).
//!
//! The fix variant keeps the marker's bytes. Before `zpool create`, the installer writes an
//! intent, a sibling file naming the device. `recover` refuses while an intent's disk is
//! missing or labelled. It clears an intent whose disk is present with no label, but only
//! for a caller holding the installation gate's shared guard; List and Inspect leave it,
//! since their installer may still be before `zpool create`. `local_storage` reads None
//! where `recover` would refuse. Under the fix, a List's `recover` also reads the intent
//! and its disk after the marker. The installer writes them in the opposite order, so each
//! answer is that of a `ListRecover` placed at one point of the install; the skill's
//! Bounds goes through the cases.
//!
//! Out of scope: a re-run of the installer, the plain `--storage zfs` path and its
//! `reconcile_device_pool_marker`, a legacy or partial marker, a second disk, a disk that
//! vanishes under an imported Pool, a boot during an installation or a Volume create, an
//! operator's own `zpool` commands, and Volume operations other than List, Inspect and
//! creating the first Pool. So is `create`'s cleanup (pool.rs:454-468), which runs when
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
    /// (installer/mod.rs:301-304, installer/host.rs:261-274).
    release: bool,
    /// The root-backed `ployz` Pool exists on `/var/lib/ployz-machine-pool`, imported
    /// (`MachineLayout::machine_pool_file`, machine_layout.rs:102-106).
    root: bool,
    /// `/var/lib/ployz-machine-pool.device` holds CONFIRMED (`MachineLayout::device_pool_marker`,
    /// machine_layout.rs:108-112; machine_pool.rs `CONFIRMED_DEVICE_POOL_MARKER`).
    marker: bool,
    /// The fix's intent file names the disk. Nothing in the code as it is writes one.
    intent: bool,
}

/// Where the installer is in `prepare_storage` and `create_device_pool`
/// (installer/storage.rs). In every phase but `Start`, `Done` and `Stopped`, `install_at`
/// holds the installation gate's exclusive guard (installer/mod.rs:259-264).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Installer {
    /// `install_at` has not run.
    Start,
    /// The checks passed; `zpool create` (:96) is next, or in the fix the intent.
    Assessed,
    /// The fix: the intent is written; `zpool create` is next.
    Intended,
    /// Under Broken: `remember_device_pool` ran before `zpool create`.
    RecordedEarly,
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
    /// The request holds both (`admit_mutation`, :102; `lock_mutation`, :103);
    /// `one_usable` (:213) is next.
    Admitting,
    /// `one_usable` found nothing imported; `recover` (:276) is next.
    Recovering,
    /// `recover` returned `Ok(None)`; `PoolStorage::create` (:390) is next.
    Creating,
    /// A List's or Inspect's `one_usable` found nothing imported; its `recover` is next,
    /// under the Pool mutation lock and without the installation gate (pool.rs:255-260,
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

/// What `local_storage` (ployzd machine/mod.rs:689-717) answered.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Observed {
    /// `MachineStorageObservation::Ready`: `zpool list` lists no Pool (:710).
    Ready,
    /// `MachineStorageObservation::Pool`: one usable Pool (:705-709).
    Pool,
    /// `None`: `machine_pool::one_usable` refused two usable Pools (:711,
    /// machine_pool.rs:78-85). Not reached on the code as it is
    /// (`current_code_keeps_the_root_pool_once_the_disk_returns`); `installer_ignores_root`
    /// reaches it.
    Ambiguous,
    /// The fix: `None` where `recover` would refuse for device ownership.
    Unresolved,
}

/// The branch the last step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// `admit_mutation`'s `try_mutation` refused: "a Ployz installation or upgrade is
    /// active" (volume_plugin/storage.rs:178-188, ployzd mutation.rs:55-69).
    InstallationActive,
    /// `install_at`'s `try_installation` refused at once while a Volume create holds the
    /// shared guard (ployzd mutation.rs:77-81, installer/mod.rs:259-260). The installer
    /// prints "another Ployz installation is active" (installer/mod.rs:92-93, :342-350).
    InstallerBusy,
    /// `prepare_storage` refuses beside a root-backed Pool: the backing check
    /// (installer/storage.rs:48-53), or `existing_device_pool` finding `ployz`
    /// (:178-187).
    InstallerRefusesRoot,
    /// `create_device_pool`'s `existing_device_pool` refused (installer/storage.rs:107-112):
    /// `machine_pool::one_usable` found two usable Pools (machine_pool.rs:78-85). Reached
    /// on neither the code as it is nor the fix; `installer_ignores_root` reaches it.
    VerifyRefuses,
    /// `create_device_pool`'s `remember_device_pool` wrote CONFIRMED
    /// (installer/storage.rs:113).
    Records,
    /// `PoolStorage::one_usable`'s `machine_pool::one_usable` refused two usable Pools
    /// (volume_plugin/pool.rs:224-225, machine_pool.rs:78-85). Reached on neither the code
    /// as it is nor the fix; `installer_ignores_root` reaches it.
    Ambiguous,
    /// `PoolStorage::one_usable` refused the backing file beside device ownership
    /// (volume_plugin/pool.rs:228-235). Reached on neither the code as it is nor the fix;
    /// the two marker flags together reach it.
    Conflict,
    /// `PoolStorage::one_usable` returned `ployz-device` and wrote CONFIRMED
    /// (volume_plugin/pool.rs:236-245).
    Confirms,
    /// `PoolStorage::one_usable` returned `ployz-device` without writing CONFIRMED: it
    /// was there (volume_plugin/pool.rs:238-241), or under Broken.
    UsesDevice,
    /// `PoolStorage::one_usable` returned `ployz` (volume_plugin/pool.rs:224-248).
    UsesRoot,
    /// `recover` imported the one importable `ployz-device` (volume_plugin/pool.rs:300-305).
    RecoverImports,
    /// `recover` refused: no importable device Pool and the marker exists
    /// (volume_plugin/pool.rs:307-312).
    RefusesOwned,
    /// The fix: `recover` refused, the intent's disk missing or labelled. A present labelled
    /// disk is imported first (volume_plugin/pool.rs:300-305) unless the `zpool import`
    /// scan does not list it. Within the bounds that happens when a List's `one_usable`
    /// ran before the installer's `zpool create` and its `recover` after: the Pool is
    /// imported, so the scan does not list it. A label the scan skips for another reason
    /// has no state in the model.
    RefusesIntent,
    /// The fix: `recover` removed an intent whose disk is present with no label, for a
    /// caller that holds the installation gate's shared guard, a Volume create or copy
    /// (`admit_mutation`, volume_plugin/pool.rs:102, copy.rs:146-153). No installer runs
    /// while that guard is held (ployzd mutation.rs:77-81).
    ClearsStale,
    /// The fix: `recover` found an intent whose disk is present with no label, for List or
    /// Inspect, which hold no installation guard (volume_plugin/removal.rs:96-104, :48-53).
    /// An installer may still be before its `zpool create`, so `recover` leaves the intent
    /// and returns `Ok(None)`. The product change: `recover` (volume_plugin/pool.rs:276)
    /// takes whether its caller holds the shared guard; `admit` (:255-260) passes it
    /// through, and copy's `first_pool`, which calls `recover` directly (copy.rs:181-187),
    /// passes yes.
    KeepsIntent,
    /// `recover` returned `Ok(None)` (volume_plugin/pool.rs:313).
    RecoverNone,
    /// `PoolStorage::create` refused on the marker (volume_plugin/pool.rs:391-395). Reached
    /// on neither the code as it is nor the fix, since `recover` read the same marker
    /// under the same gate and lock; `recover_ignores_marker` and
    /// `gate_admits_during_install` each reach it.
    CreateRefusesOwned,
    /// `PoolStorage::create` built `ployz`, and its `one_usable` returned it
    /// (volume_plugin/pool.rs:400-453).
    CreatesRoot,
    /// The fix: `local_storage` read None for device ownership.
    ObservesUnresolved,
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
    /// (installer/mod.rs:259-260, ployzd mutation.rs:77-81), then `prepare_storage`'s checks
    /// run, from resolving the device (installer/storage.rs:44) to `empty_device` (:63).
    Assess,
    /// The fix: the installer writes the intent atomically.
    WriteIntent,
    /// `create_device_pool`'s `zpool create ployz-device <device>` (installer/storage.rs:96-106).
    ZpoolCreate,
    /// `create_device_pool`'s `existing_device_pool` (installer/storage.rs:107-112).
    Verify,
    /// `remember_device_pool` (installer/storage.rs:77-83, :113). `install_locked` then
    /// installs the binaries and units and enables the plugin's socket
    /// (installer/mod.rs:301-304, installer/host.rs:270), which this step includes.
    Record,
    /// Docker asks the plugin to create a Provisioned Volume (`VolumeStorage::create`,
    /// volume_plugin/pool.rs:96), or a copy asks for the first Pool (copy.rs `first_pool`).
    /// The request takes the installation gate's shared guard (`admit_mutation`, pool.rs:102,
    /// copy.rs:146-153), then the Pool mutation lock (pool.rs:103, copy.rs:180).
    Request,
    /// `PoolStorage::one_usable` (volume_plugin/pool.rs:213-249).
    OneUsable,
    /// `PoolStorage::recover(UnlabeledBacking::Remove)` (volume_plugin/pool.rs:276-314).
    Recover,
    /// `PoolStorage::create`: the marker check, the backing file, `fallocate`,
    /// `zpool create ployz`, and its `one_usable` (volume_plugin/pool.rs:391-453).
    CreateRoot,
    /// `local_storage` runs `zpool list` (ployzd machine/mod.rs:689-717), in an installed
    /// ployzd.
    Observe,
    /// Docker lists Volumes (`VolumeStorage::list`, volume_plugin/removal.rs:96-104) or
    /// inspects one (`inspect`, :48-53). Each takes the Pool mutation lock but not the
    /// installation gate, then runs `admit` (pool.rs:255-260). This step is its
    /// `one_usable`; when that finds nothing, `ListRecover` follows. Neither creates a Pool.
    List,
    /// A List's or Inspect's `recover` (pool.rs:258, :276-314). The installer can step
    /// between it and the List's `one_usable`, since neither holds the other out.
    ListRecover,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    disk: Disk,
    /// The disk went missing once; it goes missing at most once.
    went_missing: bool,
    /// ZFS's cache file lists `ployz-device`. Neither `zpool create` sets a cachefile, so
    /// both Pools use the default one (installer/storage.rs:96-106, pool.rs:421-439), and a
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

/// A guard taken out, to show the check catches what it prevents. The first six are
/// guards the code has; the last six are parts of the fix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// The installation gate admits a Volume create while `install_at` runs, and lets
    /// `install_at` start while a Volume create holds the shared guard (installer/mod.rs:260,
    /// volume_plugin/storage.rs:182-185).
    gate_admits_during_install: bool,
    /// `recover` (volume_plugin/pool.rs:307-312) ignores the marker.
    recover_ignores_marker: bool,
    /// `create` (volume_plugin/pool.rs:391-395) ignores the marker.
    create_ignores_marker: bool,
    /// `remember_device_pool` runs before `zpool create`, as 0d1e04b143 had it before
    /// cc647240a5 moved it after `existing_device_pool` (installer/storage.rs:107-113). It
    /// takes the fix's intent's place.
    records_before_create: bool,
    /// `one_usable` (volume_plugin/pool.rs:236-245) returns an imported `ployz-device`
    /// without writing CONFIRMED.
    one_usable_never_confirms: bool,
    /// `prepare_storage` goes on beside a root-backed Pool: no backing check
    /// (installer/storage.rs:48-53) and no `ployz` refusal in `existing_device_pool`
    /// (:178-187).
    installer_ignores_root: bool,
    /// The fix's installer writes no intent.
    no_intent: bool,
    /// The fix's `recover` ignores the intent.
    recover_ignores_intent: bool,
    /// The fix's `recover` refuses on any intent, never clearing one whose disk is present
    /// with no label.
    intent_never_clears: bool,
    /// The fix's `recover` clears a stale intent for List and Inspect too, which hold no
    /// installation guard (volume_plugin/removal.rs:96-104, :48-53).
    ungated_clears_intent: bool,
    /// The fix's `local_storage` ignores the marker, which every Machine the code as it is
    /// recorded has without an intent.
    observation_ignores_marker: bool,
    /// The fix's `local_storage` ignores the intent.
    observation_ignores_intent: bool,
}

/// One `sometimes`: a branch the code handles, read from the state that took it.
struct Coverage {
    name: &'static str,
    /// Reached only with the fix.
    fix_only: bool,
    reached: fn(&PoolOwnership, &State) -> bool,
}

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

/// Each branch the bounds reach on both the code and the fix, and each branch the fix
/// adds. `current_code_breaks_only_the_findings` and `an_intent_keeps_one_owner` expect
/// every row reached.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "the gate refuses a Volume create while the installer runs",
        fix_only: false,
        reached: |_, state| took(state, Branch::InstallationActive) && state.installer.running(),
    },
    Coverage {
        name: "the gate refuses the installer while a Volume create runs",
        fix_only: false,
        reached: |_, state| took(state, Branch::InstallerBusy) && state.plugin.holds_gate(),
    },
    Coverage {
        name: "the installer refuses beside a root-backed Pool",
        fix_only: false,
        reached: |_, state| took(state, Branch::InstallerRefusesRoot) && state.host.root,
    },
    Coverage {
        name: "the installer records CONFIRMED after its check",
        fix_only: false,
        reached: |_, state| took(state, Branch::Records) && state.disk.label,
    },
    Coverage {
        name: "one_usable confirms a device Pool the installer stopped before recording",
        fix_only: false,
        reached: |_, state| took(state, Branch::Confirms) && state.installer == Installer::Stopped,
    },
    Coverage {
        name: "one_usable returns the recorded device Pool",
        fix_only: false,
        reached: |_, state| took(state, Branch::UsesDevice) && state.host.marker,
    },
    Coverage {
        name: "recover imports the device Pool once its disk returns",
        fix_only: false,
        reached: |_, state| took(state, Branch::RecoverImports) && state.went_missing,
    },
    Coverage {
        name: "recover refuses a root-backed Pool while the recorded device is missing",
        fix_only: false,
        reached: |_, state| {
            took(state, Branch::RefusesOwned) && state.host.marker && !state.disk.present
        },
    },
    Coverage {
        name: "a List across the install refuses on the marker while ployz-device is imported",
        fix_only: false,
        reached: |_, state| {
            took(state, Branch::RefusesOwned)
                && state.disk.imported
                && state.last == Some(Action::ListRecover)
        },
    },
    Coverage {
        name: "the plugin creates a root-backed Pool on a host whose disk carries no label",
        fix_only: false,
        reached: |_, state| {
            took(state, Branch::CreatesRoot) && state.host.root && !state.disk.label
        },
    },
    Coverage {
        name: "recover refuses while the unrecorded device Pool's disk is missing",
        fix_only: true,
        reached: |_, state| {
            took(state, Branch::RefusesIntent)
                && state.host.intent
                && !state.host.marker
                && state.disk.label
                && !state.disk.present
        },
    },
    Coverage {
        name: "recover refuses while the disk of an intent left before zpool create is missing",
        fix_only: true,
        reached: |_, state| {
            took(state, Branch::RefusesIntent)
                && state.host.intent
                && !state.disk.label
                && !state.disk.present
        },
    },
    Coverage {
        name: "a List across zpool create refuses on the intent while ployz-device is imported",
        fix_only: true,
        reached: |_, state| {
            took(state, Branch::RefusesIntent)
                && state.host.intent
                && state.disk.imported
                && state.last == Some(Action::ListRecover)
        },
    },
    Coverage {
        name: "recover clears an intent the installer left before zpool create",
        fix_only: true,
        reached: |_, state| {
            took(state, Branch::ClearsStale)
                && !state.host.intent
                && state.disk.present
                && !state.disk.label
        },
    },
    Coverage {
        name: "List leaves the intent of an installer still before zpool create",
        fix_only: true,
        reached: |_, state| {
            took(state, Branch::KeepsIntent)
                && state.host.intent
                && state.installer == Installer::Intended
        },
    },
    Coverage {
        name: "local_storage reads None while the owned disk is missing",
        fix_only: true,
        reached: |_, state| {
            took(state, Branch::ObservesUnresolved)
                && state.observed == Some(Observed::Unresolved)
                && state.disk.label
                && !state.disk.present
        },
    },
];

#[derive(Clone, Copy, Debug)]
struct PoolOwnership {
    /// The fix: the installer writes an intent before `zpool create`; `recover` refuses on
    /// an intent that stands, and clears a stale one only for a caller that holds the
    /// installation gate; `local_storage` reads None where `recover` would refuse.
    fix: bool,
    broken: Broken,
    /// How many Volume creates the plugin admits.
    requests: u8,
    /// Start only from the fresh host, with no earlier release. The checker keeps only the
    /// first path to each state, so a filter on the start cannot select the fresh host's
    /// paths in a model with both starts.
    fresh_only: bool,
}

impl PoolOwnership {
    /// The installer's next step from `installer`, if it has one.
    fn installer_step(&self, installer: Installer) -> Option<Action> {
        let broken = self.broken;
        match installer {
            Installer::Start => Some(Action::Assess),
            Installer::Assessed if broken.records_before_create => Some(Action::Record),
            Installer::Assessed if self.fix && !broken.no_intent => Some(Action::WriteIntent),
            Installer::Assessed | Installer::Intended | Installer::RecordedEarly => {
                Some(Action::ZpoolCreate)
            }
            Installer::Created => Some(Action::Verify),
            Installer::Verified => Some(Action::Record),
            Installer::Done | Installer::Stopped => None,
        }
    }

    /// The installation gate refuses `install_at` while a Volume create holds the shared
    /// guard, and a Volume create while `install_at` holds the exclusive one, each at once
    /// (ployzd mutation.rs:55-81).
    fn gated(&self) -> bool {
        !self.broken.gate_admits_during_install
    }

    /// The fix's intent no longer stands: its disk is present and carries no label, so
    /// `zpool create` never landed.
    fn intent_is_stale(&self, disk: Disk) -> bool {
        disk.present && !disk.label && !self.broken.intent_never_clears
    }

    fn step(&self, state: &mut State, action: Action) {
        let broken = self.broken;
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
                if state.plugin.holds_gate() && self.gated() {
                    state.installer = Installer::Done;
                    state.took = Some(Branch::InstallerBusy);
                } else if state.host.root && !broken.installer_ignores_root {
                    state.installer = Installer::Done;
                    state.took = Some(Branch::InstallerRefusesRoot);
                } else {
                    state.installer = Installer::Assessed;
                }
            }
            Action::WriteIntent => {
                state.host.intent = true;
                state.installer = Installer::Intended;
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
                } else if broken.records_before_create {
                    Installer::Done
                } else {
                    Installer::Verified
                };
            }
            Action::Record => {
                state.host.marker = true;
                state.installer = if state.installer == Installer::Assessed {
                    Installer::RecordedEarly
                } else {
                    state.took = Some(Branch::Records);
                    state.host.release = true;
                    Installer::Done
                };
            }
            Action::Request => {
                if state.installer.running() && self.gated() {
                    state.took = Some(Branch::InstallationActive);
                } else {
                    state.requests += 1;
                    state.plugin = Plugin::Admitting;
                }
            }
            Action::OneUsable => self.one_usable(state),
            Action::Recover => self.recover(state, true),
            Action::CreateRoot => {
                state.plugin = Plugin::Idle;
                if state.host.marker && !broken.create_ignores_marker {
                    state.took = Some(Branch::CreateRefusesOwned);
                } else {
                    state.host.root = true;
                    // The cache file is rewritten from the imported Pools.
                    state.cache_lists_device = state.disk.imported;
                    state.took = Some(Branch::CreatesRoot);
                }
            }
            Action::Observe => self.observe(state),
            Action::List => {
                self.one_usable(state);
                if state.plugin == Plugin::Recovering {
                    state.plugin = Plugin::Listing;
                }
            }
            Action::ListRecover => {
                self.recover(state, false);
                state.plugin = Plugin::Idle;
            }
        }
    }

    /// `PoolStorage::one_usable` (volume_plugin/pool.rs:213-249): `machine_pool::one_usable`
    /// over `zpool list`, the backing-and-device conflict, then CONFIRMED for an imported
    /// `ployz-device`.
    fn one_usable(&self, state: &mut State) {
        let host = &mut state.host;
        let branch = match (state.disk.imported, host.root) {
            (true, true) => Some(Branch::Ambiguous),
            (true, false) if host.marker || self.broken.one_usable_never_confirms => {
                Some(Branch::UsesDevice)
            }
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

    /// `PoolStorage::recover` (volume_plugin/pool.rs:276-314) with no backing file: only
    /// this plugin creates the root-backed Pool, under its lock, so `one_usable` would have
    /// returned it. With one disk the `zpool import` scan lists at most one `ployz-device`.
    /// The fix's intent check comes after the marker's. `gated` says the caller holds the
    /// installation gate's shared guard, as a Volume create does and List and Inspect do
    /// not; only such a caller clears a stale intent, since no installer can be running.
    fn recover(&self, state: &mut State, gated: bool) {
        let broken = self.broken;
        debug_assert!(
            !state.host.root,
            "recover runs only when no Pool is imported"
        );
        state.plugin = Plugin::Idle;
        state.took = Some(
            if state.disk.present && state.disk.label && !state.disk.imported {
                state.disk.imported = true;
                state.cache_lists_device = true;
                if !broken.one_usable_never_confirms {
                    state.host.marker = true;
                }
                Branch::RecoverImports
            } else if state.host.marker && !broken.recover_ignores_marker {
                Branch::RefusesOwned
            } else if self.fix && state.host.intent && !broken.recover_ignores_intent {
                if !self.intent_is_stale(state.disk) {
                    Branch::RefusesIntent
                } else if gated || broken.ungated_clears_intent {
                    state.host.intent = false;
                    state.plugin = Plugin::Creating;
                    Branch::ClearsStale
                } else {
                    Branch::KeepsIntent
                }
            } else {
                state.plugin = Plugin::Creating;
                Branch::RecoverNone
            },
        );
    }

    /// `local_storage` (ployzd machine/mod.rs:689-717) over `zpool list`. With no Pool
    /// listed, the fix reads None where `recover` would refuse: no importable
    /// `ployz-device`, and the marker, or an intent that is not stale.
    fn observe(&self, state: &mut State) {
        let observed = match (state.disk.imported, state.host.root) {
            (true, true) => Observed::Ambiguous,
            (true, false) | (false, true) => Observed::Pool,
            (false, false) => {
                let importable = state.disk.present && state.disk.label;
                let marker = state.host.marker && !self.broken.observation_ignores_marker;
                let intent = state.host.intent
                    && !self.intent_is_stale(state.disk)
                    && !self.broken.observation_ignores_intent;
                if self.fix && !importable && (marker || intent) {
                    state.took = Some(Branch::ObservesUnresolved);
                    Observed::Unresolved
                } else {
                    Observed::Ready
                }
            }
        };
        state.observed = Some(observed);
    }
}

impl Model for PoolOwnership {
    type State = State;
    type Action = Action;

    /// A fresh host, and a host where an earlier `ployzd install` left a release and ZFS
    /// but no Pool, such as one run with `--storage zfs` and no device
    /// (installer/storage.rs:34-39). The fix adds a Machine the code as it is recorded:
    /// `ployz-device` imported and CONFIRMED, with no intent.
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
                intent: false,
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
        if self.fresh_only {
            return vec![fresh];
        }
        let mut states = vec![fresh, rerun];
        if self.fix {
            states.push(State {
                disk: Disk {
                    present: true,
                    label: true,
                    imported: true,
                },
                cache_lists_device: true,
                host: Host {
                    release: true,
                    marker: true,
                    ..fresh.host
                },
                installer: Installer::Done,
                ..fresh
            });
        }
        states
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
        if let Some(step) = self.installer_step(state.installer) {
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
        self.step(&mut state, action);
        state.last = Some(action);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(NO_ROOT_BESIDE_DEVICE, |_, state: &State| {
                !(state.host.root && state.disk.label)
            }),
            Property::always(NOTHING_STRANDED, |_, state: &State| {
                let refused = matches!(
                    state.took,
                    Some(Branch::RefusesOwned | Branch::RefusesIntent | Branch::CreateRefusesOwned)
                );
                !(refused && state.disk.present && !state.disk.label)
            }),
            Property::always(READY_ONLY_WITH_THE_DISK, |_, state: &State| {
                !(state.observed == Some(Observed::Ready)
                    && state.disk.label
                    && !state.disk.present)
            }),
        ];
        properties.extend(
            COVERAGE
                .iter()
                .filter(|row| self.fix || !row.fix_only)
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

const NO_ROOT_BESIDE_DEVICE: &str =
    "no root-backed ployz Pool exists while a disk carries a ployz-device label";
const NOTHING_STRANDED: &str = "the plugin never refuses a root-backed Pool for device ownership while the disk is present and carries no ployz-device label";
const READY_ONLY_WITH_THE_DISK: &str =
    "local_storage never reads Ready while the disk that carries the ployz-device label is missing";

/// `path` in the code's words: each step, then the disk, the cache file, the host's files,
/// the installer and the plugin.
fn story(model: &PoolOwnership, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || start(state),
            |(before, action)| reads_as(model, &before, action, state),
        );
        // A row that skips a guard is reached by a flag or by a direct edit to the model;
        // it names Broken only when a flag is set.
        let what = if model.broken == Broken::default() {
            what.replace(" (under Broken)", "")
                .replace("under Broken, ", "")
        } else {
            what
        };
        led_by = action.map(|action| (*state, action));
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    disk: present={} label={} imported={} cached={} | host: release={} marker={} root={}{} | installer: {:?} | plugin: {:?}",
            state.disk.present,
            state.disk.label,
            state.disk.imported,
            state.cache_lists_device,
            state.host.release,
            state.host.marker,
            state.host.root,
            if model.fix {
                format!(" intent={}", state.host.intent)
            } else {
                String::new()
            },
            state.installer,
            state.plugin,
        );
    }
    story
}

/// The host a path starts from (`init_states`).
fn start(state: &State) -> String {
    if state.host.marker {
        "start: a Machine the installer recorded before the fix: ployz-device imported and CONFIRMED, no intent".to_owned()
    } else if state.host.release {
        "start: an earlier ployzd install left a release, its volume plugin and ZFS, and no Pool; the disk is present and empty".to_owned()
    } else {
        "start: a fresh host with no release; the disk is present and empty".to_owned()
    }
}

/// `action` from `before` under `model`, read with `after` for the branch it took.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement of the code the model copies"
)]
fn reads_as(model: &PoolOwnership, before: &State, action: Action, after: &State) -> String {
    let broken = model.broken;
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
                "Installer: install_at's try_installation finds a Volume create holding the gate and returns \"another Ployz installation is active\" at once (installer/mod.rs install_at :260, mutation.rs try_installation)".to_owned()
            }
            Some(Branch::InstallerRefusesRoot) => {
                "Installer: install_at takes the installation gate; a file-backed Machine Pool already exists; refuses (installer/storage.rs prepare_storage :48)".to_owned()
            }
            _ if before.host.root => {
                "Installer: install_at takes the installation gate; the root-backed Pool is ignored (under Broken); goes on to create (installer/storage.rs prepare_storage :44-63)".to_owned()
            }
            _ if before.plugin.holds_gate() => {
                "Installer: install_at takes the installation gate while a Volume create holds it (under Broken); no backing file, no ployz-device, the device is empty; goes on to create (installer/storage.rs prepare_storage :44-63)".to_owned()
            }
            _ => "Installer: install_at takes the installation gate; no backing file, no ployz-device, the device is empty; goes on to create (installer/mod.rs install_at, installer/storage.rs prepare_storage :44-63)".to_owned(),
        },
        Action::WriteIntent => {
            "Installer: writes the intent naming the device, atomically (the fix)".to_owned()
        }
        Action::ZpoolCreate => {
            "Installer: zpool create ployz-device on the device; the disk is labelled and imported, and the cache file lists it (installer/storage.rs create_device_pool :96)".to_owned()
        }
        Action::Verify => match after.took {
            Some(Branch::VerifyRefuses) => {
                "Installer: existing_device_pool finds two usable Pools and refuses; remember_device_pool never runs (installer/storage.rs create_device_pool :107, machine_pool.rs one_usable)".to_owned()
            }
            _ if broken.records_before_create => {
                "Installer: existing_device_pool finds ployz-device the sole usable Pool; ownership was recorded before (under Broken)".to_owned()
            }
            _ => "Installer: existing_device_pool finds ployz-device the sole usable Pool (installer/storage.rs create_device_pool :107)".to_owned(),
        },
        Action::Record if after.installer == Installer::RecordedEarly => {
            "Installer: remember_device_pool writes CONFIRMED before zpool create (under Broken, 0d1e04b143's order)".to_owned()
        }
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
            _ if before.installer.running() => {
                "Plugin: a Volume create is admitted while the installer runs (under Broken), and takes the Pool mutation lock (pool.rs VolumeStorage::create)".to_owned()
            }
            _ => "Plugin: a Volume create takes the installation gate's shared guard and the Pool mutation lock (pool.rs VolumeStorage::create, volume_plugin/storage.rs admit_mutation, pool.rs lock_mutation)".to_owned(),
        },
        Action::OneUsable => {
            let then = match after.took {
                Some(Branch::Ambiguous) => "two usable Pools: refuses as ambiguous",
                Some(Branch::Conflict) => "ployz beside the device marker: refuses as conflicting",
                Some(Branch::Confirms) => "returns ployz-device and writes CONFIRMED",
                Some(Branch::UsesDevice) if !after.host.marker => {
                    "returns ployz-device and writes nothing (under Broken)"
                }
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
                Some(Branch::RefusesIntent) if after.disk.present && !after.disk.label => {
                    "no importable ployz-device, no marker, and an intent whose disk is present with no label: refuses on any intent (under Broken)"
                }
                Some(Branch::RefusesIntent) => {
                    "no importable ployz-device, no marker, and the intent's disk is missing or labelled: refuses (the fix)"
                }
                Some(Branch::ClearsStale) => {
                    "no importable ployz-device, no marker, and the intent's disk is present with no label: removes the intent and returns Ok(None) (the fix)"
                }
                Some(Branch::KeepsIntent) => {
                    "no importable ployz-device, no marker, and the intent's disk is present with no label, but the caller holds no installation guard, so an installer may still be before zpool create: leaves the intent and returns Ok(None) (the fix)"
                }
                _ if before.host.marker => {
                    "the marker refusal is skipped (under Broken): returns Ok(None) (:314)"
                }
                _ if before.host.intent => {
                    "the intent is ignored (under Broken): returns Ok(None) (:314)"
                }
                _ => "no backing file, no importable ployz-device, no marker: returns Ok(None) (:314)",
            };
            format!("Plugin: recover; {then} (pool.rs recover)")
        }
        Action::CreateRoot => match after.took {
            Some(Branch::CreateRefusesOwned) => {
                "Plugin: create finds the marker and refuses (pool.rs create :391)".to_owned()
            }
            _ => format!(
                "Plugin: create {} (:391), creates the backing file (:400), runs zpool create ployz on it (:421){}, and its one_usable returns ployz (pool.rs create :444); the Volume is created on it (pool.rs VolumeStorage::create :112-119)",
                if before.host.marker {
                    "skips the marker refusal (under Broken)"
                } else {
                    "finds no marker"
                },
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
            reads_as(model, before, Action::OneUsable, after).trim_start_matches("Plugin: ")
        ),
        Action::ListRecover if after.took == Some(Branch::ClearsStale) => {
            "Plugin: List's recover; no importable ployz-device, no marker, and the intent's disk is present with no label: removes the intent without the installation guard, which the fix does not allow; no Pool is created (pool.rs recover)".to_owned()
        }
        Action::ListRecover => format!(
            "Plugin: List's {}; no Pool is created",
            reads_as(model, before, Action::Recover, after).trim_start_matches("Plugin: ")
        ),
        Action::Observe => {
            let what = match after.observed {
                Some(Observed::Ready) => "no Pool listed: Ready, which ployz server show and Cloud print \"Managed volumes available\"",
                Some(Observed::Pool) => "one usable Pool: Pool",
                Some(Observed::Unresolved) => "no Pool listed, and recover would refuse for device ownership: None (the fix)",
                _ => "two usable Pools: one_usable refuses, so None",
            };
            format!("Machine: local_storage reads zpool list; {what} (machine/mod.rs local_storage)")
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stateright::{Checker, Expectation, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    /// The code as it is, with two Volume creates.
    fn as_is() -> PoolOwnership {
        PoolOwnership {
            fix: false,
            broken: Broken::default(),
            requests: 2,
            fresh_only: false,
        }
    }

    /// The fix: the installer's intent, `recover`'s intent check, and `local_storage`'s
    /// None.
    fn fixed() -> PoolOwnership {
        PoolOwnership {
            fix: true,
            ..as_is()
        }
    }

    fn as_is_without(broken: Broken) -> PoolOwnership {
        PoolOwnership { broken, ..as_is() }
    }

    fn fixed_without(broken: Broken) -> PoolOwnership {
        PoolOwnership { broken, ..fixed() }
    }

    fn check(model: PoolOwnership) -> impl Checker<PoolOwnership> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so
    /// no shorter path breaks it, printed as a story under the test's name. For a
    /// `sometimes`, such as a `COVERAGE` row's name, it is the first path that reaches it.
    /// It panics when no path does, as for an `always` that holds. The checker's own
    /// discovery is not that path: `check_block` stores every violation it visits, each
    /// overwriting the last, and `finish_when` is read once per 1500-state block, so a
    /// visitor keeps the first. One thread keeps the path the same from run to run.
    fn breaks(model: PoolOwnership, property: &'static str) -> Path {
        first_break(model, property, |_| true, true).expect("a path breaks the property")
    }

    /// As `breaks`, the first such path that also satisfies `and`. It explores the whole
    /// model, since the first discovery may not pass `and`.
    fn breaks_where(model: PoolOwnership, property: &'static str, and: fn(&Path) -> bool) -> Path {
        first_break(model, property, and, false).expect("a path breaks the property")
    }

    /// As `breaks_where`, or `None` when no path breaks `property` and satisfies `and`.
    fn first_break(
        model: PoolOwnership,
        property: &'static str,
        and: fn(&Path) -> bool,
        stop_at_discovery: bool,
    ) -> Option<Path> {
        let named = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property");
        let reached = matches!(named.expectation, Expectation::Sometimes);
        first_path(
            model,
            property,
            named.condition,
            reached,
            and,
            stop_at_discovery.then_some(property),
        )
    }

    /// The first path in BFS order to a state where `holds` is `reached` and that
    /// satisfies `and`, printed as a story under the test's name, or `None`.
    fn first_path(
        model: PoolOwnership,
        what: &str,
        holds: fn(&PoolOwnership, &State) -> bool,
        reached: bool,
        and: fn(&Path) -> bool,
        stop_at: Option<&'static str>,
    ) -> Option<Path> {
        let first = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&first);
        let checker =
            model
                .checker()
                .threads(1)
                .visitor(move |path: stateright::Path<State, Action>| {
                    let mut first = seen.lock().expect("the first break");
                    if first.is_none() && holds(&model, path.last_state()) == reached {
                        let path = path.into_vec();
                        if and(&path) {
                            *first = Some(path);
                        }
                    }
                });
        let checker = match stop_at {
            Some(property) => checker.finish_when(HasDiscoveries::AllOf([property].into())),
            None => checker,
        };
        checker.spawn_bfs().join();
        let path = first.lock().expect("the first break").take()?;
        let test = std::thread::current();
        println!(
            "[{}] {what} {} under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            if reached { "is reached" } else { "fails" },
            story(&model, &path)
        );
        Some(path)
    }

    /// Whether any path under `model` reaches a state that took `branch`.
    fn reaches(model: PoolOwnership, branch: Branch) -> bool {
        let found = Arc::new(Mutex::new(false));
        let seen = Arc::clone(&found);
        model
            .checker()
            .threads(1)
            .visitor(move |path: stateright::Path<State, Action>| {
                if path.last_state().took == Some(branch) {
                    *seen.lock().expect("the branch") = true;
                }
            })
            .spawn_bfs()
            .join();
        *found.lock().expect("the branch")
    }

    fn first(path: &Path) -> State {
        path.first().expect("a path has a state").0
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    fn took_action(path: &Path, action: Action) -> bool {
        path.iter().any(|(_, taken)| *taken == Some(action))
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
            story(&model, &path)
        );
        path
    }

    /// The plugin built `ployz` after the disk went missing.
    fn built_root_while_missing(path: &Path) -> bool {
        matches!(
            (position(path, Action::DiskMissing), position(path, Action::CreateRoot)),
            (Some(missing), Some(created)) if missing < created
        )
    }

    /// `one_usable` returned `ployz-device` before the disk went missing.
    fn used_the_device_before_it_went_missing(path: &Path) -> bool {
        let used = path.iter().position(|(state, _)| {
            matches!(state.took, Some(Branch::Confirms | Branch::UsesDevice))
        });
        matches!(
            (used, position(path, Action::DiskMissing)),
            (Some(used), Some(missing)) if used <= missing
        )
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
    /// the marker (pool.rs:307-312) while `ployz-device` is imported.
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

    /// The fix's straddle. A List's `one_usable` finds nothing imported while the installer
    /// holds an intent; `zpool create` lands; the List's `recover` finds the intent's disk
    /// labelled and refuses, where the code as it is returns `Ok(None)` and lists nothing.
    #[test]
    fn the_fix_refuses_a_list_across_zpool_create() {
        let path = replay(
            fixed(),
            &[
                Action::Assess,
                Action::WriteIntent,
                Action::List,
                Action::ZpoolCreate,
                Action::ListRecover,
            ],
        );
        let refused = last(&path);
        assert_eq!(refused.took, Some(Branch::RefusesIntent));
        assert!(refused.disk.imported && refused.disk.label && refused.host.intent);
    }

    /// On a fresh host the plugin arrives only with a completed install, after
    /// `remember_device_pool`, so no path from it builds `ployz` beside a labelled disk.
    /// The model starts from the fresh host alone, and a Volume create still runs there:
    /// `OneUsable` follows only `Request`, since a List runs its own `one_usable`.
    #[test]
    fn a_fresh_host_never_builds_a_root_pool_beside_the_disk() {
        let fresh = PoolOwnership {
            fresh_only: true,
            ..as_is()
        };
        let checker = check(fresh);
        println!("fresh host: {} states", checker.unique_state_count());
        assert!(checker.discovery(NO_ROOT_BESIDE_DEVICE).is_none());
        let created = first_path(
            fresh,
            "a Volume create's one_usable returns the recorded device Pool",
            |_, state| took(state, Branch::UsesDevice) && state.last == Some(Action::OneUsable),
            true,
            |_| true,
            None,
        );
        assert!(created.is_some(), "a Volume create ran");
    }

    /// After the finding the disk returns: the first path to a state where the disk was just
    /// attached beside `ployz`. The root create dropped `ployz-device` from the cache file,
    /// and an attach imports nothing, so `ployz-device` stays unimported. On the code as it
    /// is, no path imports `ployz-device` beside `ployz`, so `one_usable` keeps returning
    /// `ployz` (its `(false, true)` arm) and nothing refuses the two as ambiguous.
    #[test]
    fn current_code_keeps_the_root_pool_once_the_disk_returns() {
        let path = first_path(
            as_is(),
            "the disk returns beside ployz",
            |_, state| {
                state.last == Some(Action::DiskReturns) && state.host.root && state.disk.label
            },
            true,
            |_| true,
            None,
        )
        .expect("a path reaches it");
        let returned = last(&path);
        assert!(!returned.disk.imported && !returned.cache_lists_device);
        assert!(built_root_while_missing(&path));
        assert!(
            first_path(
                as_is(),
                "ployz-device is imported beside ployz",
                |_, state| state.host.root && state.disk.imported,
                true,
                |_| true,
                None,
            )
            .is_none()
        );
    }

    /// The second finding. The installer recorded the device Pool, and the host then boots
    /// without the disk. `local_storage` reads Ready, though `recover` refuses a
    /// root-backed Pool.
    #[test]
    fn current_code_reads_ready_while_the_recorded_device_is_missing() {
        let path = breaks_where(as_is(), READY_ONLY_WITH_THE_DISK, |path| {
            took_action(path, Action::Record)
        });
        let broken = last(&path);
        assert!(broken.host.marker && !broken.disk.present);
        assert_eq!(broken.observed, Some(Observed::Ready));
    }

    /// The code as it is breaks only the properties the findings name, and reaches every
    /// `sometimes` the fix does not add.
    #[test]
    fn current_code_breaks_only_the_findings() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(NOTHING_STRANDED).is_none());
        for property in [NO_ROOT_BESIDE_DEVICE, READY_ONLY_WITH_THE_DISK] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
        }
        for row in COVERAGE.iter().filter(|row| !row.fix_only) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The fix holds, and every `sometimes` is reached.
    #[test]
    fn an_intent_keeps_one_owner() {
        let checker = check(fixed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Four branches the code handles are reached on neither the code as it is nor the fix,
    /// which is why `COVERAGE` has no row for them. Each is reached on the code as it is
    /// once a guard is taken out.
    #[test]
    fn some_branches_need_a_broken_guard() {
        let gate = Broken {
            gate_admits_during_install: true,
            ..Broken::default()
        };
        let markers = Broken {
            recover_ignores_marker: true,
            create_ignores_marker: true,
            ..Broken::default()
        };
        let root = Broken {
            installer_ignores_root: true,
            ..Broken::default()
        };
        for (branch, needs) in [
            (Branch::VerifyRefuses, root),
            (Branch::Ambiguous, root),
            (Branch::Conflict, markers),
            (Branch::CreateRefusesOwned, gate),
        ] {
            for model in [as_is(), fixed()] {
                assert!(!reaches(model, branch), "{branch:?} under {model:?}");
            }
            assert!(
                reaches(as_is_without(needs), branch),
                "{branch:?} under {needs:?}"
            );
        }
    }

    /// Without the installation gate, a Volume create builds `ployz` after the installer's
    /// checks, and the installer's `zpool create` then labels the disk beside it, with no
    /// disk missing. The model does not run `create`'s cleanup (pool.rs:454-468), so the
    /// test keeps to paths where `ployz` was built before the label, which no cleanup
    /// undoes.
    #[test]
    fn the_installation_gate_keeps_volume_creates_out_of_the_install() {
        let present = |path: &Path| !took_action(path, Action::DiskMissing);
        assert!(first_break(as_is(), NO_ROOT_BESIDE_DEVICE, present, false).is_none());
        let path = breaks_where(
            as_is_without(Broken {
                gate_admits_during_install: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
            |path| {
                !took_action(path, Action::DiskMissing)
                    && last(path).last == Some(Action::ZpoolCreate)
            },
        );
        let assessed = position(&path, Action::Assess).expect("the installer checked the host");
        let created = position(&path, Action::CreateRoot).expect("the plugin created ployz");
        assert!(assessed < created, "the checks ran before ployz existed");
    }

    /// Without the marker refusals in `recover` and `create`, a recorded device Pool whose
    /// disk is missing gets a root-backed Pool beside it. The code as it is breaks the
    /// property only on paths where `remember_device_pool` never ran, so this test keeps to
    /// paths where it ran.
    #[test]
    fn recovery_needs_the_marker_refusals() {
        let recorded = |path: &Path| took_action(path, Action::Record);
        assert!(first_break(as_is(), NO_ROOT_BESIDE_DEVICE, recorded, false).is_none());
        let path = breaks_where(
            as_is_without(Broken {
                recover_ignores_marker: true,
                create_ignores_marker: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
            recorded,
        );
        let broken = last(&path);
        assert!(broken.host.marker && !broken.disk.present);
    }

    /// Without `recover`'s marker refusal, `create`'s refusal still keeps a recorded device
    /// Pool's host free of `ployz`.
    #[test]
    fn create_refuses_when_recover_ignores_the_marker() {
        let model = as_is_without(Broken {
            recover_ignores_marker: true,
            ..Broken::default()
        });
        let recorded = |path: &Path| took_action(path, Action::Record);
        assert!(first_break(model, NO_ROOT_BESIDE_DEVICE, recorded, false).is_none());
        let path = first_path(
            model,
            "create refuses on the marker",
            |_, state| state.took == Some(Branch::CreateRefusesOwned),
            true,
            recorded,
            None,
        )
        .expect("a path reaches it");
        assert!(!last(&path).host.root);
    }

    /// With `remember_device_pool` before `zpool create`, an installer that stops between
    /// them leaves a marker for a disk with no label. The story ends on List's `recover`
    /// refusing on that marker; a Volume create there takes the same refusal
    /// (volume_plugin/pool.rs:307-312). A List while that installer still runs refuses too,
    /// so the test keeps to paths where the installer stopped.
    #[test]
    fn ownership_is_recorded_only_after_zpool_create() {
        let path = breaks_where(
            as_is_without(Broken {
                records_before_create: true,
                ..Broken::default()
            }),
            NOTHING_STRANDED,
            |path| last(path).installer == Installer::Stopped,
        );
        let broken = last(&path);
        assert_eq!(broken.took, Some(Branch::RefusesOwned));
        assert_eq!(broken.installer, Installer::Stopped);
    }

    /// Without `one_usable`'s CONFIRMED write, a device Pool the plugin already used is
    /// unrecorded once its disk is missing. The code as it is keeps it, on paths where the
    /// plugin used the device Pool before the disk went missing.
    #[test]
    fn one_usable_needs_to_confirm_an_unrecorded_device_pool() {
        let used = used_the_device_before_it_went_missing;
        assert!(first_break(as_is(), NO_ROOT_BESIDE_DEVICE, used, false).is_none());
        let path = breaks_where(
            as_is_without(Broken {
                one_usable_never_confirms: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
            used,
        );
        assert!(!last(&path).host.marker);
    }

    /// Without the installer's root-backed Pool refusals, it labels the disk beside a
    /// `ployz` the plugin built first, with no disk missing.
    #[test]
    fn the_installer_needs_its_root_pool_refusal() {
        let path = breaks_where(
            as_is_without(Broken {
                installer_ignores_root: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
            |path| !took_action(path, Action::DiskMissing),
        );
        let broken = last(&path);
        assert_eq!(broken.last, Some(Action::ZpoolCreate));
        let created = position(&path, Action::CreateRoot).expect("the plugin created ployz");
        let assessed = position(&path, Action::Assess).expect("the installer checked the host");
        assert!(
            created < assessed,
            "ployz existed when the installer checked"
        );
    }

    /// Without the intent, the fix is the code as it is, and the finding comes back.
    #[test]
    fn the_fix_needs_the_intent() {
        let path = breaks(
            fixed_without(Broken {
                no_intent: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
        );
        let broken = last(&path);
        assert!(broken.went_missing && !broken.host.intent && !broken.host.marker);
    }

    /// Without `recover`'s intent check, the intent is written and nothing reads it.
    #[test]
    fn the_fix_needs_recover_to_heed_the_intent() {
        let path = breaks(
            fixed_without(Broken {
                recover_ignores_intent: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
        );
        let broken = last(&path);
        assert!(broken.went_missing && broken.host.intent && !broken.host.marker);
    }

    /// Without clearing a stale intent, an installer that stops before `zpool create`
    /// leaves an intent on a disk that never got a label. The story ends on List's
    /// `recover` refusing on that intent; a Volume create there takes the same refusal,
    /// which the fix adds beside the marker's (volume_plugin/pool.rs:307-312). A List while
    /// that installer still runs refuses too, so the test keeps to paths where the
    /// installer stopped.
    #[test]
    fn the_fix_needs_a_stale_intent_to_clear() {
        let path = breaks_where(
            fixed_without(Broken {
                intent_never_clears: true,
                ..Broken::default()
            }),
            NOTHING_STRANDED,
            |path| last(path).installer == Installer::Stopped,
        );
        let broken = last(&path);
        assert_eq!(broken.took, Some(Branch::RefusesIntent));
        assert_eq!(broken.installer, Installer::Stopped);
        assert!(broken.host.intent && !broken.disk.label);
    }

    /// Without List and Inspect leaving a stale-looking intent, a List while the installer
    /// is before `zpool create` removes the intent. The installer then labels the disk and
    /// stops, the host boots without it, and nothing refuses the root-backed Pool.
    #[test]
    fn the_fix_needs_list_to_leave_a_live_intent() {
        let path = breaks(
            fixed_without(Broken {
                ungated_clears_intent: true,
                ..Broken::default()
            }),
            NO_ROOT_BESIDE_DEVICE,
        );
        let cleared = position(&path, Action::ListRecover)
            .and_then(|at| path.get(at + 1))
            .expect("List's recover ran")
            .0;
        assert_eq!(cleared.took, Some(Branch::ClearsStale));
        assert_eq!(cleared.installer, Installer::Intended);
        assert!(!cleared.host.intent);
        let broken = last(&path);
        assert!(broken.host.root && broken.disk.label && !broken.host.marker);
    }

    /// Without `local_storage` reading the marker, a Machine the code as it is recorded,
    /// with a marker and no intent, reads Ready once its disk is missing.
    #[test]
    fn the_fix_needs_local_storage_to_read_the_marker() {
        let path = breaks(
            fixed_without(Broken {
                observation_ignores_marker: true,
                ..Broken::default()
            }),
            READY_ONLY_WITH_THE_DISK,
        );
        let broken = last(&path);
        assert!(broken.host.marker && !broken.host.intent);
        assert_eq!(
            first(&path).installer,
            Installer::Done,
            "recorded before the fix"
        );
    }

    /// Without `local_storage` reading the intent, an unrecorded device Pool reads Ready
    /// once its disk is missing.
    #[test]
    fn the_fix_needs_local_storage_to_read_the_intent() {
        let path = breaks(
            fixed_without(Broken {
                observation_ignores_intent: true,
                ..Broken::default()
            }),
            READY_ONLY_WITH_THE_DISK,
        );
        let broken = last(&path);
        assert!(broken.host.intent && !broken.host.marker);
    }
}
