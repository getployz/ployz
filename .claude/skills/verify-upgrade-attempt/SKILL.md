---
name: verify-upgrade-attempt
description: Model-check one Machine upgrade attempt (request_locked's Accepted receipt, the .upgrade-active marker and the worker launch, the worker's work, and the settling readers settle, reconcile_locked and reconcile_for_install) and what MutationGate::try_mutation answers around it to daemon RPCs and the volume plugin, against the daemon, the worker and ployzd install being killed between two steps, ployzd install or a late worker holding the lock while daemon start settles, and clear_active returning Io before or after it removes the marker, using the Stateright model in core/crates/ployz-verify. Use when a change touches core/crates/ployzd/src/installer/upgrade.rs (request, request_locked, replay, latest, settle, work, reconcile, reconcile_for_install, reconcile_locked, launch_worker, worker_state), core/crates/ployzd/src/mutation.rs (MutationGate, try_mutation, try_installation, lock_installation, active, mark_active, clear_active), core/crates/ployzd/src/machine/local_machine.rs (inspect, admit_installation, finish_mutation, finish_installed, update's fast path, depart), core/crates/ployzd/src/machine/local_machine/upgrade.rs (request_upgrade), core/crates/ployzd/src/daemon.rs (the start-up reconcile), core/crates/ployzd/src/installer/mod.rs (install_at), core/crates/ployzd/src/installer/host.rs (the daemon and volume plugin units), core/crates/ployzd/src/volume_plugin/storage.rs (admit_mutation), or MachineUpgradeOutcome::is_terminal (which MachineUpgradeAttempt::is_terminal delegates to) in core/crates/ployz-core/src/rpc/upgrade.rs.
---

# Verify the Machine upgrade attempt

The model in `core/crates/ployz-verify/src/upgrade_attempt.rs` explores every interleaving of one upgrade attempt on one Machine. Four processes take part: the daemon, the worker unit, `ployzd install` and the volume plugin.

Around the attempt run settling readers: one the daemon hosts at a time, with `ployzd install` beside it. Any number of `try_mutation` calls run too, from daemon RPCs and from the volume plugin. The daemon, the worker and `ployzd install` can each be killed between any two of their steps. The lock holder's `clear_active` can return Io before it removes the marker or after.

The model has two start states: a Machine with no receipt and no marker, and one with `.upgrade-active` and no receipt. The fix variant changes what `try_mutation` reads. It also adds a release from before the fix that still reads the marker alone.

Paths below are under `core/crates/ployzd/src/` unless they say otherwise, and a bare `:n` is in the file last named.

## What the model copies

**The processes.**
- **The daemon** runs `request_locked` in a task that holds the lock (`installer/upgrade.rs:156-158`). It also hosts every RPC: Inspect, a resend, Leave, a new upgrade request, and the mutations that admit through `admit_installation`.
  - One `DaemonDies` ends the task and every reader the daemon hosts, and releases their locks.
  - systemd starts it again (`Restart=always`, `RestartSec=2`, `installer/host.rs:296-298`).
  - Daemon start runs `reconcile` before it serves anything (`daemon.rs:184-186`). So after a kill, daemon start is the only daemon caller the model offers (`Daemon::Down`, then `Daemon::Starting`).
  - `reconcile` drops what `settle` returns (`installer/upgrade.rs:349-351`), so a daemon start whose `settle` lost `try_installation` serves anyway (`daemon.rs:187`).
  - A daemon start whose `clear_active` returns Io fails, and the daemon exits again (`daemon.rs:184-186`).
- **The worker** is its own transient unit (`installer/upgrade.rs:397-459`).
- **`ployzd install`** is its own process, with its own reader slot (`State::install`). It runs beside the daemon's reader, including daemon start.
- **The volume plugin** is its own unit (`installer/host.rs:252-273`, `:348-371`). Its `admit_mutation` runs `try_mutation` (`volume_plugin/storage.rs:115-124`) whether or not the daemon is up.

The lock is an `flock` on an open file, so a kill releases it (`mutation.rs:37`).

**The request.** `request_upgrade` replays the attempt's id, then takes the installation lock with `try_installation` (`machine/local_machine/upgrade.rs:27-38`). Under the lock, `request_locked` (`installer/upgrade.rs:170-239`) does the following:
- reads the receipt `upgrade-attempt.json` (`:177`);
- runs `reconcile_locked` on it (`:185`);
- refuses a live receipt (`:186-187`) and an existing marker (`:191-192`);
- writes Accepted (`:227`), then `.upgrade-active` with `mark_active` (`:228`, `mutation.rs:107-111`), then launches the worker (`:230`);
- if the launch fails, writes Failed{Launching} (`:231-235`), then clears the marker (`:236`).

Only `:228` writes the marker, and it follows the Accepted write. A marker with no receipt therefore comes from outside the attempt, as ployzd's `machine_api/local_tests.rs:188` and `build/tests.rs:451-453` write it. The model starts from that state too.

**The worker.** `work` (`installer/upgrade.rs:299-346`) does the following:
- waits for the lock with `lock_installation` (`:306`) and reads the receipt (`:307`);
- returns `NotActive` for a receipt that is not its own or is already terminal (`:308-310`). Its `guard` holds the lock from `:306` until that return. The model takes these as two steps, `WorkerLocks` and `WorkerRefuses`;
- otherwise writes Running (`:316`), installs, writes the terminal receipt (`:336-343`) and clears the marker (`:344`).

The unit has `RuntimeMaxSec=15min` (`:21`, `:414`).

**`clear_active`** (`mutation.rs:113-123`) has three outcomes:
- **The marker names the attempt.** It removes it (`:116`), then syncs the directory (`:121`).
- **No marker.** It returns Ok and removes nothing (`:118`). The story prints that as "finds no .upgrade-active and removes nothing".
- **Io.** An Io from reading or removing the marker (`:116`, `:119`) leaves the marker: `ClearFails { removed: false }`. An Io from the sync (`:121`) comes after the removal: `ClearFails { removed: true }`.

The caller returns the Io with `?`.

**Admission.** `try_mutation` (`mutation.rs:55-69`) takes the lock shared (`:57`), then reads the marker (`:58-62`). It answers Busy, "a Ployz installation or upgrade is active" (`:26`), when another process holds the lock or the marker exists. Every receipt and marker write holds the lock exclusively, so neither changes while `try_mutation` holds it shared.

The model has three askers:
- **`Asker::Daemon`**, for a daemon RPC. It runs only while the daemon serves.
  - `admit_installation` (`machine/local_machine.rs:331-334`), and through it:
    - `finish_mutation` (`:337-344`, `:355`), for enroll, register, update, forget and Leave;
    - `finish_installed` (`:359-367`), for a Deploy's container step (`EnsureContainer`, `machine_api/local.rs:469`, `machine/local_machine/container.rs:1081`, `:1108`, `:1143`) and volume copy cuts (`:581`);
    - image ingest (`machine/local_machine/container.rs:690`);
    - a Build's admission (`build.rs:196-201`).
  - The Update fast path (`machine/local_machine.rs:846-856`, `try_mutation` at `:851`). It takes every update that leaves the name, the public IP and the advertised endpoints unchanged (`:846-848`). That includes labels and the policy fields: `accepts_builds`, `accepts_services`, `accepts_ingress` and build concurrency (`core/crates/ployz-core/src/domain/machine.rs:523-541`). Other updates go through `finish_mutation`.
- **`Asker::Plugin`**, for the volume plugin, at any time. Its `admit_mutation` gates:
  - create (`volume_plugin/pool.rs:102`) and mount (`volume_plugin/storage.rs:257`);
  - remove (`volume_plugin/removal.rs:21`) and resize (`volume_plugin/size.rs:28`);
  - seed (`volume_plugin/seed.rs:25`), copy (`volume_plugin/copy.rs:145`) and departure (`volume_plugin/departure.rs:62`);
  - the capacity reads (`volume_plugin/capacity.rs:122`, `:137`).
- **`Asker::Older`**, under the fix only. It stands for a release before the fix.

**The settling readers.** `settle` (`installer/upgrade.rs:251-276`) repairs what a stopped writer left:
- **A terminal receipt:** `try_installation` (`:261`), then `clear_active` (`:259`). On Busy it returns the receipt as Ok and clears nothing (`:260`, `:263`).
- **A receipt that is not terminal:** `worker_state` (`:265`). A running unit returns the receipt as it stands (`:265-266`). Otherwise `try_installation` (`:268`), which returns on Busy (`:270`), then a re-read under the lock (`:273`) and `reconcile_locked` (`:274`).

`worker_state` reads `systemctl show`, and a unit that is not found reads Stopped (`:467-518`).

`reconcile_locked` (`:372-395`) clears the marker of a terminal receipt (`:377-378`). It returns while the unit runs (`:381-382`). Otherwise it writes Interrupted (`:384-392`), then clears the marker (`:393`).

| Caller | Process | Route | When it runs |
|---|---|---|---|
| Inspect | daemon | `machine/local_machine.rs:463`, `:512`, through `latest` (`installer/upgrade.rs:109-117`). A `settle` error is logged as "could not read the latest upgrade attempt", and Inspect answers without the attempt (`machine/local_machine.rs:512-520`). | Whenever a client sends Inspect (`machine_api/local.rs:294`). The daemon has no timer for it. Senders include `ployz server ls` (lane 3, below); a Deploy's storage reads (`core/crates/ployz/src/deploy/pipeline.rs:437`, `observe_machine_storage` in `core/crates/ployz/src/cluster.rs`); the SDK's runtime watch (`core/crates/ployz/src/sdk.rs:1491`); a GitHub build (`core/crates/ployz/src/sdk/github_build.rs:303`); and Cloud's enrollment (`dashboard/src/modules/machines/enrollment.server.ts:822`), forget (`dashboard/src/modules/machines/forget-servers.server.ts:92`) and pairing removal (`dashboard/src/modules/machines/pairing-removal.server.ts:384`). |
| A resend of the same attempt | daemon | `machine/local_machine/upgrade.rs:27-31`, through `replay` (`installer/upgrade.rs:90-105`), which returns `settle`'s error | The CLI polls every second (`POLL_INTERVAL` and `poll_upgrade` in `core/crates/ployz/src/handlers/server/upgrade.rs`) and stops at the first terminal reply (`:710-711`) or error. Cloud resends while it watches the attempt (`dashboard/src/modules/server-upgrade/server-upgrade.server.ts:288-292`). |
| Leave | daemon | `depart`, `machine/local_machine.rs:938-961`, through `latest` (`:945`). A `settle` error fails Leave (`:956`). | When a user leaves the Machine. `depart` refuses a live attempt (`:946-953`) and otherwise runs `finish_mutation` (`:959`). |
| Daemon start | daemon | `daemon.rs:184-186`, through `reconcile` (`installer/upgrade.rs:349-351`) and `latest` | Once per start, before the daemon serves |
| A new upgrade request | daemon | `replay` returns None for another id (`installer/upgrade.rs:98-99`); `try_installation` (`machine/local_machine/upgrade.rs:38`); `request_locked`'s `reconcile_locked` (`:185`) | On the next upgrade |
| `ployzd install` | its own | `install_at`: `try_installation` (`installer/mod.rs:254`), then `reconcile_for_install` (`installer/upgrade.rs:353-370`, from `installer/mod.rs:255-257`), holding the lock through `install_locked` (`:255`) | When an operator runs it |

**The finding.** Every terminal write comes before its `clear_active`:
- the request's Failed{Launching} at `:235`, before `:236`;
- the worker's terminal write at `:343`, before `:344`;
- `reconcile_locked`'s Interrupted at `:392`, before `:393`.

Each window can open in two ways: the writer's process is killed between the two, or `clear_active` returns Io before it removes the marker (`mutation.rs:116`, `:119`). An Io at the sync (`:121`) opens no window, because the marker is already gone. Either way the lock is released while the marker still names a finished attempt. `try_mutation` then answers Busy on the marker alone, until a settling reader wins `try_installation` and clears it.

The property that breaks is `BUSY_ONLY_WHILE_LIVE`: "try_mutation answers Busy only while a process holds the installation lock or the receipt is not terminal". It is checked for the current release's askers, Daemon and Plugin. A missing receipt counts as not terminal, so a bare marker's Busy does not break it.

**Which callers see which route.** Lane 3 is the live run in PR #3845's Live lanes section: one Boat Machine at the sync commit, with a real upgrade.

| Route | Who sees Busy | Until |
|---|---|---|
| The daemon killed between `installer/upgrade.rs:235` and `:236`, or while a reader it hosts is between a write and its `clear_active` | The volume plugin while the daemon is down (`current_code_leaves_a_finished_upgrade_refusing_mutations`). Daemon RPCs too, once daemon start's `settle` loses `try_installation` and the daemon serves. Two processes can hold the lock then: `ployzd install` (`daemon_start_loses_the_lock_to_ployzd_install`) and a late worker that holds it until it returns NotActive (`daemon_start_loses_the_lock_to_a_late_worker`). `DAEMON_START_SETTLES_FIRST` says that is the only way: "a daemon RPC answers Busy on the marker a killed daemon left only after daemon start's settle lost try_installation". | Daemon start's `settle` when it wins the lock. After lane 3's `systemctl kill -s KILL ployz`, the marker was gone about 2.1 s after the kill. After a start that lost the lock, as the next row. A daemon start whose `clear_active` returns Io once exits (`daemon.rs:184-186`), and systemd starts it again (`RestartSec=2`, `installer/host.rs:296-298`). |
| The worker killed between `:343` and `:344`, or a one-off `clear_active` Io before removal while the daemon serves | Every daemon RPC and the volume plugin (`the_worker_leaves_a_finished_upgrade_refusing_mutations`, `the_poller_can_stop_before_the_worker_clears`) | The first settling reader that wins `try_installation`: an Inspect from any sender above, a resend, Leave, a new upgrade, a daemon restart, or `ployzd install`. A reader that gets Busy returns the terminal receipt and leaves the marker to the next (`installer/upgrade.rs:257-263`). With the marker removable again, lane 3 saw an Inspect clear it (`ployz server ls` at 18:39:55, the marker gone at 18:39:56.138), a new upgrade go on, and a `systemctl restart ployz` clear it 77 ms after the new start. |
| `ployzd install` killed between `:392` and `:393`, or a one-off Io from its `clear_active` | The plugin (`an_interrupting_reader_leaves_the_same_window`). Its story runs install while the daemon is down, so daemon start then clears the marker. Daemon RPCs see it only if the daemon served when install stopped, or daemon start lost `try_installation` to install. | As above |
| A persistent Io: every `clear_active` on the marker fails, as when it cannot be removed. Lane 3 made it immutable with `chattr +i`, and the worker's clear at `:344` returned EPERM. | Every daemon RPC and the volume plugin, as on the routes above | Until the cause is removed. No reader can clear the marker: each settling caller's own `clear_active` returns the Io. A daemon restart does not end it, because daemon start's `reconcile` returns the Io and the daemon exits before it serves (`daemon.rs:184-186`). The model takes each `ClearFails` as one step, and it has no persistent Io. |

Each route needs a kill, or a filesystem error right after a successful write, inside a window of one statement. A reboot kills the worker and the daemon together, and daemon start then settles, unless another process holds the lock. Two kinds of route leave the daemon serving beside the marker, so daemon RPCs see them until a settling reader wins the lock or the daemon restarts:
- the worker-kill and one-off Io routes;
- a daemon start that lost the lock.

The model has no clock, so it says nothing about how long a window lasts. With the marker removable again, lane 3 saw no clearing in 60 s with no client, then an Inspect from `ployz server ls` clear it. Lane 3 also saw Inspect sent by a Cloud Servers-page load and a Cloud deploy, in the daemon's "could not read the latest upgrade attempt" warnings. A worker that dies mid-install has the same symptom and is likelier. It is outside the finding, and the fix keeps it (The fix).

A daemon start that loses the lock needs a second rare event after the daemon's kill:
- **`ployzd install`** must hold the lock when daemon start's `settle` tries it, and must then stop before its own clear: killed, or its `clear_active` returning Io. An install that reaches its clear removes the marker itself (`reconcile_for_install`, then `reconcile_locked`'s `installer/upgrade.rs:377-378`).
- **A late worker** needs `launch_worker` to time out after systemd started the unit (the 20 s timeout, `installer/upgrade.rs:22`, `:440-447`), so that Failed{Launching} is written, and the daemon killed between `:235` and `:236`. The worker must then hold the lock when daemon start's `settle` tries it. A worker already waiting in `lock_installation` takes the lock when the daemon dies, and returns NotActive after one receipt read (`:306-310`). Daemon start comes `RestartSec=2` later (`installer/host.rs:296-298`); lane 3 saw systemd schedule the restart 2.03 s after a SIGKILL. So the worker must reach `lock_installation` about then, not before.

**What a user sees while the daemon serves beside the marker.** Each item below answers "a Ployz installation or upgrade is active", unless it says otherwise.
- **`ployz server set` on this Machine.**
  - The CLI sends UpdateMachine (`core/crates/ployz/src/handlers/server.rs:1017-1024`). A label or policy edit reaches the fast path's `try_mutation` (`machine/local_machine.rs:851`), and other edits reach `finish_mutation`.
  - The daemon maps Busy to Conflict (`machine_api/local.rs:1069-1074`).
  - The CLI returns the `RpcError` (`core/crates/ployz/src/handlers/server.rs:1057`) as `Failure::command` (`core/crates/ployz/src/failure.rs`, its `Display` impl prints the message, and `report` builds the `--json` error).
  - `ployz server set --accepts-ingress=true` sends Inspect first (`ingress_off`, `core/crates/ployz/src/handlers/server.rs:1178-1190`), and that clears the marker before the update.
- **Enroll, register, forget and Leave.** Leave runs its own `settle` first, which clears the marker unless its `try_installation` loses to another lock holder.
- **Builds and Deploys.** A Build on the Machine fails at its Admission stage (`build.rs:196-201`). A Deploy to this Machine fails its container step (`machine/local_machine/container.rs:1108`, `:1143`). Image ingest (`:690`) and volume copy cuts (`:581`) fail too.
- **The volume plugin.** Every plugin path listed under Admission fails, such as `docker volume create`.
- **A Cloud policy change** gets Conflict, and the function retries 3 times (`dashboard/src/modules/machines/server-policy.inngest.ts:50`). Any settling reader between two attempts ends the window, and the next attempt applies. If every attempt gets Conflict, the Servers page reads the policy back unchanged from Runtime observation (`:20-23`), and shows no error.
- **Not refused, and no error shown:**
  - An Inspect-backed command such as `ployz server ls` answers normally, and clears the marker unless another process holds the lock.
  - Cloud's enrollment, forget and pairing-removal flows send Inspect to their session's Machine first, which clears the marker when that Machine is this one.
  - A new upgrade from the CLI or Cloud clears it in its `reconcile_locked` (`installer/upgrade.rs:185`, `:377-378`) and goes on.

**What only a persistent Io adds.** Lane 3 saw the poll, Inspect and Cloud items while the marker was immutable (18:31:39 to 18:38:54 in its log):
- **The CLI's upgrade poll reports a failure for an upgrade that succeeded.** The poll is a resend, and its `replay` reaches `settle` (`installer/upgrade.rs:90-105`). `settle` wins `try_installation`, and `clear_active` returns the Io (`:261-262`), which comes back through `request_upgrade` (`machine/local_machine/upgrade.rs:27-31`). The CLI prints "error: Machine mutation admission failed" (`mutation.rs:29`) with its cause, and exits 3. That is `PARTIAL_EXIT` (`core/crates/ployz/src/ui/exit.rs:11-12`), which a run gets while a Server's result is unknown (`core/crates/ployz/src/handlers/server/upgrade.rs:543-547`). It needs the poll's own `clear_active` (`installer/upgrade.rs:259`) to fail too. After a one-off Io at `:344`, the poll's next resend clears the marker and gets the terminal receipt.
- **Inspect answers without the attempt.** `ployz server ls` exits 0 with no error, and the daemon logs "could not read the latest upgrade attempt" for each Inspect (`machine/local_machine.rs:512-520`). The marker stays.
- **Leave fails** with the Io (`machine/local_machine.rs:945`, `:956`). Lane 3 did not run Leave.
- **A new upgrade fails** with the Io: `request_locked` runs `reconcile_locked` on the finished receipt (`installer/upgrade.rs:185`), whose `clear_active` returns it (`:378`). Lane 3 ran its new upgrade after it removed the pin.
- **A daemon restart never serves**, as in the route table. Lane 3's restart and kill ran after it removed the pin, so their timings are one-off numbers.
- **A Cloud policy change is lost.** Lane 3's toggle got Conflict on the first attempt and all 3 retries, and the page read the policy back unchanged with no error. A control click after the window closed took effect.

**The characterization tests behind each guard.**

| Guard | Flag, and the model test that sets it | Property that breaks without it | Pinned in ployzd by |
|---|---|---|---|
| `try_mutation`'s shared lock (`mutation.rs:57`) | `mutation_ignores_lock`: `the_lock_keeps_mutations_out_of_an_installation` | `NO_MUTATION_UNDER_THE_LOCK` | `installation_and_active_handoff_exclude_mutations_across_instances` (`mutation.rs:167`) |
| `try_mutation`'s marker read (`:58-62`) | `mutation_ignores_marker`: `the_marker_keeps_mutations_out_of_the_handoff` | `NO_MUTATION_BESIDE_A_LIVE_WORKER` | the same test; `retry-active`'s Busy after the request returned (`installer/upgrade/tests.rs:251-254`); `every_existing_marker_type_fails_closed` (`mutation.rs:185`); `marker_metadata_errors_are_not_absence` (`:195`) |
| `mark_active` before `launch_worker` (`installer/upgrade.rs:228`, `:230`) | `launches_before_marking`: `the_marker_is_written_before_the_launch` | `NO_MUTATION_BESIDE_A_LIVE_WORKER` | `cancelled-launch` asserts `active()` while the launch runs (`installer/upgrade/tests.rs:201`) |
| the worker's terminal check (`installer/upgrade.rs:308`) | `worker_ignores_terminal`: `the_worker_refuses_a_finished_attempt` | `RECEIPT_FINAL` | none |
| `settle`'s re-read under the lock (`installer/upgrade.rs:273`) | `settle_reuses_snapshot`: `settle_rereads_the_receipt_under_the_lock` | `RECEIPT_FINAL` | none |
| `settle`'s `worker_state` (`installer/upgrade.rs:265`) | `settle_skips_worker_state`: `settle_relies_on_the_worker_check_under_the_lock` | no `always` property beyond the finding's; only its own `COVERAGE` row goes unreached | `retry-active`'s replay returning Accepted unchanged (`installer/upgrade/tests.rs:247-250`), together with `installer/upgrade.rs:381` |
| `reconcile_locked`'s `worker_state` (`installer/upgrade.rs:381`) | `reconcile_skips_worker_state`: `reconcile_locked_needs_its_worker_check`, and the second half of `settle_relies_on_the_worker_check_under_the_lock` | `INTERRUPTED_ONLY_STOPPED` | `retry-active`'s other-attempt request answering Busy (`installer/upgrade/tests.rs:277-292`); `interrupted` (`:312-328`) for the stopped branch |
| daemon start's `reconcile` before serving (`daemon.rs:184-186`) | `serves_before_reconcile`: `daemon_start_settles_before_serving` | `DAEMON_START_SETTLES_FIRST` | none |

`launch-failed` (`installer/upgrade/tests.rs:341-351`) pins the clear after Failed{Launching}, and `upgrade_worker_contract` (`:592-596`) pins the clear after work. Neither stops a process between the write and the clear, so neither sees the finding.

Codex's reviews of PR #3845 reproduced these on the real code at the sync commit, in scratch `ployzd` tests that are not in the tree:
- **The end state.** A terminal receipt, the naming marker and a free lock answer Busy. Inspect, `reconcile_locked`, `reconcile_for_install` and daemon start each clear the marker, after which mutation is admitted.
- **The poll.** The poller's `replay` returns the terminal reply under the worker's lock.
- **The Io split.** An Io before removal keeps the marker, and one after removal admits.
- **Daemon start losing the lock.** Daemon start's `reconcile` returns Ok while `ployzd install` holds the lock. Install then stops before its clear, and a daemon RPC answers Busy.

## Run it

```
cd core && cargo nextest run -p ployz-verify upgrade_attempt --no-capture
```

The code as it is has 10529 states and the fix 13088. `current_code_breaks_only_the_finding` and `the_fix_answers_from_the_receipt` print them. Each story prints under its test's name. CI runs `ployz-verify` in its workspace nextest run.

The run is green when all 24 tests pass. Read them as five groups.

- **The finding.** Six tests find a stale marker refusing on the code as it is:
  - `current_code_leaves_a_finished_upgrade_refusing_mutations` is the shortest path that breaks `BUSY_ONLY_WHILE_LIVE`. It pins its six steps: the daemon killed between `:235` and `:236`, then the volume plugin answering Busy while the daemon is down.
  - `the_worker_leaves_a_finished_upgrade_refusing_mutations` keeps to paths where the worker wrote its terminal receipt and a daemon RPC answers Busy.
  - `an_interrupting_reader_leaves_the_same_window` keeps to paths where `reconcile_locked` wrote Interrupted. Its first path is `ployzd install`, run while the daemon is down, whose `clear_active` after Interrupted returns Io.
  - `the_poller_can_stop_before_the_worker_clears` replays a fixed schedule (`follow`) rather than filtering paths. The checker keeps only the first path to each state, and the last state is the same with or without the poll.
  - `daemon_start_loses_the_lock_to_ployzd_install` and `daemon_start_loses_the_lock_to_a_late_worker` keep to paths where daemon start's `settle` found that process holding the lock (`start_lost_to`). A daemon RPC then answers Busy on the marker the killed daemon left.
- **The window and its rows.**
  - `a_settling_reader_ends_the_window` is the first path where Leave's own `settle` removes the marker a stopped writer left (`State::cleared`), and Leave is then admitted.
  - `each_clearing_row_is_its_callers` checks that each clearing row's witness names the caller in the row's name.
  - `current_code_breaks_only_the_finding` expects `BUSY_ONLY_WHILE_LIVE` to break and every other `always` property to hold. It also expects every `COVERAGE` row that `on` keeps for the code as it is to be reached.
- **The code's guards.** These eight tests set one `Broken` flag each on `as_is_without(..)`. Every test but `settle_relies_on_the_worker_check_under_the_lock` expects the property in the table above to break.
  - `settle_relies_on_the_worker_check_under_the_lock` runs `settle_skips_worker_state`. It expects `INTERRUPTED_ONLY_STOPPED` and `RECEIPT_FINAL` to hold, and only "settle returns a running worker's receipt as it stands" to go unreached among the rows that `on` keeps for the code as it is. Then it runs `reconcile_skips_worker_state` and expects a `settle` caller to break `INTERRUPTED_ONLY_STOPPED` through its own check: `worker_state` read the unit stopped, and the unit started before `try_installation` (`:265-268`).
  - The guard table names the other seven beside their flags.
- **The fix.**
  - `the_fix_answers_from_the_receipt` runs `proposed()`: every property holds, and every row that `on` keeps for the fix is reached.
  - `the_fix_refuses_a_marker_with_no_receipt` prints the bare marker refusing under the fix.
  - `the_fix_leaves_older_readers_their_window` and `the_fix_keeps_the_dead_attempt_window` print the fix's two limits.
- **The fix's parts.** `the_fix_needs_the_receipt`, `the_fix_reads_the_receipt_only_beside_the_marker` and `the_fix_still_writes_the_marker` each drop one part on `proposed_without(..)`, under The fix.

Every clearing `COVERAGE` row requires `State::cleared` to name its caller. `cleared` is set only on the step where that caller's `clear_active` removed a marker that a stopped writer left (`stranded`). A `clear_active` on an absent marker sets `Branch::ClearsNothing` and no `cleared`.

### Breaking a guard yourself

Set its `Broken` flag through `as_is_without`, or its `Fix` field through `proposed_without`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every red test is listed.

An edit to the model's code changes every model at once. For PR #3845, each edit below was made alone in `core/crates/ployz-verify/src/upgrade_attempt.rs`, run with `--no-fail-fast`, and reverted.
- **A flag made a no-op** means every read of it outside `mod tests` was replaced with `false`. `launches_before_marking` has two reads, in `actions` and in the `Launch` arm of `step`. `settle_reuses_snapshot` has two, in `reader_step` and in `reader_step_reads_as`.
- **A caller never offered** means a `Caller::Inspect => false,` arm (for that caller) put first in the `.filter(|caller| match caller {` of `actions`.
- **`settle_relies_..` goes red with every edit that leaves another non-Fix row unreached.** It pins the exact list of unreached rows under its flag. That is a side effect, not a broken settle guard.

| Edit | Tests that went red |
|---|---|
| R1. In `admission`, `let marker = state.marker && !broken.mutation_ignores_marker;` replaced with `let marker = false;` | 16: `current_code_leaves_..`, `the_worker_leaves_..`, `an_interrupting_reader_..`, `the_poller_..`, both `daemon_start_loses_..`, `current_code_breaks_only_the_finding`, `daemon_start_settles_before_serving`, `settle_relies_..`, `the_lock_keeps_mutations_out_of_an_installation`, `the_marker_is_written_before_the_launch`, and `the_fix_answers_..`, `the_fix_keeps_the_dead_attempt_window`, `the_fix_leaves_older_..`, `the_fix_needs_the_receipt`, `the_fix_refuses_a_marker_with_no_receipt` |
| R2. In `reader_step`'s `At::Clear` arm, `state.cleared = Some(reader.caller);` deleted | `a_settling_reader_ends_the_window`, `current_code_breaks_only_the_finding`, `each_clearing_row_is_its_callers`, `settle_relies_..`, `the_fix_answers_from_the_receipt` |
| R3. Inspect, Replay, Leave or NewRequest never offered | `current_code_breaks_only_the_finding`, `each_clearing_row_is_its_callers`, `settle_relies_..` and `the_fix_answers_from_the_receipt`, at that caller's clearing row. Also `the_poller_..` for Replay, and `a_settling_reader_..` for Leave |
| R4. DaemonStart never offered | as R3, and both `daemon_start_loses_..` |
| R4. Install never offered | as R3, and `daemon_start_loses_the_lock_to_ployzd_install` |
| R5. In `reader_step`'s `At::Clear` arm, `let repaired = false;` | `a_settling_reader_ends_the_window`, `current_code_breaks_only_the_finding`, `settle_relies_..`, `the_fix_answers_from_the_receipt` |
| R6. In `actions`, `Mutate(Asker::Daemon)` offered while the daemon is down (`if serving` made `if true`) | `current_code_breaks_only_the_finding` (`DAEMON_START_SETTLES_FIRST`), and `current_code_leaves_..`, whose pinned path now ends at a daemon RPC |
| R7. In `reader_step`'s `At::TryNonTerminal` arm, `if state.lock_free()` made `if state.lock_free() && !state.worker.active()`, so `settle` checks the worker again once it would hold the lock | `settle_relies_..` |
| R8. In `reconcile_locked`, the `Some(_) if state.worker.active() && ..` arm's guard made `if false` | `settle_relies_..`, `current_code_breaks_only_the_finding`, `the_fix_answers_..`, and, because their pinned paths moved, `current_code_leaves_..`, `daemon_start_loses_the_lock_to_a_late_worker` and `the_worker_refuses_a_finished_attempt` |
| R9. In `admission`, the fix's `marker && !state.receipt_terminal()` replaced with `marker`, dropping the receipt read alone | `the_fix_answers_from_the_receipt` (`BUSY_ONLY_WHILE_LIVE`) |
| R10. In `step`'s `Mark` arm, `if !self.proposal().writes_no_marker` made `if self.fix.is_shipped()`, so the fix writes no marker | `the_fix_answers_..`, `the_fix_keeps_the_dead_attempt_window`, `the_fix_leaves_older_..`, `the_fix_needs_the_receipt` |
| R11. Both reads of `launches_before_marking` made `true`, so every model launches first | `current_code_breaks_only_the_finding`, `current_code_leaves_..`, `daemon_start_loses_the_lock_to_a_late_worker`, `settle_relies_..`, `the_fix_answers_..`, `the_poller_..` (its `Mark` is no longer offered) |
| R12. The fix's `marker && !state.receipt_terminal()` replaced with round 1's `marker && state.receipt_live()` | `the_fix_answers_from_the_receipt`, `the_fix_refuses_a_marker_with_no_receipt` |
| R13. In `settle_busy`, `state.start_lost = true;` deleted | `current_code_breaks_only_the_finding` (`DAEMON_START_SETTLES_FIRST`), both `daemon_start_loses_..`, `settle_relies_..` |
| R14. The one-reader bound restored: in `actions`, `&& state.install.is_none()` added to the `Caller::DaemonStart` arm of the filter, and `&& state.reader.is_none()` to the `Caller::Install` arm | `daemon_start_loses_the_lock_to_ployzd_install` |
| R15. The worker's NotActive refusal made one step: in `step`'s `WorkerLocks` arm, `state.worker = Worker::Refusing;` replaced with `state.worker = Worker::Stopped; Self::release(state); state.took = Some(Branch::WorkerNotActive);` | `daemon_start_loses_the_lock_to_a_late_worker` |
| R16. `BUSY_ONLY_WHILE_LIVE` reads `state.receipt_live()`, so no receipt counts as finished | `current_code_leaves_..` (the bare marker is now its shortest path), `the_fix_answers_..`, `the_fix_needs_the_receipt` |
| R17. The start state with a bare `.upgrade-active` removed from `init_states` | `current_code_breaks_only_the_finding`, `settle_relies_..`, `the_fix_answers_..`, `the_fix_refuses_a_marker_with_no_receipt` |
| R18. In `step`'s `Accept` arm, `} else if state.marker {` made `} else if false {` | `current_code_breaks_only_the_finding`, `current_code_leaves_..`, `settle_relies_..`, `the_fix_answers_..` |
| R19. The Inspect clearing row's predicate reads `Some(Caller::Replay)` | `each_clearing_row_is_its_callers` |
| R20. The fix's marker gate dropped: in `admission`, `marker && !state.receipt_terminal()` replaced with `state.receipt_live()`, as `skips_marker_gate` does | `the_fix_answers_from_the_receipt` (`NO_NEW_REFUSAL`), `the_fix_refuses_a_marker_with_no_receipt`, and `the_fix_still_writes_the_marker`, whose pinned path no longer ends in an admission |
| R21. The fix's marker gate dropped: `marker && !state.receipt_terminal()` replaced with `!state.receipt_terminal()` | `the_fix_answers_from_the_receipt` (`NO_NEW_REFUSAL`), `the_fix_still_writes_the_marker` |
| Each flag made a no-op | that flag's own test. `reconcile_skips_worker_state` also reddens `settle_relies_..`. `settle_skips_worker_state`'s test is `settle_relies_..`, which goes red because the flag's `COVERAGE` row is reached again |

A side-effect failure is a test whose pinned path, flag or unreached-row list your edit already moved. A guard that no longer earns its keep fails with its flag alone, on a model with no other edit.

A scratch test inside `upgrade_attempt::tests` can call the module's private helpers. Put it at the end of `mod tests`, and run it alone with a nextest filter that matches only its name:

```
cd core && cargo nextest run -p ployz-verify -E 'test(=upgrade_attempt::tests::scratch)' --no-capture
```

It adds to the 24 tests that the full run counts. The snippet below prints 12494 states, then `BUSY_ONLY_WHILE_LIVE` and `RECEIPT_FINAL` breaking and `NO_MUTATION_BESIDE_A_LIVE_WORKER` holding, as the guard table says for `settle_reuses_snapshot`. Its `breaks` then prints an 8-step story that ends in daemon start's `clear_active`.

```rust
    #[test]
    fn scratch() {
        let model = as_is_without(Broken { settle_reuses_snapshot: true, ..Broken::default() });
        let checker = check(model);
        println!("{} states", checker.unique_state_count());
        for name in [BUSY_ONLY_WHILE_LIVE, NO_MUTATION_BESIDE_A_LIVE_WORKER, RECEIPT_FINAL] {
            let broke = checker.discovery(name).is_some();
            println!("{name}: {}", if broke { "breaks" } else { "holds" });
        }
        // A COVERAGE row's name is the name of its `sometimes` property.
        breaks(as_is(), "daemon start's settle removes the marker a stopped writer left");
    }
```

## The fix

The fix is a design for a later PR. No product code has it yet.

```text
try_mutation
  take the installation lock shared          Busy if another process holds it
  .upgrade-active exists?                    no: admit, reading nothing more (as today)
    read upgrade-attempt.json                Busy unless it is terminal; no receipt is Busy
```

In the model that is `marker && !receipt_terminal()`. A marker with no receipt still refuses, as `try_mutation` does today and `reconcile_for_install` does (`installer/upgrade.rs:357-362`). Two ployzd tests pin that refusal: Forget gets Conflict (`machine_api/local_tests.rs:188`), and a Build is refused (`build/tests.rs:451-453`). `the_fix_refuses_a_marker_with_no_receipt` pins it in the model. Dropping the refusal breaks no `always` property: only the `COVERAGE` row "try_mutation refuses on .upgrade-active with no receipt" goes unreached. So that test and `the_fix_answers_from_the_receipt`, which expects every row reached, are what catch it (R12).

The writes stay as they are: `request_locked` and the worker still write and clear `.upgrade-active`.

| Part | What it keeps | `Fix` field | Test, and the property that breaks without it |
|---|---|---|---|
| The receipt read beside the marker | A finished attempt's stale marker no longer refuses | `ignores_receipt` | `the_fix_needs_the_receipt`, `BUSY_ONLY_WHILE_LIVE` |
| The marker gate on that read | Admission reads nothing new while no marker exists. The code as it is admits on Accepted with no marker, which a daemon killed between `:227` and `:228` leaves, and so does the fix | `skips_marker_gate` | `the_fix_reads_the_receipt_only_beside_the_marker`, `NO_NEW_REFUSAL` ("try_mutation answers Busy only while a process holds the installation lock or .upgrade-active exists") |
| `.upgrade-active` still written | The handoff between the request dropping the lock and the worker taking it, for the fix's own admission and for a release before the fix | `writes_no_marker` | `the_fix_still_writes_the_marker`, `NO_MUTATION_BESIDE_A_LIVE_WORKER` |

A receipt-only shape, which reads the receipt on every `try_mutation`, was rejected:
- It reads and decodes JSON on every admission, including the hot callers above: Build admission, image ingest, a Deploy's container step, copy cuts and the plugin.
- A receipt it cannot decode would refuse every mutation on the Machine.
- It refuses Accepted with no marker, where the code as it is admits. `the_fix_reads_the_receipt_only_beside_the_marker` is that shape's counterexample.

What the fix keeps, and what the later PR must decide:
- **The dead-attempt window.** A receipt that is not terminal, beside the marker and a worker that stopped or never started, still refuses until a reader writes Interrupted, as on the code as it is (`the_fix_keeps_the_dead_attempt_window`). A worker killed mid-install leaves that state. So does a daemon killed between `:228` and `:230`, and a terminal write that fails at `:343`, which the model leaves out.
- **Older readers.** A release before the fix still refuses on the marker a stopped writer left, until a settling reader clears it (`the_fix_leaves_older_readers_their_window`). The model offers the older reader at every state, as the reader the Stable promise keeps. It does not show which mix of releases on one Machine produces it.
- **Read and decode failures.** With the marker present, a receipt that cannot be read or decoded must refuse, as `try_mutation` does on marker metadata errors today (`mutation.rs:64-67`). The model has no such error.
- **A marker naming another attempt.** Refuse, as today, because the receipt cannot say the marker's attempt ended. `request_locked` writes a new attempt's receipt only after `active` found no marker (`installer/upgrade.rs:191`, `:227`). `clear_active` leaves a marker naming another id in place (`mutation.rs:117`). So that state is permanent today: no reader clears it, and every new request refuses at `:191`. The fix PR may want an operator path out. The model has one attempt, so it has no such state.
- **A persistent Io.** With the fix, admission reads the terminal receipt beside the marker and admits, so mutations no longer refuse. But every settling caller still runs its own `clear_active`, and each still fails with the Io: Inspect's attempt read (`machine/local_machine.rs:512-520`), Leave (`:956`), a new upgrade (`installer/upgrade.rs:185`, `:378`), the poll (`:259`), and daemon start, which exits before it serves (`daemon.rs:184-186`).
- **Layering.** The fix makes `MutationGate` (`mutation.rs`) decode the installer's `StoredAttempt` (`installer/upgrade.rs`). That is a layering choice for the PR that ships it.
- **Asking systemd.** A shape that asked systemd for the worker's state at admission could end the dead-attempt window. That leans on systemd as bet 9 asks ("systemd for lifecycle", `core/DESIGN.md:207-208`), so it crosses none of the bet's red flags. Its cost is a `systemctl` spawn on every admission, on the hot path.

## Read a counterexample

A failure prints the test name, the property, the model's flags, and a numbered story cut at the first state in BFS order that breaks the property, so the last row is the broken state. For a `sometimes`, the story is cut at the first state that reaches it. `breaks` keeps that first path with a single-thread visitor and stops at the first discovery. `breaks_where` keeps the first path that also passes a filter on the path, and explores the whole model.

```
 5. Daemon: the daemon process is killed while request_locked holds the lock, after the Failed{Launching} write and before clear_active (:235-236); its locks are released, and systemd starts it again (Restart=always, RestartSec=2, installer/host.rs:296-298)
    receipt: Failed | marker: names it | lock: free | daemon: Down | request: Done | worker: Absent | reader: none | install: none
```

Each row is one step, named for who takes it: Daemon, Worker, Reader, Install, Daemon RPC, Volume plugin or Older release. The line under it is the state after the step:
- `receipt` is the outcome `upgrade-attempt.json` holds.
- `marker` says whether `.upgrade-active` names the attempt.
- `lock` is who holds the installation lock exclusively: `Reader` for a reader the daemon hosts, `Install` for `ployzd install`.
- `daemon` is Serving, Down or Starting.
- `request` and `worker` are where the acceptance task and the worker are.
- `reader` is the settling reader the daemon hosts and its next step, and `install` is `ployzd install`'s next step.

A row that skips a guard says "(under Broken)" when a flag is set.

A test that fails inside `assert_properties` (`the_fix_answers_from_the_receipt`) reports only the first property it finds broken, and prints the raw actions instead of the rows. To see the rows, call `breaks` with that model and property in a scratch test.

## Real bug or model error

A counterexample is a **lead**, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites. Each `Action` and `Branch` variant's doc comment names its lines, and `reads_as` prints the statement each step copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `actions`, which decides what is offered in each state, including what the daemon's state allows;
  - `UpgradeAttempt::step`, for the request's and the worker's steps and the kills;
  - `start`, `reader_step` and `reconcile_locked`, for the readers;
  - `admission`, for `try_mutation`.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

`next_state` panics if any step other than a kill or a `clear_active` Io leaves the lock free beside a finished attempt's marker. A panic there is a model error.

## A break that stays green

Guards overlap, so removing one can leave every `always` property holding. Before calling a guard dead, rerun with a neighbouring flag set too.

- `settle`'s `worker_state` (`:265`) is backed by `reconcile_locked`'s (`:381`), which runs under the lock. With `settle_skips_worker_state` alone, `INTERRUPTED_ONLY_STOPPED` and `RECEIPT_FINAL` still hold. Only its own `COVERAGE` row, "settle returns a running worker's receipt as it stands", goes unreached. `settle_relies_on_the_worker_check_under_the_lock` pins both halves.
- On the code as it is, `BUSY_ONLY_WHILE_LIVE` already breaks through the finding. A flag that removes a guard adds nothing to that property, so each guard test names another property.

## Keep the model in step

When a change alters any of these, change the model in the same PR:
- how the attempt writes its receipt or marker;
- how `try_mutation` decides;
- which process runs what, or how long it holds the lock;
- how a settling reader repairs.

Examples:
- a reordered write, such as `clear_active` before the terminal write;
- a new reader of the marker or the receipt;
- a new caller of `settle`;
- a timer that settles;
- a daemon start that serves before it reconciles, or that retries a lost lock.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `request_locked`: the `Accept`, `Mark`, `Launch`, `LaunchFails`, `WriteLaunchFailure` and `ClearAfterLaunchFailure` arms of `step`, and the `Request` states.
   - `work`: `WorkerLocks`, `WorkerRefuses`, `WorkerFinishes`, `WorkerClears` and `WorkerDies`.
   - The daemon process and its start: `DaemonDies`, the `Daemon` states, `start_lost`, and what `actions` offers in each.
   - `clear_active`: `UpgradeAttempt::clear` and `ClearFails`.
   - `try_mutation`: `admission`, and the `Asker` that `actions` offers.
   - `settle`: `start`, `reader_step` and `settle_busy`.
   - `reconcile_locked` and its callers' decisions: `UpgradeAttempt::reconcile_locked` and the `Decide` arm.
   - `install_at`: the `install` slot, `InstallStep` and `InstallDies`.
   - `depart`: `settled` and the `Depart` arms.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change ships the fix, model it as the code writes it and make it the default. Then turn the finding tests into a `Broken` flag that restores the old admission, and keep the tests, since their printed paths are the record of what the fix keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names (`took`), and, for a clear, the caller that removed the marker (`cleared`). Add the pair to `each_clearing_row_is_its_callers`.
5. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Bounds and blind spots

The bounds:
- **One attempt and one worker.** The receipt and the marker always name it.
- **One settling reader the daemon hosts at a time**, beside `ployzd install`, the request task and the worker, with any number of readers over a path. A second daemon reader would add no step the model lacks:
  - Every reader takes the lock only through `try_installation`, so one holds it. The other's `settle` returns without clearing (`:260`, `:270`), which the model reaches against install and the worker.
  - The holder can clear or return Io, which the model reaches for every caller.
  - A daemon reader cannot die alone, since the daemon's kill ends all of them.
  - Codex's round 1c review of PR #3845 lifted this bound in a scratch copy, with two daemon reader slots beside `ployzd install`. The code as it is broke only `BUSY_ONLY_WHILE_LIVE`, at 155553 states, and the fix broke nothing and reached all 30 rows, at 184020 states. That copy kept `State::last`, which later rounds removed.
- **Any number of `try_mutation` calls.**
- **No second attempt.** A new request that passes `:186` and `:191` ends there (`NewRequestGoesOn`).
- **A unit starts only at a step that holds the lock.**
  - The steps are `Launch`, `LaunchFails { started: true }` (the 20 s timeout, `:22`, `:440-447`) and `DaemonDies { started: true }` (the daemon killed during `launch_worker`).
  - `launch_worker` runs `systemd-run` for a transient `Type=exec` unit, without `--no-block` and with `kill_on_drop` (`:397-459`). A `systemd-run` that timed out or was killed may already have asked systemd to start the unit, which can then start later under no lock. A later start is out of the model.
  - Such a worker would find the receipt terminal and hold the lock until it returns `NotActive` (`:306-310`), as `daemon_start_loses_the_lock_to_a_late_worker` shows. But it could also start beside a reader inside `reconcile_locked`.
  - With both `started: true` actions removed, `the_worker_refuses_a_finished_attempt`, `daemon_start_loses_the_lock_to_a_late_worker` and the NotActive `COVERAGE` row lose their witness.

The other processes can step between any two reads, not only at an await. Each read in the model:
- **`try_mutation`'s two reads.** It holds the lock shared, and every write needs it exclusively, so they are one step.
- **`settle`'s caller's receipt read** (`latest`, `:113`; `replay`, `:95`). It runs without the lock and is its own step (`Start`). `settle` returns that read when it does not take the lock (`At::*::seen`).
- **`settle`'s `worker_state`** (`:265`). It runs without the lock and is its own step (`QueryWorker`). The worker and the request can step between it and `try_installation` (`:268`), and `settle_relies_on_the_worker_check_under_the_lock` shows a unit starting there.
- **`try_installation`, the re-read and `reconcile_locked`** (`:268-274`). These are one step. The lock keeps every writer out, and the model starts a unit only under the lock (above).
- **`reconcile_locked`'s reads and its Interrupted write.** These are one step, followed by a separate `clear_active` step. A waiting unit can die between `worker_state` and the return, and `reconcile_locked` then returns without a write, as it would have anyway.
- **The worker's lock and receipt read** (`:306-307`). These are one step, and its NotActive return (`:309`) is another.
- **`depart`'s check and its `try_mutation`** (`machine/local_machine.rs:946-953`, `:959`). These are separate steps, and the check reads `settle`'s return, not the receipt again.
- **A new request's `replay`** (`installer/upgrade.rs:95-99`). It returns None for another id whatever the receipt holds, so the model starts at its `try_installation`.

An RPC cancelled while the daemon lives drops its future at an await. In `settle` those are `worker_state` (`:265`) and `reconcile_locked` (`:274`), whose only await is its own `worker_state` (`:381`). So a dropped `settle` stops before any write, like a reader that never takes its next step, and the model has no separate action for it. The request task is spawned (`:156-158`), so a cancelled request RPC does not stop it (`cancelled-launch`, `installer/upgrade/tests.rs:189-201`).

Out of scope:
- a second attempt, and the same-id request path (`:178-183`);
- the in-process admission lock that `finish_mutation` takes before `try_mutation` (`machine/local_machine.rs:342`);
- `request_locked`'s already-installed path (`:216`) and release resolution;
- Io from the receipt writes (`installer/upgrade.rs:227`, `:316`, `:343`, `:392`) and from `mark_active` (`:228`). A terminal write that fails at `:343` leaves Running and the marker: the dead-attempt state;
- receipt read or decode errors, and `worker_state` errors (`WorkerEvidence`), which `inspection-unknown` pins (`installer/upgrade/tests.rs:329-340`);
- probes that do not hold the shared lock, and `lock_installation` waiting on shared holders;
- a reader's `try_installation` failing with Io;
- a reboot, which stops every process at once;
- systemd's restart limits on the daemon unit;
- how long any step takes, and how often Inspect is called. The model has no clock.

A green model says nothing about them. Cover the code ones with:

```
cd core && cargo nextest run -p ployzd mutation::tests installer::upgrade::tests
```

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/installer/upgrade.rs`: `request`, `request_locked`, `replay`, `latest`, `settle`, `work`, `reconcile`, `reconcile_for_install`, `reconcile_locked`, `launch_worker` and `worker_state`.
- `core/crates/ployzd/src/mutation.rs`: `try_mutation`, `try_installation`, `lock_installation`, `active`, `mark_active` and `clear_active`.
- `core/crates/ployzd/src/machine/local_machine.rs`: `inspect`, `admit_installation`, `finish_mutation`, `finish_installed`, `update`'s fast path and `depart`.
- `core/crates/ployzd/src/machine/local_machine/upgrade.rs`: `request_upgrade`.
- `core/crates/ployzd/src/daemon.rs`: the start-up `reconcile`, and serving after it.
- `core/crates/ployzd/src/installer/mod.rs`: `install_at`.
- `core/crates/ployzd/src/installer/host.rs`: `ployz.service`'s `Restart=always` and `RestartSec=2`, and the volume plugin's own unit.
- `core/crates/ployzd/src/volume_plugin/storage.rs`: `admit_mutation`.
- `core/crates/ployz-core/src/rpc/upgrade.rs`: `MachineUpgradeOutcome::is_terminal`, which the model calls on one outcome per variant.

Sync commit: `538657f812e6aff0d7b7b4f10f24b6fa6e76653e`, the code commit of the change that moved these lines onto `MachineLayout`, rebased on `main`, and every line cited above in `ployzd`, `ployz-core` and `ployz-verify` is at it. The CLI-side citations (`core/crates/ployz/src/handlers/server.rs`, `cluster.rs`, `deploy/pipeline.rs`, `sdk.rs`) had already moved on `main` and are not rechecked. Code in the list above that changed after it has not been rechecked.
