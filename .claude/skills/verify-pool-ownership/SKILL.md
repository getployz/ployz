---
name: verify-pool-ownership
description: Model-check who owns a Machine's Machine Pool, between the installer's named-device path (install_at's installation gate, prepare_storage, create_device_pool, existing_device_pool, remember_device_pool and its CONFIRMED marker) and the volume plugin's first Pool (VolumeStorage::create and copy's first_pool under admit_mutation and lock_mutation, one_usable, recover, has_device_pool, PoolStorage::create) and its ungated List and Inspect, against the installer stopping between two steps, the disk going missing and returning, and ZFS's cache-file import at boot, with what local_storage reports, using the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployzd/src/installer/storage.rs (prepare_storage's named-device path, create_device_pool, existing_device_pool, remember_device_pool), core/crates/ployzd/src/installer/mod.rs (install_at, install_locked), core/crates/ployzd/src/installer/host.rs (activate_units), core/crates/ployzd/src/main.rs (ployzd install's --storage default), core/crates/ployzd/src/docker/observe.rs (the Docker volume rescan), core/crates/ployzd/src/mutation.rs (MutationGate), core/crates/ployzd/src/volume_plugin/pool.rs (VolumeStorage::create, lock_mutation, one_usable, admit, recover, has_device_pool, create), core/crates/ployzd/src/volume_plugin/storage.rs (admit_mutation), core/crates/ployzd/src/volume_plugin/removal.rs (list, inspect), core/crates/ployzd/src/volume_plugin/copy.rs (admit, first_pool), core/crates/ployzd/src/machine_pool.rs (one_usable, the marker and backing paths), or core/crates/ployzd/src/machine/mod.rs (local_storage).
---

# Verify Machine Pool ownership

The model in `core/crates/ployz-verify/src/pool_ownership.rs` explores every interleaving of one installer attempt, up to two Volume creates, and any number of List or Inspect calls on one Machine with one selected disk. The attempt either runs or is refused at once by the installation gate. The model starts from a fresh host and from a host where an earlier `ployzd install` left a release. The fix variant also starts from a Machine the code as it is recorded.

## What the model copies

**The installation gate.** `install_at` tries the `MutationGate`'s exclusive guard once, with `try_installation`, before anything else (`core/crates/ployzd/src/installer/mod.rs:259-260`, `core/crates/ployzd/src/mutation.rs:77-81`). It holds the guard through `install_locked`, which calls `prepare_storage` (`installer/mod.rs:264`, `:286`). `install_locked` takes the guard as a parameter (`:268`), so its other caller, the upgrade's compensation, holds it too (`installer/compensation.rs:48`, `:68`). The plugin's `admit_mutation` takes the same gate's shared guard with `try_mutation` (`core/crates/ployzd/src/volume_plugin/storage.rs:156-166`, `mutation.rs:55-70`). Both try once and never wait (`try_lock_shared` at `mutation.rs:57`, `try_lock` at `:79`).
- While the installer holds the gate, a Volume create fails with "a Ployz installation or upgrade is active" (`mutation.rs:26-27`) and runs no Pool command.
- While a Volume create holds it, `install_at` returns at once, and the installer prints "another Ployz installation is active" (`installer/mod.rs:92-93`, `:342-350`). The model ends the attempt there (`InstallerBusy`).
- A crash or a stop releases the guard, since each guard is a lock held on an open file (`mutation.rs:34`, `:37`). `installation_and_active_handoff_exclude_mutations_across_instances` (`mutation.rs:167`) pins the exclusion.
- Not every plugin caller of `admit` holds the gate. List (`volume_plugin/removal.rs:96-104`) takes the plugin's in-process mutation lock (`:97`) and then the Pool mutation lock (`:98`), and Inspect (`:48-53`) takes only the Pool mutation lock (`:49`). Neither takes the installation gate, so they run while the installer does. The others take `admit_mutation` first: Create (`pool.rs:102`), remove (`removal.rs:17`), copy (`copy.rs:145`), size (`size.rs:28`), capacity's inspect and prepare (`capacity.rs:122`, `:137`) and `mountpoint` (`storage.rs:297`). The installer takes no Pool mutation lock. A scratch `ployzd` test from PR #3842's round-2 review held the exclusive installation guard through a List over the plugin socket: List answered `{"Err":"","Volumes":[]}`, and `zpool import` ran, so `recover` did.

**Where the plugin comes from.** `install_locked` runs `prepare_storage` (`installer/mod.rs:289`) before `install_binaries`, `install_units` and `activate_units` (`:301-304`). `activate_units` enables the volume plugin's socket whatever the storage (`installer/host.rs:261-274`, `:270-273`). So on a fresh host no plugin exists until an install gets past `prepare_storage`.
- Cloud's enroll prepares ZFS "without starting Ployz" (`core/crates/ployzd/src/main.rs:125`), so it is a fresh host too.
- `ployzd install`'s `--storage` defaults to `none` (`main.rs:108-109`), and `prepare_storage` does nothing for it (`installer/storage.rs:33`). With `--storage zfs` and no device it prepares ZFS and creates no Pool (`:36-41`). Either run installs the release and its plugin.
- A later `ployzd install --storage zfs:<device>` on that host runs `prepare_storage` again (`installer/mod.rs:283-289`), with the earlier release's plugin socket still enabled. `docs/user/servers/add-a-server.md:207-208` names that command. Its backing-file check refuses only once a Volume has built `ployz` (`installer/storage.rs:48-53`).

The model's `release` bit is that plugin. A fresh host gets it only when the installer records the device Pool. The second starting host has it, with ZFS prepared and no Pool, as after `--storage zfs` with no device. `Request`, `List` and `Observe` are offered only once `release` holds.

**What closes the route.** Two things record ownership while the disk is present and `ployz-device` imported:
- **The daemon's Docker volume rescan.** On an enrolled Machine it runs every 30 s (`core/crates/ployzd/src/docker/observe.rs:20`). It is attached by `ContainerRuntime::replicating` (`:130-143`). It calls the plugin's List (`volume_plugin/removal.rs:96-104`), which runs `admit` and so `one_usable`, and that writes CONFIRMED (`pool.rs:236-245`). The model has List as a step it may take or not, so it has paths where List confirms the Pool and paths where it never runs. It has no clock, so it cannot force the rescan within 30 s. `docs/user/servers/add-a-server.md:197-199` documents it: "Ployz records device ownership before serving volumes, including when the operating system already imported the pool." Boat lane F1's run 1 enrolled first and saw the plugin write the marker 2.3 seconds after the installer was killed, at a rescan tick (PR #3842's Live lanes).
- **A rerun of the failed install.** It finds the imported `ployz-device` and records it (`installer/storage.rs:57-58`). A rerun is out of the model's bounds.

A Volume create or copy records it the same way. The model has that step: `one_usable` confirms an unrecorded device Pool (a `COVERAGE` row). So on a real host, finding 1 needs the disk missing before any of these runs. Two routes stay open:
- **(a) An operator's `ployzd install` before `ployz server add`.** No rescan runs before enrolment. Lane F1's run 2 reproduced finding 1 end to end this way on trunk.
- **(b) An enrolled Machine whose disk goes missing before the next rescan.** For example, the host crashes or loses power right after `zpool create` and boots without the disk. This was not run.

**The installer's named-device path.** It is `prepare_storage` (`installer/storage.rs:40-73`). It resolves the device (`:44`), refuses when the backing file exists (`:48-53`), runs `existing_device_pool` (`:57`, `:62`) and `empty_device` (`:60`, `:63`), and calls `create_device_pool` (`:64`). That function runs `zpool create ployz-device` (`:96-106`), checks the Pool with `existing_device_pool` (`:107-112`), and only then calls `remember_device_pool` (`:113`). `remember_device_pool` writes `CONFIRMED_DEVICE_POOL_MARKER` (`core/crates/ployzd/src/machine_pool.rs:6`) to `/var/lib/ployz-machine-pool.device`, which `MachineLayout::device_pool_marker` spells (`core/crates/ployz-core/src/machine_layout.rs:118-122`), with `write_file_atomically` (`core/crates/ployzd/src/installer/storage.rs:77-83`).

**The plugin's first Pool.** `VolumeStorage::create` (`core/crates/ployzd/src/volume_plugin/pool.rs:96-124`) takes the gate (`:102`), then the Pool mutation lock (`:103`, `lock_mutation` at `:173-206`), and calls `admit` (`:104`, `:255-260`). `admit` runs `one_usable` (`:213-249`), then `recover` (`:276-366`) when nothing is imported. When `admit` returns `None`, `create` builds the root-backed `ployz` (`:111`, `:390-470`), and the Volume is created on it (`:112-119`). Copy takes the gate in its `admit` (`core/crates/ployzd/src/volume_plugin/copy.rs:143-150`), and its `first_pool` takes the lock and the same path. The model copies these pieces:

- `one_usable` reads `zpool list` through `machine_pool::one_usable` (`machine_pool.rs:63-87`). It refuses the backing file beside device ownership (`pool.rs:228-235`), and writes CONFIRMED for an imported `ployz-device` when the marker holds anything else (`:236-245`).
- `recover`, with no backing file, imports a `ployz-device` that `zpool import` lists (`:300-305`). It refuses when the marker exists (`:307-312`), and otherwise returns `Ok(None)` (`:313`).
- `has_device_pool` checks only that the marker exists (`:368-373`).
- List and Inspect run the same `admit` and create no Pool (`removal.rs:96-104`, `:48-53`). The model takes each as two steps, offered whenever no Volume create holds the Pool lock, including while the installer runs: `List` is `one_usable`, and `ListRecover` is `recover` when `one_usable` found nothing. The installer can step between them. A List whose `one_usable` read `zpool list` before the installer's `zpool create`, and whose `recover` runs after `remember_device_pool`, finds no importable Pool and the marker, and refuses while `ployz-device` is imported (`a_list_across_the_install_refuses_beside_the_imported_device`). One step could not reach that: a List that starts after the installer finds `ployz-device` in `one_usable`. A scratch `ployzd` test from PR #3842's round-1d review (`scratch_list_nonatomic_admission`, with a fake `zpool`) held the installation guard, let List's `zpool list` read empty, then wrote CONFIRMED and listed `ployz-device`. List answered `{"Err":"device Machine Pool is unavailable; refusing to create a root-backed Pool","Volumes":[]}`. The refusal is transient: the next List's `one_usable` finds `ployz-device` imported and returns it (the `COVERAGE` row "one_usable returns the recorded device Pool").
- `create` refuses on the marker (`:391-395`), creates the backing file (`:400-411`), runs `zpool create ployz` on it (`:421-443`), and its `one_usable` returns `ployz` (`:444-453`). The model takes that, and the Volume create after it, as one step.

**`local_storage`** (`core/crates/ployzd/src/machine/mod.rs:689-717`) reads `zpool list` alone. It answers Ready for no Pool (`:710`), Pool for one (`:705-709`), and `None` when `machine_pool::one_usable` refuses (`:711`).

**ZFS at boot.** This is an outside fact, verified on one host only: a Boat on Ubuntu 24.04.4 with ZFS 2.2.2 (`zfs-2.2.2-0ubuntu9.5`), rebooted with sysrq. The table of what the cache file listed after each step is in PR #3842's Summary.

- Neither `zpool create` sets a cachefile (`installer/storage.rs:96-106`, `pool.rs:421-443`), so `zpool get cachefile` reads `default` for both Pools, which is `/etc/zfs/zpool.cache`.
- At a boot without the disk, `zfs-import-cache`'s import of `ployz-device` fails and leaves the file as it is.
- A Pool change rewrites the file from the Pools imported at that moment. On the run, the plugin's `zpool create ployz` while the disk was missing left the file listing only `ployz`. OpenZFS writes the file in `spa_write_cachefile` (https://github.com/openzfs/zfs/blob/zfs-2.3.4/module/zfs/spa_config.c#L228-L289).
- `zfs-import-cache` runs `zpool import -c /etc/zfs/zpool.cache -aN`, so it imports only what the file lists. `zfs-import-scan` is disabled by Ubuntu's preset, and its `ConditionFileNotEmpty=!/etc/zfs/zpool.cache` is false while the file lists any Pool.
- A disk attached while the host runs is not imported. On the run, `ployz-device` was still not imported 60 seconds after the attach.
- With no Pool change while the disk was missing, the next boot with the disk imports `ployz-device`.

The installer's RPM path's note says the RPMs enable `zfs-import-cache` (`installer/storage.rs:707-709`). The run did not check the RPM or Debian presets. To check another distro, use Boat lane F1's disk (PR #3842's Live lanes):

1. Boot one Machine with the `verify-server` skill (lane F1 used `scripts/verify-cluster up --bare --machines 1 --daemon checkout`). Do not use a loop device. It is gone after a reboot, and the installer refuses it: `empty_device` keeps only devices that lsblk reports as TYPE `disk` (the `type == "disk"` filter in `disks_from_lsblk` and the `device_blocker` check in `empty_device`, both in `installer/assess.rs`), and a loop device reports `loop`.
2. Attach a sparse file as `/dev/nbd0` with `qemu-nbd`, from a unit that runs before ZFS imports. This is lane F1's unit, after `apt-get install qemu-utils`, `mkdir -p /var/lanef1 && truncate -s 4G /var/lanef1/disk.img` and `echo "options nbd nbds_max=1 max_part=0" > /etc/modprobe.d/lanef1-nbd.conf`:

   ```ini
   [Unit]
   Description=lane-f1 test disk: attach /var/lanef1/disk.img as /dev/nbd0 before ZFS import
   DefaultDependencies=no
   After=systemd-remount-fs.service
   Before=systemd-udev-settle.service zfs-import-cache.service zfs-import-scan.service zfs-import.target
   RequiresMountsFor=/var/lanef1

   [Service]
   Type=forking
   ExecStartPre=/usr/sbin/modprobe nbd
   ExecStart=/usr/bin/qemu-nbd --format=raw --cache=writethrough --connect=/dev/nbd0 /var/lanef1/disk.img
   ExecStartPost=/usr/bin/udevadm settle

   [Install]
   WantedBy=sysinit.target
   ```

   Lane F1 saved it as `/etc/systemd/system/lanef1-disk.service` and ran `systemctl daemon-reload`. For the first install, lane F1 ran `systemctl enable lanef1-disk.service` and `systemctl start lanef1-disk.service`, which attaches the disk with no reboot, and then passed it as `ployzd install --storage zfs:/dev/nbd0` (PR #3842's Live lanes, step 2). Run `systemctl enable lanef1-disk.service` before a boot with the disk present, and `systemctl disable lanef1-disk.service` before a boot with the disk missing. On lane F1's boots, `nbd0` attached at 2.0 to 3.8 seconds, before `zfs-import-cache` started at 3.83 seconds.
3. Reboot with `sync`, then sysrq `b` (`echo b > /proc/sysrq-trigger`), and poll `/proc/sys/kernel/random/boot_id` until it changes. Lane F1's reboots took about 10 seconds.
4. If the plugin must not run between steps, stop and mask `ployz.service`, `ployz.socket` and `ployz-volume-plugin.socket`. On an enrolled Machine, the rescan's List writes CONFIRMED (`pool.rs:236-245`) and changes what you measure. Lane F1 did not mask them. Its run 2 kept the Machine unenrolled until the step that needed the plugin.
5. After each step, record `zdb -C -U /etc/zfs/zpool.cache`, `zpool list` and `zpool import`, and `systemctl list-unit-files 'zfs*'` once.

**The world's steps.** The host boots without the disk (`DiskMissing`), the disk is attached again (`DiskReturns`), and `zfs-import-cache` imports `ployz-device` at boot (`AutoImport`). `AutoImport` is offered only while `cache_lists_device` holds. The installer's `zpool create` and every import of `ployz-device` set it. The plugin's `zpool create ployz` clears it when `ployz-device` is not imported. `DiskMissing`'s row says whether the cache file listed `ployz-device` to fail on.

Nothing calls the real installer or plugin. The model is a hand copy, and it drifts unless you keep it in step. `ployz-verify` dev-depends only on `ployz-core` and `stateright` (`core/crates/ployz-verify/Cargo.toml`). So a product change leaves the model green, without even recompiling it, until you copy the change in.

These are the model's steps:

- Installer: `Assess` tries the gate, then runs the checks from `:44` to `:63`. While a Volume create holds the gate, the attempt ends there with `InstallerBusy`. Then `zpool create` (`:96`), `existing_device_pool` (`:107`) and `remember_device_pool` (`:113`). The `Record` step includes the binaries, units and plugin socket that `install_locked` installs after `prepare_storage` returns (`installer/mod.rs:301-304`).
- The installer can stop between any two steps (`InstallerStops`). After `zpool create` the stop is a crash, or `existing_device_pool`'s backing check refusing with `ployz-device` still imported (`installer/storage.rs:214`). Elsewhere it is a crash. The refusal at `:225-228` also leaves `ployz-device` imported, but only when the Pools change between the `zpool list` at `:166-171` and the one at `:215-220`, so the model leaves it out.
- Plugin: a request (`Request`), which the gate refuses while the installer runs, then `one_usable`, `recover` and `create`. Each is one step, and the lock keeps a second request out until the first returns.
- Plugin: List or Inspect, which the gate does not refuse: `List` runs `one_usable`, then `ListRecover` runs `recover` when nothing is imported. Each is one step under the Pool lock, and the installer can step between them.
- `local_storage` is one step.

The properties, each one sentence:

- `NO_ROOT_BESIDE_DEVICE`: **no root-backed `ployz` Pool exists while a disk carries a `ployz-device` label.** This is finding 1.
- `NOTHING_STRANDED`: **the plugin never refuses a root-backed Pool for device ownership while the disk is present and carries no `ployz-device` label.** It holds on the code as it is, and it is what stops a fix from recording ownership before `zpool create`.
- `READY_ONLY_WITH_THE_DISK`: **`local_storage` never reads Ready while the disk that carries the `ployz-device` label is missing.** This is finding 2.

## Run it

```
cd core && cargo nextest run -p ployz-verify pool_ownership --no-capture
```

`cd core && cargo test -p ployz-verify pool_ownership -- --nocapture` runs the same tests. On a warm build, nextest's Summary line puts the twenty-one tests under a second; the whole command, with cargo's build check, took 0.8 to 3.6 seconds of wall time over three runs; the slowest was the first after an edit, which relinks the test binary. The code as it is has 394 states and the fix 574 (`current_code_breaks_only_the_findings` and `an_intent_keeps_one_owner` print them). Each story prints under its test's name, in brackets above it (`first_path`). CI runs `ployz-verify` in its workspace nextest (`.github/workflows/ci.yml:151`, `:216`), so a red model blocks the PR.

The run is green when all twenty-one tests pass. Read them as five groups.

- **The findings.** Four tests run on the code as it is.
  - `current_code_builds_a_root_pool_beside_an_unrecorded_device_pool` is finding 1, the shortest path that breaks `NO_ROOT_BESIDE_DEVICE`. It starts from the host with an earlier release.
  - `a_fresh_host_never_builds_a_root_pool_beside_the_disk` checks a model that starts only from the fresh host (`fresh_only`, 96 states). There, no path breaks the property, and a Volume create still runs: the test asks for a state that `OneUsable` reached, and only `Request` leads to `OneUsable`, since a List runs its own `one_usable`. With `requests: 0` the test goes red at 28 states. To check that, copy the test into a scratch test (Breaking a guard yourself) and add `requests: 0` to its `fresh` model, rather than editing the test. A filter on the start would not do: the checker keeps only the first path to each state. Starting the fresh-only model from a host with an earlier release turns the test red (383 states).
  - `current_code_keeps_the_root_pool_once_the_disk_returns` prints finding 1's path on to the disk being attached again. It also asserts that no path on the code as it is imports `ployz-device` beside `ployz`.
  - `current_code_reads_ready_while_the_recorded_device_is_missing` is finding 2. It keeps to paths where `remember_device_pool` ran.

  `current_code_breaks_only_the_findings` checks the code as it is. It expects `NO_ROOT_BESIDE_DEVICE` and `READY_ONLY_WITH_THE_DISK` to break, `NOTHING_STRANDED` to hold, and every `COVERAGE` row that is not `fix_only` to be reached. When the product fix lands, model it as the code writes it and make it the default. Then turn each finding test into a `Broken` flag that restores the old code, as `removal_marks.rs` `finish_needs_its_conditional_save` did.
- **The List straddle.** Two replays step a List across the install by hand (`replay`). `a_list_across_the_install_refuses_beside_the_imported_device` is the schedule above, on the code as it is. `the_fix_refuses_a_list_across_zpool_create` is the same straddle under the fix: the List's `one_usable` runs after the intent and before `zpool create`, and its `recover`, before `remember_device_pool`, refuses on the intent with `ployz-device` imported, where the code as it is returns `Ok(None)` and lists nothing.
- **The fix variant and coverage.** `an_intent_keeps_one_owner` runs `fixed()`: every property holds and every `sometimes` is reached. Each `sometimes` is one `COVERAGE` row naming a branch the code handles, and `current_code_breaks_only_the_findings` expects each row that is not `fix_only` reached on `as_is()`. Making the "installer records CONFIRMED" row unreachable, by adding `&& false` to its predicate, turned both tests red. Each row's predicate requires the branch it names (`took`), so a check that cut that branch from the model and expected the row unreached could not fail; the model has no such check. `some_branches_need_a_broken_guard` shows that `VerifyRefuses`, `Ambiguous`, `Conflict` and `CreateRefusesOwned` are reached on neither variant, which is why they have no row, and that a `Broken` flag reaches each on the code as it is.
- **The code's guards.** Six tests take guards out with `Broken` flags, all on the code as it is, `as_is_without(..)`. Four drop one guard each and expect a property to fail. One drops the two marker refusals together and expects a property to fail. One drops `recover`'s marker refusal alone and expects the property to hold, since `create`'s refusal backs it up. Their characterization tests run in the `ployzd` command under Bounds.
  - `the_installation_gate_keeps_volume_creates_out_of_the_install` sets `gate_admits_during_install`, which admits a Volume create while the installer runs and lets the installer start beside one (`installer/mod.rs:260`, `volume_plugin/storage.rs:160-163`). A Volume create then builds `ployz` after the installer's checks, and the installer's `zpool create` labels the disk beside it, with no disk missing. The test first asserts that the code as it is breaks `NO_ROOT_BESIDE_DEVICE` only on paths where the disk went missing. The characterization test is `installation_and_active_handoff_exclude_mutations_across_instances` (`mutation.rs:167`).
  - `ownership_is_recorded_only_after_zpool_create` sets `records_before_create`, which puts `remember_device_pool` before `zpool create`, as 0d1e04b143 had it before cc647240a5 moved it after `existing_device_pool` (`installer/storage.rs:107-113`). It breaks `NOTHING_STRANDED` on a path where the installer stopped. A List while that installer still runs refuses too, so the test filters to the stop. The characterization test is `device_creation_commits_ownership_only_after_confirmation` (`core/crates/ployzd/src/installer/storage/tests.rs:1047`).
  - `one_usable_needs_to_confirm_an_unrecorded_device_pool` sets `one_usable_never_confirms`, dropping `one_usable`'s CONFIRMED write (`pool.rs:236-245`). It breaks `NO_ROOT_BESIDE_DEVICE` on a path where the plugin used the device Pool before the disk went missing. The characterization test is `imported_device_admission_preserves_ownership_when_device_later_disappears` (`pool.rs:870`).
  - `the_installer_needs_its_root_pool_refusal` sets `installer_ignores_root`, dropping the backing check (`installer/storage.rs:48-53`) and `existing_device_pool`'s `ployz` refusal (`:172-181`). It breaks `NO_ROOT_BESIDE_DEVICE` on a path with no disk missing.
  - `recovery_needs_the_marker_refusals` sets `recover_ignores_marker` and `create_ignores_marker`, dropping the marker refusals in `recover` and `create` (`pool.rs:307-312`, `:391-395`). It breaks `NO_ROOT_BESIDE_DEVICE` on a path where `remember_device_pool` ran. The characterization tests are `imported_device_admission_preserves_ownership_when_device_later_disappears` (`pool.rs:870`) and `a_missing_device_pool_never_becomes_a_file_backed_pool` (`pool.rs:1073`).
  - `create_refuses_when_recover_ignores_the_marker` sets `recover_ignores_marker` alone. The property holds on paths where `remember_device_pool` ran, and the test prints the path where `create` refuses.
- **The fix's parts.** Six tests each drop one part of the fix on `fixed_without(..)` and expect the property it keeps to fail. They are below, under The fix.

If a test that drops a guard goes from green to red, the guard no longer earns its keep, or the model lost the path that shows it. If `create_refuses_when_recover_ignores_the_marker` goes red, `create`'s refusal no longer backs up `recover`'s.

### Breaking a guard yourself

Set its `Broken` flag through `as_is_without` or `fixed_without`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every test that goes red is listed.

An edit to the model's code changes every model at once, so other tests go red as a side effect:
- **An edit to fix code** changes every `fixed()` model. Deleting the `WriteIntent` arm of `installer_step` turned `an_intent_keeps_one_owner` red, as expected. It also turned `the_fix_needs_recover_to_heed_the_intent`, `the_fix_needs_a_stale_intent_to_clear`, `the_fix_needs_list_to_leave_a_live_intent`, `the_fix_needs_local_storage_to_read_the_intent` and `the_fix_refuses_a_list_across_zpool_create` red, since they need an intent to act on.
- **An edit to a code guard** changes `as_is()` as well. It moves the code-as-is baseline, so the finding tests, which pin exact steps, and `current_code_breaks_only_the_findings` go red too. Making `gated()` return `false` turned 12 of the 21 tests red. No `Broken` flag drops one half of the gate: `gate_admits_during_install` drops both, so either half alone can be broken only by a direct edit. Editing the `Assess` arm's Busy check to `if false`, or the `Request` arm's refusal to `if false`, turned the same 12 red. Under any of these edits the gate test's first assertion fails: it asserts that the code as it is never breaks `NO_ROOT_BESIDE_DEVICE` with the disk present, and it prints the edited `as_is()` path that does.
- **An edit to one fix part** can also add a shorter path that another fix part's test pins. Deleting `marker ||` from `observe`'s `(marker || intent)` turned `an_intent_keeps_one_owner` red, as expected. It also turned `the_fix_needs_local_storage_to_read_the_intent` red, at its assertion that the break ends with an intent and no marker, since the shortest `READY_ONLY_WITH_THE_DISK` break now starts at the Machine recorded before the fix, which has a marker and no intent.

A side-effect failure is a test whose own flag drops a part that your edit already took out or starved, or a test whose pinned path your edit moved: a finding test, or a fix-part test that asserts its path's shape. A guard that no longer earns its keep fails with its flag alone, on a model with no other edit.

A scratch test inside `pool_ownership::tests` can call the module's private helpers. Run it with `cd core && cargo nextest run -p ployz-verify pool_ownership::tests::scratch --no-capture`. It also adds to the twenty-one tests the full run counts.

```rust
#[test]
fn scratch() {
    let model = fixed_without(Broken { intent_never_clears: true, ..Broken::default() });
    let checker = check(model);
    println!("{} states", checker.unique_state_count());
    for name in [NO_ROOT_BESIDE_DEVICE, NOTHING_STRANDED, READY_ONLY_WITH_THE_DISK] {
        let broke = checker.discovery(name).is_some();
        println!("{name}: {}", if broke { "breaks" } else { "holds" });
    }
    // A COVERAGE row's name is the name of its `sometimes` property.
    breaks(fixed(), COVERAGE[0].name);
}
```

## The fix

The fix keeps the marker's bytes and adds three parts. They are a design for a later PR. No product code has them yet.

1. Before `zpool create`, the installer atomically writes an intent: a sibling file of the marker, naming the device.
2. With no importable `ployz-device` and no marker, `recover` reads the intent. It refuses while the intent's disk is missing or labelled. It removes an intent whose disk is present with no label, since `zpool create` never landed there, and goes on as the code does, but only for a caller that holds the installation gate's shared guard. List and Inspect hold none (`removal.rs:96-104`, `:48-53`), so for them `recover` leaves the intent and returns `Ok(None)`: their installer may still be before `zpool create`. `recover` imports a present labelled disk first (`pool.rs:300-305`) unless the `zpool import` scan does not list it. Within the bounds that is a List across `zpool create`: its `one_usable` ran before, its `recover` after it and before `remember_device_pool`, and the scan does not list an imported Pool (`the_fix_refuses_a_list_across_zpool_create`). A label the scan skips for another reason has no state in the model.
3. With no Pool listed, `local_storage` reports `None` where `recover` would refuse: no importable `ployz-device`, and the marker, or an intent that still stands.

The fix needs no lock of its own, but it needs `recover` to know whether its caller holds the gate. A Volume create or copy holds the shared guard from before `recover` to after `create` (`pool.rs:102-111`, `copy.rs:143-150`), so no installer runs while it reads an intent (`mutation.rs:77-81`). List and Inspect run while the installer does. If List cleared a stale-looking intent, it could do so while the installer is between the intent and `zpool create`; the installer would then label the disk and could stop before CONFIRMED, and a boot without the disk would bring finding 1 back (`the_fix_needs_list_to_leave_a_live_intent`). So in the product, `recover` (`pool.rs:276`) would take whether its caller holds the shared guard, and its clear would need it. `admit` (`:255-260`) passes it through, and copy's `first_pool`, which calls `recover` directly (`copy.rs:178-184`), passes yes.

The model takes this rule rather than having List and Inspect take the shared guard. That other rule would let the rescan clear a stale intent. But a List holding the shared guard makes the installer's `try_installation` fail at once (`mutation.rs:79`), so an install (`installer/mod.rs:260`) or a new upgrade (`machine/local_machine/upgrade.rs:38`, after the replay of an existing attempt at `:26-30`) that starts during any rescan's List would fail. Settling an existing attempt still succeeds: `settle` ignores Busy (`installer/upgrade.rs:258-260`, `:268-270`). Also, and List and Inspect would fail with "a Ployz installation or upgrade is active" during an install (`mutation.rs:26-27`).

The clear needs a gated caller to run while the disk is present. If an install dies before `zpool create` and its disk goes missing before any gated caller runs, every Volume create refuses on the intent (`RefusesIntent`; the `COVERAGE` row "recover refuses while the disk of an intent left before zpool create is missing"), `local_storage` reads `None`, and `server show` and Cloud print "Volume support unknown" (`show.rs:186`, `server-status.ts:7`), until the disk returns. Once it returns, the next gated caller clears the intent, and `create` builds the same root-backed `ployz` the code as it is would have built at once (`ClearsStale` goes on to `CreateRoot`, the `COVERAGE` row "recover clears an intent the installer left before zpool create"). In both, a later `ployzd install --storage zfs:<device>` refuses on the backing file (`installer/storage.rs:48-53`). So the fix's cost is the refusal window, not a different end state.

The fix never removes an intent after `remember_device_pool`. Once the marker exists, `recover` refuses on the marker first, so nothing in the bounds needs the intent gone.

`fixed()` also starts from a Machine the code as it is recorded: `ployz-device` imported and CONFIRMED, with no intent. Machines the current installer recorded have a marker and no intent. This start is the one among them with the disk present and imported, and from it the model explores the disk going missing, which is finding 2 (`current_code_reads_ready_while_the_recorded_device_is_missing`). It is the only start with a marker and no intent, so it is what tests `local_storage`'s marker read.

Each part has a `Broken` flag and a test that breaks the property it keeps on `fixed_without(..)`:

| Part of the fix | `Broken` flag | Test | Property it keeps |
|---|---|---|---|
| The intent before `zpool create` | `no_intent` | `the_fix_needs_the_intent` | `NO_ROOT_BESIDE_DEVICE` |
| `recover` refuses on an intent that stands | `recover_ignores_intent` | `the_fix_needs_recover_to_heed_the_intent` | `NO_ROOT_BESIDE_DEVICE` |
| `recover` clears an intent whose disk is present with no label | `intent_never_clears` | `the_fix_needs_a_stale_intent_to_clear` | `NOTHING_STRANDED` |
| `recover` clears it only for a caller holding the gate | `ungated_clears_intent` | `the_fix_needs_list_to_leave_a_live_intent` | `NO_ROOT_BESIDE_DEVICE`, and `READY_ONLY_WITH_THE_DISK` on the same path |
| `local_storage` reads `None` for the marker | `observation_ignores_marker` | `the_fix_needs_local_storage_to_read_the_marker` | `READY_ONLY_WITH_THE_DISK` |
| `local_storage` reads `None` for an intent that stands | `observation_ignores_intent` | `the_fix_needs_local_storage_to_read_the_intent` | `READY_ONLY_WITH_THE_DISK` |

To take a part out by a direct edit instead of its flag, edit these lines in `pool_ownership.rs`. Read Breaking a guard yourself first, since a direct edit changes every `fixed()` model.
- The intent: the `Installer::Assessed if self.fix && !broken.no_intent => Some(Action::WriteIntent)` arm of `installer_step`.
- `recover`'s refusal: `} else if self.fix && state.host.intent && !broken.recover_ignores_intent {` in `recover`.
- The stale-intent clear: `if !self.intent_is_stale(state.disk) {` in `recover`. `observe` calls `intent_is_stale` too, so leave that call as it is. Making it `if true {` turned `an_intent_keeps_one_owner` red, and `the_fix_needs_list_to_leave_a_live_intent` as a side effect, since its flag's clear never runs.
- The clear only for a gated caller: `} else if gated || broken.ungated_clears_intent {` in `recover`. Making it `true || gated || ...` turned `an_intent_keeps_one_owner` red on `NO_ROOT_BESIDE_DEVICE`, and no other test. With the flag, a scratch check found `READY_ONLY_WITH_THE_DISK` broken too: after the boot without the disk, `local_storage` reads Ready, since the List removed the intent.
- `local_storage`'s marker and intent reads: `(marker || intent)` in `observe`.

`the_fix_needs_a_stale_intent_to_clear` is the model's installer that fails before `zpool create`. It writes the intent, stops, and leaves the disk with no label. A `recover` that refuses on any intent then refuses on that host. The story ends on a List's refusal; a Volume create there takes the same branch. A List while that installer still runs refuses too, so the test filters to the stop.

`the_fix_needs_list_to_leave_a_live_intent` is Codex's race. The installer writes the intent, a List's `recover` clears it while the disk is still blank, `zpool create` labels the disk, the installer stops, and a boot without the disk leaves `recover` with nothing to refuse on.

Every `always` holds on `fixed()`, so passing it to `breaks` with an `always` panics with "a path breaks the property". To see a fix part's break, pass the fix with that part dropped: `breaks(fixed_without(Broken { intent_never_clears: true, ..Broken::default() }), NOTHING_STRANDED)`. To print the first path that reaches a `sometimes`, pass its `COVERAGE` row's name to `breaks`, which looks for a state where a `sometimes` holds (`first_break`): `breaks(fixed(), "recover clears an intent the installer left before zpool create")`. To print every row's first path, loop over `COVERAGE` and call `breaks(model, row.name)` for `fixed()`, and for `as_is()` on the rows that are not `fix_only`.

Notes for the PR that ships the fix:

- `local_storage` gets heavier. Today it runs `zpool list` alone (`machine/mod.rs:689-717`). To read `None` where `recover` would refuse, it also needs an import scan, the marker and the intent. It reads them under neither the gate nor the lock, as it reads `zpool list` today.
- Check every reader of `observe_storage` (`machine/local_machine.rs:506-507`, `machine/local_machine/container.rs:112`, `:1308`). `None` makes the Machine's own container create refuse through `require_eligible` with `StorageUnobservable` (`container.rs:112-116`, `docker/lifecycle.rs:922-928`, `:936-938`), where Ready let it through.
- The stale-intent clear trusts the device the intent names. Name it by a stable ID, such as a `/dev/disk/by-id` path, so a renamed device is not read as present with no label.
- Machines already in finding 1's state, with `ployz` beside an unimported `ployz-device`, have no intent and no marker, so the fix does not repair them.
- `recover` gains a parameter saying whether the caller holds the installation gate's shared guard (`pool.rs:276`). `admit` passes it through (`:255-260`): every caller that takes `admit_mutation` passes yes, and List and Inspect pass no (`removal.rs:96-104`, `:48-53`). Copy's `first_pool` calls `recover` directly (`copy.rs:178-184`) and passes yes. A new caller of either has to choose.
- Under the fix, a `docker volume ls` or a rescan whose List runs `one_usable` before `zpool create` and `recover` before `remember_device_pool` answers with the intent's refusal, where the code as it is answers with no Volumes (`the_fix_refuses_a_list_across_zpool_create`). A List whose `recover` runs after `remember_device_pool` refuses on the marker under both: `a_list_across_the_install_refuses_beside_the_imported_device` shows it for the code as it is, and the same replay under `fixed()`, with `WriteIntent` added, took `RefusesOwned` in a round-1e scratch run, since the fix checks the marker before the intent. Either refusal ends at the next List while `ployz-device` stays imported, since that List's `one_usable` returns it.
- While the intent's disk is missing, nothing on the Machine can tell an intent left before `zpool create` from one left after it, which is the case the fix refuses on. A rerun of `ployzd install --storage zfs:<device>` fails first, resolving the missing device (`installer/storage.rs:44`). The ways out are to reattach the disk, after which the next gated caller clears the intent, or for the operator to delete the intent, accepting a root-backed Pool beside a label if `zpool create` had landed. An installer rerun that finds an intent from an earlier attempt with its disk present needs a rule too. The model runs one attempt, so it says nothing about that rerun.
- Bet 3 says "A removal mark is the one persisted intent, finished by later admissions, never by a timer" (`core/DESIGN.md:93-97`). The intent file is a second one, finished the same way, by a later gated admission. The PR that ships the fix must amend bet 3 or argue why the file is not a persisted intent.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. For a `sometimes`, the story is cut at the first state that reaches it. `breaks` keeps that first path with a visitor and stops at the first discovery (`finish_when(HasDiscoveries::AllOf(..))`). `breaks_where` keeps the first path that also passes a filter on the path, and explores the whole model. The checker visits each state once, so the filter sees only the first path to each state. Each row is one step in the code's words, then the disk, the cache file, the host's release and files, the installer and the plugin, for example:

```
 7. Plugin: recover; no backing file, no importable ployz-device, no marker: returns Ok(None) (:313) (pool.rs recover)
    disk: present=false label=true imported=false cached=true | host: release=true marker=false root=false | installer: Stopped | plugin: Creating
```

`present` is whether the host sees the disk, `label` whether it carries a `ployz-device` label, and `imported` whether `ployz-device` is imported. `cached` is whether the cache file lists `ployz-device`. `release` is whether a release with its volume plugin is installed. `marker` is whether `/var/lib/ployz-machine-pool.device` holds CONFIRMED. `root` is whether the root-backed `ployz` exists on `/var/lib/ployz-machine-pool`. Under the fix, `intent` is whether the intent file exists. `installer` and `plugin` are where each is: the next step of `prepare_storage` and `create_device_pool`, and of `VolumeStorage::create` or a List. Row 0 names the starting host. A row says `(under Broken)` where a guard was skipped and the model has a `Broken` flag set; under a direct edit with no flag set, the row drops the label (`story`). A row says `(the fix)` where the fix acted.

Write the story out in the code's words before deciding anything: what the installer wrote, where it stopped, what the disk and the cache file did, and what each plugin step read.

A test that fails inside `assert_properties` (`an_intent_keeps_one_owner`) reports only the first property it finds broken (stateright 0.31.0's `checker.rs:479-493`), and prints the raw path of actions instead of the rows. To see whether other properties broke too, read `checker.discovery(name)` for each property on `check(model)`, as the scratch test above does. To see the rows, call `breaks` with that model and property in a scratch test.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Confirm each step of the story against the function it names before calling it a bug. Each `Action` and `Branch` variant's doc comment names its file and function, or, for the world's steps, the boot behaviour under What the model copies. `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. Three places are the usual suspects. `actions` decides which steps are offered in each state, including the boot steps and what needs a release. `PoolOwnership::step` holds the installer's steps, the gate's refusals and the plugin's `create`. `one_usable`, `recover` and `observe` copy the plugin's and `local_storage`'s reads.
- If every step matches the code, it is a finding. Report it in plain words with the path. Keep the property and the model as they are, because weakening either to go green hides the bug.

Finding 1 has a path of 9 rows (`current_code_builds_a_root_pool_beside_an_unrecorded_device_pool`). It needs the plugin installed before the installer runs, as under Where the plugin comes from. It also needs the disk missing before anything records ownership, as under What closes the route. For PR #3842's review, scratch `ployzd` tests at the sync commit reproduced it on the real `create_device_pool` and a Volume create over the plugin socket, with a fake `zpool`. Boat lane F1 reproduced it end to end on a real Machine by route (a). The PR body lists both.

1. An earlier `ployzd install` left a release, its plugin and ZFS, and no Pool.
2. `ployzd install --storage zfs:<device>` takes the gate, its checks pass on an empty disk (`installer/storage.rs:44-63`), and `zpool create ployz-device` labels and imports the disk (`:96-106`).
3. The installer stops before `remember_device_pool` (`:113`) and releases the gate. It crashed or was killed, or `existing_device_pool`'s backing check refused with `ployz-device` still imported (`:214`). In lane F1's run 2, a kill right after `zpool create` printed nothing, and the installer exited 137.
4. The host boots without the disk. The cache file still lists `ployz-device`.
5. A Volume create takes the gate and the lock (`pool.rs:102-103`). `one_usable` finds nothing imported (`:220-225`), so `admit` calls `recover` (`:258`). With no backing file, `recover`'s `zpool import` scan lists no `ployz-device` (`:286-300`), and `has_device_pool` finds no marker (`:307`, `:368-373`). It returns `Ok(None)` (`:313`).
6. `create` passes its marker-only refusal (`:391-395`), creates the backing file (`:400-411`), runs `zpool create ployz` on it (`:421-443`), and its `one_usable` returns `ployz` (`:444-453`). The Volume is created on it (`:112-119`), on the host root filesystem. That `zpool create` rewrites the cache file without `ployz-device`.

The installer has a third way to stop there that the model has no step for. `existing_device_pool` can return `Ok(false)`, when `ployz-device` is neither listed as usable nor importable (`installer/storage.rs:188-204`), and `create_device_pool` refuses with "the device Machine Pool was not imported after creation" (`:107-112`). That leaves the label and no marker with `ployz-device` not imported. One of the scratch repros took this refusal into the plugin's root create.

What a user sees on this path:

- First, the device install fails. Killed, it prints nothing and exits 137 (lane F1). Otherwise it crashes, or it prints its refusal, such as "Machine Pool ployz-device is not backed solely by <device>" (`installer/storage.rs:236-239`), or "the device Machine Pool was not imported after creation" (`:110`) for the refusal the model has no step for.
- While the disk is missing and before the create, `local_storage` reads Ready (`machine/mod.rs:708`). `ployz server show` prints it as "Managed volumes available" (`format_storage`, `core/crates/ployz/src/handlers/server/show.rs:188`), and so does Cloud (`volumeSupportText`, `dashboard/src/modules/machines/server-status.ts:8`). Placement treats Ready as eligible (`core/crates/ployz-core/src/service/eligibility.rs:126-129`).
- The Volume create succeeds on `ployz`. `local_storage` then reads Pool (`machine/mod.rs:703-707`): `server show` prints "Managed volumes available" with the root-backed Pool's size (`show.rs:189-197`), and Cloud prints "Managed volumes available" (`server-status.ts:8`).
- When the disk is attached again, `ployz-device` is not imported: the cache file stopped listing it at the root create, and an attach imports nothing. `current_code_keeps_the_root_pool_once_the_disk_returns` prints that path, with `cached=false` at the attach. With `ployz` imported, `admit` returns it from `one_usable` and never reaches `recover` (`pool.rs:255-260`). Volumes stay on the root-backed Pool, and the disk sits unused with an importable `ployz-device` label.
- A rerun of the installer with that disk refuses at `installer/storage.rs:48-53`, since the backing file exists. A rerun is out of the model's bounds.
- If an operator imports the disk by hand (`zpool import ployz-device`), `machine_pool::one_usable` refuses both Pools with "multiple usable Machine Pools (ployz-device, ployz) are ambiguous; Ployz will not choose one" (`machine_pool.rs:78-85`), and Volume create and Path both fail with it. A scratch test at the sync commit reproduced that over the plugin socket, with a fake `zpool`. An operator's import is out of the model's bounds.

Finding 2 has 7 rows (`current_code_reads_ready_while_the_recorded_device_is_missing`). The installer records the device Pool, and the host boots without the disk. `local_storage` reads only `zpool list`, so it answers Ready (`machine/mod.rs:689-708`), while `recover` refuses a root-backed Pool on that host (`pool.rs:307-312`).

- `ployz server show` and Cloud print "Managed volumes available" (`show.rs:188`, `server-status.ts:8`), and placement treats the Machine as eligible (`eligibility.rs:126-129`).
- A Deploy's Volume create there then fails with "device Machine Pool is unavailable; refusing to create a root-backed Pool" (`pool.rs:309`).
- Under the fix, `local_storage` reads `None`, including on a Machine recorded before the fix (`the_fix_needs_local_storage_to_read_the_marker` breaks without it). `ployz server show` and Cloud print "Volume support unknown" (`show.rs:186`, `server-status.ts:7`), and placement reads `Unknown(MissingStorageEvidence)` (`eligibility.rs:133-135`).

## A break that stays green

Guards overlap, so removing one can leave all three `always` properties holding. Before calling a guard dead, rerun with a neighbouring `Broken` flag set too, or with the other value of `fix`.

- Under the fix, the intent never goes away after `remember_device_pool`. So on `fixed()`, `recover_ignores_marker` alone, `create_ignores_marker` alone and `one_usable_never_confirms` each leave all three `always` properties holding, because the intent check still refuses. Removing a guard's branch can still make that branch's `COVERAGE` `sometimes` fail, since each row is registered as a `sometimes` property (`properties`). In a round-2 scratch check on `fixed()`, `recover_ignores_marker` left "recover refuses a root-backed Pool while the recorded device is missing" and "a List across the install refuses on the marker while ployz-device is imported" unreached, `one_usable_never_confirms` left "one_usable confirms a device Pool the installer stopped before recording" unreached, and `create_ignores_marker` left every row reached. Both marker flags together do break `NO_ROOT_BESIDE_DEVICE` on `fixed()`, through the Machine recorded before the fix, which has no intent. That is one reason the code-guard tests run on the code as it is.
- On the code as it is, `NO_ROOT_BESIDE_DEVICE` already breaks through finding 1, so dropping a guard there adds nothing without a filter. `recovery_needs_the_marker_refusals`, `one_usable_needs_to_confirm_an_unrecorded_device_pool`, `the_installation_gate_keeps_volume_creates_out_of_the_install` and `the_installer_needs_its_root_pool_refusal` each keep to paths where their guard matters, and the first three first assert that the code as it is breaks nothing on those paths.
- The installer's two root-backed Pool refusals overlap. With a `ployz` imported, `prepare_storage`'s backing check (`installer/storage.rs:48`) and `existing_device_pool`'s (`:172-181`) each refuse, so `installer_ignores_root` drops both.
- The marker refusals overlap too. `create`'s (`pool.rs:391`) acts only when `recover` skipped the marker, or when `remember_device_pool` lands between a request's `recover` and its `create`, which the gate keeps out. So `create_ignores_marker` alone leaves the code as it is unchanged, and `recovery_needs_the_marker_refusals` sets both.

## Keep the model in step

When a change intentionally alters how the installer records a device Pool, when it installs the plugin, how the gate admits either side, or how the plugin decides to build `ployz`, change the model in the same PR. Examples: a new check, a reordered write, a new lock, a new reader of the marker, a new `zpool` command or cachefile option, or a `local_storage` that reads more than `zpool list`.

1. Update the model function that copies the product function you changed, citing the file and function it copies, and the `reads_as` line that prints it:
   - `install_at`, `install_locked` and the `MutationGate` (`installer/mod.rs`, `mutation.rs`): `PoolOwnership::gated`, the `Assess` and `Request` arms.
   - `install_binaries`, `install_units` and `activate_units` (`installer/mod.rs`, `installer/host.rs`): the `release` bit, set in the `Record` arm and in `init_states`, and where `actions` offers `Request` and `Observe`.
   - `prepare_storage`'s named-device path and `create_device_pool` (`installer/storage.rs`): the `Assess`, `ZpoolCreate`, `Verify` and `Record` arms of `PoolOwnership::step`, and `installer_step`.
   - `existing_device_pool`: the `Assess` and `Verify` arms, and `InstallerStops` after `zpool create`.
   - `VolumeStorage::create`, `admit_mutation`, `lock_mutation` and copy's `admit` and `first_pool`: the `Request` arm, and when `actions` offers it.
   - `VolumeStorage::list` and `inspect` (`removal.rs`): the `List` and `ListRecover` arms of `PoolOwnership::step`, and when `actions` offers them.
   - `one_usable` (`pool.rs`) and `machine_pool::one_usable`: `PoolOwnership::one_usable`.
   - `recover` and `has_device_pool`: `PoolOwnership::recover`.
   - `PoolStorage::create`: the `CreateRoot` arm.
   - `local_storage` (`machine/mod.rs`): `PoolOwnership::observe`.
   - A `zpool create` or import that changes the cache file: every arm that sets or clears `cache_lists_device`.
2. If the change adds a guard, add a `Broken` flag that removes it and a `breaks` test naming the property it keeps. A guard with no failing variant is untested.
3. If the change ships the fix, model it as the code writes it and flip the finding tests as described under Run it. Do not delete them, since their printed paths are the record of what the fix keeps out.
4. If a property's meaning changes, change its text and say why in the PR body. Narrow a property only when the code is right and the property was wrong about the design.
5. If the change adds a branch, add a `COVERAGE` row naming it, or add it to `some_branches_need_a_broken_guard` if neither variant reaches it.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

These are the bounds. There is one disk and one installer attempt, which starts on that disk while it is empty. The attempt starts from a fresh host or from a host with an earlier release and no Pool; the fix also starts from a Machine recorded before it. The plugin takes at most two Volume creates, one at a time under its lock, and only once a release is installed. List and Inspect run any number of times, each as two steps under the lock, `List` and `ListRecover`, and the installer can step between them. A request refused by the gate does not count. The disk goes missing at most once, and only by a boot, when neither the installer, nor a request, nor a List is running. ZFS's boot import happens only then too. The disk can be attached again at any time. Each rescan is a List, which the model has. The model has no clock, so nothing forces a List within 30 s; see What closes the route.

The installer is another process (`ployzd install`), so it can step between any two of a List's reads, not only at an await. Each read, and why a model step placed after the installer's step lands in the same place:
- `one_usable` (the `List` step) reads `zpool list` (`pool.rs:220`), then, only when `ployz-device` is listed, the backing file (`:228`) and the marker (`:238-244`). `ployz-device` is listed only after `zpool create`, and the backing file is never the installer's. A `Record` between `zpool list` and the marker read or write leaves the same CONFIRMED marker the List would write; only the branch differs, `UsesDevice` instead of `Confirms`.
- `recover` (the `ListRecover` step) reads the backing file and the marker (`:277-278`), which matter only with a backing file, then runs the `zpool import` scan (`:286`). While the installer runs, the scan never lists `ployz-device`: the disk is blank, or `zpool create` imported it. So no installer step changes the scan's answer, and the import branch (`:300-305`) is never taken.
- After the scan, `recover` reads the marker (`:307`). If `zpool create`, the check and `Record` land between the scan and that read, the List refuses with `ployz-device` imported. A `ListRecover` placed after `Record` reaches the same state, with the same files (`a_list_across_the_install_refuses_beside_the_imported_device`). Codex's round-1e review replayed this at the base with the real List handler and a fake `zpool` whose import scan waits for the install (`scratch_list_marker_changes_during_import_scan`): List answered "device Machine Pool is unavailable". With no `Record` yet, the List returns `Ok(None)`, as a `ListRecover` placed before `Record` does.
- Under the fix, `recover` then reads the intent and the intent's disk. The installer writes the intent, then the label (`zpool create`), then the marker; `recover` reads the marker, then the intent, then the disk. A later read never sees less of the install than an earlier one. If the marker read sees CONFIRMED, the List refuses, as one placed after `Record`. If not, and the intent read sees the intent and the disk read sees the label, it refuses on the intent, as one placed after `zpool create` and before `Record` (`the_fix_refuses_a_list_across_zpool_create`). If the intent read sees the intent and the disk read sees no label, it leaves the intent, as one placed after the intent and before `zpool create`. With no intent, it returns `Ok(None)`, as one placed before the intent. None of these writes a file, so the files are the installer's in each case.

A Volume create takes the gate and the lock in one model step. Behind a List, the code's Volume create waits at the plugin's in-process lock in `admit_mutation` (`volume_plugin/storage.rs:159`) before it tries the gate (`try_mutation` in the same function), so it does not hold the gate while it waits. Behind an Inspect, which takes no in-process lock, it holds the gate while it waits for the Pool lock (`pool.rs:102-103`; `lock_mutation` blocks in `lock_exclusive`, `:190`). That only makes the installer's Busy more likely.

These are out of scope:

- a re-run of the installer after this attempt, and its named-device path's branch for an existing device Pool (`installer/storage.rs:57-58`, `:65-66`)
- `existing_device_pool`'s `Ok(false)` refusal after `zpool create` (`:107-112`), which the model has no step for, as described under Real bug or model error
- an install that crashes after `remember_device_pool` and before `activate_units` (`installer/mod.rs:290-304`), since `Record` includes the release
- the plain `--storage zfs` path and `reconcile_device_pool_marker` (`installer/storage.rs:34-38`, `:123-160`)
- a legacy marker (`"ployz-device\n"`), and a partial write from `one_usable`'s plain `fs::write` (`pool.rs:242`)
- a second disk, a disk replaced by another, and `existing_device_pool`'s device-backing checks (`installer/storage.rs:210`, `:213-214`)
- a disk that vanishes under an imported Pool, which ZFS keeps listed but unusable, so `one_usable` refuses it (`machine_pool.rs:74-76`)
- a boot during an installation or a Volume create
- an operator's own `zpool` commands, such as importing `ployz-device` by hand
- `create`'s cleanup after its `one_usable` finds a Pool other than `ployz` (`pool.rs:454-468`). Between `recover` and `create`, the gate keeps the installer out, and only a boot or an operator imports `ployz-device`, so the bounds never reach it. Only `gate_admits_during_install` lets the installer's `zpool create` land there, and `the_installation_gate_keeps_volume_creates_out_of_the_install` keeps to paths where `ployz` came first.
- a backing file left with no label (`pool.rs:314-339`), a destroyed or ambiguous label (`:320-330`, `:342-349`), and an exported root-backed Pool (`:350-354`)
- `create` failing before `zpool create ployz`, such as the host root's reserve or `fallocate` (`:396-419`)
- Volume operations other than the first Pool's creation and List's and Inspect's `admit`, such as removal, which refuses once the Machine has held a Pool that is not imported (`volume_plugin/removal.rs:24-26`). Inspect's same refusal (`:55-57`) changes no file or Pool, so the model leaves it out.
- how ZFS imports at boot on any host but Ubuntu 24.04 with ZFS 2.2.2

A green model says nothing about them. Cover the code ones with:

```
cd core && cargo nextest run -p ployzd installer::storage volume_plugin machine::tests::current_zpool_evidence mutation::tests
```

It runs 253 tests. It builds `ployzd`: from a `core/target` with no `ployzd` build, runs on a host with the `kache` compile cache took between 28 seconds and 4 minutes, depending on the cache. Across runs, `core/target` started at 0.9 to 6.7 GB and ended at 6.1 to 9.9 GB. The `volume_plugin` tests live in the `ployzd` binary's test target (nextest lists them under `ployzd::bin/ployzd`), so `cargo test -p ployzd --lib volume_plugin` misses them.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/installer/mod.rs`: `install_at` and `install_locked`.
- `core/crates/ployzd/src/installer/host.rs`: `activate_units`.
- `core/crates/ployzd/src/main.rs`: `ployzd install`'s `--storage` default.
- `core/crates/ployzd/src/docker/observe.rs`: `RESCAN_INTERVAL` and `ContainerRuntime::replicating`: the rescan's period, which the model has no clock for.
- `core/crates/ployzd/src/volume_plugin/removal.rs`: `list` and `inspect`, which run `admit` without the installation gate.
- `core/crates/ployzd/src/mutation.rs`: `MutationGate::try_installation` and `try_mutation`.
- `core/crates/ployzd/src/installer/storage.rs`: `prepare_storage`'s named-device path, `create_device_pool`, `existing_device_pool` and `remember_device_pool`.
- `core/crates/ployzd/src/volume_plugin/pool.rs`: `VolumeStorage::create`, `lock_mutation`, `one_usable`, `admit`, `recover`'s no-backing branch, `has_device_pool` and `create`.
- `core/crates/ployzd/src/volume_plugin/storage.rs`: `admit_mutation`.
- `core/crates/ployzd/src/volume_plugin/copy.rs`: `admit` and `first_pool`.
- `core/crates/ployzd/src/machine_pool.rs`: `one_usable`, `importable_names` and `CONFIRMED_DEVICE_POOL_MARKER`.
- `core/crates/ployz-core/src/machine_layout.rs`: `machine_pool_file` and `device_pool_marker`.
- `core/crates/ployzd/src/machine/mod.rs`: `local_storage`.

Sync commit: `538657f812e6aff0d7b7b4f10f24b6fa6e76653e`, the code commit of the change that moved these lines onto `MachineLayout`, rebased on `main`, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
