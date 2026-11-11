---
name: verify-bridge-notes
description: Model-check the ployz bridge's two member notes across daemon starts, bridge-stopped-members.json (Stopped) and bridge-member-addresses.json (Pins), through the readdress, the pin pass and Stopped::start, against crashes, Docker refusing a start, a listing, a connect or a disconnect, Docker's restart policy, failed note writes and the operator's own start, stop, hold and remove, with the Stateright model in core/crates/ployz-verify/src/bridge_notes.rs. Use when a change touches core/crates/ployzd/src/network/linux.rs (ensure_docker_network, ensure_docker_network_shape, readdress_docker_network, replace_docker_network, pin_members, detach, stop, settle, rejoin, rejoin_with, bridge_members, needs_pin, addresses), core/crates/ployzd/src/network/linux/stopped.rs (Stopped::load, note, start, gone), core/crates/ployzd/src/network/linux/pins.rs (Pins::load, restore, record, finish), or what ContainerStep::Start, Stop, Hold and ContainerRuntime::stop do to a Container the notes name.
---

# Verify the bridge member notes

The model in `core/crates/ployz-verify/src/bridge_notes.rs` explores the two notes a daemon start keeps about the ployz bridge's members: `bridge-stopped-members.json`, the members a start stopped so it could move them (`Stopped`, `core/crates/ployzd/src/network/linux/stopped.rs`), and `bridge-member-addresses.json`, the address each member held before a detach (`Pins`, `core/crates/ployzd/src/network/linux/pins.rs`). A bare `:n` below is a line of `core/crates/ployzd/src/network/linux.rs`; `stopped.rs:n` and `pins.rs:n` are lines of those two files. Every other path is written in full from the repo root, and every command runs from the repo root.

## What the model copies

**What hosts each step.** One `ployzd` process runs on the Machine. Each daemon start runs `NetworkPlane::start` (`core/crates/ployzd/src/daemon.rs:274`), whose `ensure_docker_network` (`:687-716`) loads both notes (`:708-709`), settles the bridge's shape and readdresses it when its IPv6 shape is wrong (`:712`, `:731-741`), pins its members (`:713`) and starts the members `Stopped` names (`:714`). The Machine API is built and served only after `NetworkPlane::start` returns (`core/crates/ployzd/src/daemon.rs:347-372`, `:411-417` of that file), so the operator's own steps on a Container, `ContainerStep::Start`, `Stop` and `Hold` (`core/crates/ployzd/src/machine/local_machine/container.rs:1099-1136`), run only between daemon starts. The model offers them only while `Process::Serving`, after a start returned and before the next one. Docker's restart policy acts at any step. A crash ends the process: the in-memory `Stopped`, `Pins` and member list go, and the files and Docker's state stay. Each Docker call is one atomic step, and so is each `atomic_write` (`core/crates/ployzd/src/filesystem.rs:13-26`).

**The steps,** `BridgeNotes::daemon`, one arm per statement, each printing its lines in the story:
- `At::Load`: `Stopped::load` and `Pins::load` (`:708-709`).
- `At::Shape`: `inspect_network` (`:731-741`). The bridge is `Old` (fits, IPv6 shape wrong: the readdress runs), `New` (fits) or `None` (`create_bridge`, `:781`).
- `At::List(phase)`: `bridge_members` (`:811`, `:1206-1240`) or `bridge_containers` (`:915`, parsed at `:925`), then `Pins::restore` (`:818`, `:926`, `pins.rs:46-60`). A failed listing in the readdress returns `Ok` (`:813-816`), so the pin pass runs; in the pin pass it returns before `finish` (`:917-922`), and `Stopped::start` runs.
- `At::Record(phase)`: `Pins::record` (`:819-822`, `:932-940`, `pins.rs:62-78`). A failure abandons the readdress, and in the pin pass makes `recorded` false, so attached members are skipped (`:946-948`).
- `At::Detach`: `detach` (`:990-1014`): a disconnect, and when Docker refuses it as between restarts, `stop` (`:1021-1056`) and a second disconnect. `stop` inspects, asks `settle` (`:1173-1187`), notes the member with `Stopped::note` before `stop_container` when `settle` answers StopAndNote (`:1043-1048`, `stopped.rs:47-56`), and returns without stopping when the note fails.
- `At::Remove` and `At::Create`: `replace_docker_network` (`:836-842`, `:855-868`) and `create_bridge`.
- `At::Rejoin`: `rejoin` and `rejoin_with` (`:1058-1102`): the pinned connect, then `connect(None)` (`:1088-1093`). The readdress discards the answer (`:1058-1061`); the pin pass puts a false into `unjoined` (`:981-983`).
- `At::Finish`: `Pins::finish` keeps only `unjoined` (`pins.rs:82-100`).
- `At::Start(member)`: `Stopped::start`'s start of one member (`stopped.rs:60-79`): Docker runs it, it already runs (bollard reads Docker's 304 as `Ok`), "No such container" drops it (`stopped.rs:66`, `stopped.rs:104-110`), and any other error keeps it.
- `At::Update`: `Stopped::start`'s removal or rewrite of the note (`stopped.rs:80-93`). A failure keeps the old file, with every id it held.

**The other actors.** `Who::Crash` ends a daemon start at any step. `Who::Docker` is the restart policy moving a member that was not stopped through the API between running and between restarts, or, under the `no` policy, from between restarts to exited. `Who::Operator` is `ContainerStep::Start`, `Stop`, `Hold` (a stop of a Container at rest, whose 304 reads as success, `core/crates/ployzd/src/docker/lifecycle.rs:870-881`) and the operator removing the Container.

**The real code it calls.** Only `ployz_core::RestartPolicy::default()`, to pick the restart policy of the first start state (`core/crates/ployz-core/src/host_config.rs:13-14`). `ployz-verify`'s only Ployz dev-dependency is `ployz-core` (`core/crates/ployz-verify/Cargo.toml`), and every function above lives in `ployzd`, so each is a hand copy citing its lines. `bridge_address::next` is copied as `next` (`core/crates/ployzd/src/docker/bridge_address.rs:80-102`).

**Model-local types.** `RestartPolicy` derives no `Hash` (`core/crates/ployz-core/src/host_config.rs:8`), and Stateright needs `State: Hash`, so the state holds `Policy`, `Restarts` or `No`, which is all `settle` reads. `on-failure` is left out: `settle` answers `Leave` for it (`:1184`), the answer it gives a member that already exited (`:1177-1179`), which `No` reaches. A member is `M0` or `M1`, an address `A`, `B` or `C`, and an endpoint `Detached`, `Dynamic(address)` or `Pinned(address)`, kept apart from the address a note owes.

**Each read, and why the model's steps reach the same states.**
- **The listings** at `:811` and `:915` (parsed at `:925`). Each pass acts on its listing without reading again. Between the listing and each act, Docker's restart policy can move a member between running and between restarts, and the operator cannot act (their steps wait for `Process::Serving`). The model's listing is one step and every act after it is another, and a restart-policy move is offered between any two, so the model acts on a listing a move has made stale: the gap between a read and the act on it.
- **`stop`'s `inspect_container`** (`:1022-1030`). The model's `Step::Inspect` reads the member's run state and policy, and `settle`'s answer decides the next step; the note and the stop are their own steps, with a restart-policy move offered before each.
- **`pin_members`' `inspect_container`** (`:949`). It reads only the endpoint settings to rejoin with, and fails only for a gone container. The model folds it into the first detach step of the pin pass: a gone member is skipped (`:959-966`), and the endpoint it reads does not depend on what a restart-policy move changes.

## The findings

**The first finding.** A member `Stopped` keeps stays in `bridge-stopped-members.json` after the operator stops it, and the next daemon start's `Stopped::start` starts it again (`:714`, `stopped.rs:60-79`). Nothing in `ContainerRuntime::stop` touches the note (`core/crates/ployzd/src/docker/lifecycle.rs:611-670`). Its property is `STARTS_ONLY_OWED`, an `always`: "Stopped::start starts only a container Ployz stopped and nobody stopped since". Its ghost, `State::operator_stopped`, is set by an operator stop, hold or remove and cleared by Ployz's own stop. Three routes keep an entry:
- **a refused start** (`current_code_starts_a_refused_member_the_operator_stopped`): Docker refuses the start with a 500, the id is kept (`stopped.rs:67-73`), the operator starts M0 and stops it, and the next start runs it;
- **the netless 404**, kept on purpose (`stopped.rs:102-103`). On Docker 29.1.3, checked during this PR's review, one way to get it is `docker network rm` on a network a stopped container is still attached to: the removal succeeds, and the container's next start answers HTTP 404 "failed to set up container networking: network … not found". The code removes the bridge only after every detach (`:836-842`), so no modelled step leads there, and the model gives the 404 as Docker's answer to any start. That over-approximates, and no finding replay relies on it;
- **route 3, a failed update** (`current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it`): Docker runs M0, only the note's removal fails, and the old file keeps M0 (`stopped.rs:80-93`).

**The second finding.** `Stopped::start` starts every member written down, whether or not `pin_members` rejoined it. Its property is `STARTS_ONLY_ON_BRIDGE`, an `always`: "Stopped::start starts a member only while Docker shows it on the bridge". Two routes leave M0 detached:
- **the early return** (`current_code_starts_a_member_pin_members_returned_before_rejoining`): the readdress's rejoin is refused pinned and refused again by `connect(None)`, and the pin pass's listing fails, so `pin_members` returns before rejoining anything (`:915-923`);
- **the false rejoin** (`current_code_starts_a_member_whose_rejoin_failed`): start 1 dies after detaching M0, start 2's readdress lists M0 detached and rejoins nothing, and the pin pass's pinned connect and `connect(None)` are both refused, so `rejoin_with` returns false (`:981-983`).

No repo line says what Docker does when it starts a container whose `ployz` endpoint was removed. On Docker 29.1.3, checked during this PR's review, it runs the container with no network: `NetworkSettings.Networks` is `{}`, and only `lo` has an address inside it. Other Docker versions are unchecked, so the model gives both answers: it runs detached (`Started::Ran { attached: None, .. }`), or Docker attaches it on a free address (`Pick::Dynamic`).

Each finding test replays its schedule with `follow`, asserts each branch, then asks the checker for its first path to the same broken state (`breaks_where`). Each story shows one schedule the checker found.

**Two more `always` properties keep the code's own guards**, and both hold on the code as it is:
- `NOTED_BEFORE_STOP`: "the start stops a member it must start again only once the stopped note names it". `Stopped::note`'s doc comment, in full (`stopped.rs:47`): "Writes `id` down. Stop it only once this succeeds." `stop`'s doc says the same (`:1019-1020`).
- `ADDRESS_NOTED_BEFORE_DETACH`: "the start detaches a member only while the address note names an address for it". `Pins::record`'s doc comment, in full (`pins.rs:62-63`): "Writes down the address of every member that has one. Detach a member only once this succeeds." The readdress's doc says the same (`:795-797`).

## Run it

```
cd core && cargo nextest run -p ployz-verify bridge_notes --no-capture
```

Expected: the summary line ends with `36 tests run: 36 passed, 152 skipped`. `current_code_breaks_both_properties_and_reaches_every_row` prints `as is: 25538 states`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows` prints `drop: 20445 states`, `fix_with_retry_breaks_the_first_property_by_a_refusal` prints `retry: 26153 states`, and `restore_held_restores_an_address_another_member_holds` prints `two members, unguarded: 256389 states`. Each story prints under its test's name. The first build compiles 197 crates: 42.71 s on this PR's build machine with an empty `core/target`, apart from the run; reviewers saw 53 s to 80 s. A warm run takes under 15 s of nextest time with `--no-capture`, which runs the tests one at a time (three warm runs at this PR's head: 2.512 s, 2.566 s, 2.574 s). CI runs `ployz-verify` in its workspace nextest run.

Read the tests in six groups.
- **The findings, on the code as it is.** The four route tests above, and `current_code_breaks_both_properties_and_reaches_every_row`, which expects `STARTS_ONLY_OWED` and `STARTS_ONLY_ON_BRIDGE` to break, `NOTED_BEFORE_STOP` and `ADDRESS_NOTED_BEFORE_DETACH` to hold, and every `COVERAGE` row to be reached.
- **The first property's edge states**, each from `the_refusal`'s start 1 with different operator steps before start 2: `first_property_holds_for_a_member_nobody_touched`, `first_property_fails_for_a_member_the_operator_held`, `first_property_fails_for_a_member_the_operator_started_then_stopped`, `first_property_holds_for_a_member_the_operator_only_started` (the 304) and `first_property_holds_for_a_member_the_operator_removed` (gone).
- **The second property's edge states**, one step from a hand-built start: `second_property_fails_for_a_detached_member_with_an_address_noted`, `second_property_fails_for_a_detached_member_with_no_address_noted` (`Pins::record` skips a member with no address, `pins.rs:69`), `second_property_holds_for_a_member_attached_on_another_address_than_owed` (a row, not a property: Bounds and blind spots) and `second_property_holds_for_a_pinned_member`.
- **The fix's start predicate**, at each state it must handle: `fix_starts_a_start_debt_attached`, `fix_defers_a_start_debt_detached`, `fix_starts_an_address_then_start_debt_attached_at_the_noted_address`, `fix_defers_an_address_then_start_debt_detached`, `fix_starts_an_address_then_start_debt_attached_on_another_address`, `fix_handles_an_address_then_start_debt_whose_address_another_member_holds`, `fix_drops_or_retries_a_start_docker_refuses`, `fix_drops_or_retries_a_netless_refusal` and `fix_drops_a_gone_container`. Then the findings' replays on the fix: `fix_replays_the_refusal`, `fix_replays_the_failed_update`, `fix_replays_the_early_return` and `fix_replays_the_false_rejoin`.
- **The flags,** one test each (Breaking a guard yourself, below).
- **The policies**: `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows` and `fix_with_retry_breaks_the_first_property_by_a_refusal`.

## The fix

The fix variant is the shape of a later PR, E-125. No product code has it. It reads each member's `MemberDebt` (`Address`, `Start` or `AddressThenStart`) from the two note files, with their formats unchanged (`stopped.rs:18-21`, `pins.rs:17-20`), and keeps the code's order: Docker's start first, then the note's update.

```text
Stopped::start, for each id the note names:
  Docker shows the member detached        defer its start (Started::Deferred)
  otherwise                               start it, as the code does
  a start Docker refuses, or one deferred Drop: drop the id
                                          Retry: keep it for RETRIES later daemon starts, then drop it
```

`Retry` needs a count per id that `bridge-stopped-members.json`'s `BTreeSet<String>` cannot hold (`stopped.rs:18-21`), so it needs a new format for that file or a second file beside it; the model keeps the count in `State::retries`.

**The address check is left out.** The fix as first drawn started a member that owes an address only at that address. A scratch run with that check added gave every `always` property the same verdict with and without it, with one and two members, under Drop and Retry; its only effect was that the row "Stopped::start starts a member attached on a dynamic address while the address note owes another" went unreached. So no property here needs it, and the fix checks only that Docker shows the member attached. A property that would need it says a member is started only at the address the note owes it; E-125 decides whether to add it.

| Part | Flag | Test, and the `always` property that breaks without it |
|---|---|---|
| Defer a member Docker shows detached | `fix_starts_detached` | `fix_starts_detached_breaks_the_second_property`, `STARTS_ONLY_ON_BRIDGE` |
| Drop, on a start the fix cannot pay | `fix_keeps_unpaid` | `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal`, `STARTS_ONLY_OWED` by a refusal |
| Retry's limit | none; K03 below removes it | No `always` property: `STARTS_ONLY_OWED` already breaks by a refusal under Retry. Without the limit, `fix_with_retry_breaks_the_first_property_by_a_refusal` goes red only on its unreached-rows list ("the fix drops a member whose start it cannot pay" and "the fix drops a member whose retries are spent"), and `fix_drops_or_retries_a_start_docker_refuses` on its step of a member whose retries are spent |

**Per policy,** from `scratch_rows` (below) at this PR's head:

| Policy | States | `always` that hold | `always` that break | Rows unreached |
|---|---|---|---|---|
| The code as it is | 25,538 | `NOTED_BEFORE_STOP`, `ADDRESS_NOTED_BEFORE_DETACH` | `STARTS_ONLY_OWED`, by a kept refusal and by route 3, `STARTS_ONLY_ON_BRIDGE`, by the early return and the false rejoin | none |
| Drop | 20,445 | `STARTS_ONLY_ON_BRIDGE`, `NOTED_BEFORE_STOP`, `ADDRESS_NOTED_BEFORE_DETACH` | `STARTS_ONLY_OWED`, by route 3 only | "Docker refuses a start, and the member stays written down"; "Docker answers the netless 404, and the member stays written down"; "Stopped::start rewrites the stopped note with the members Docker did not start"; "a member Docker refused is started at a later daemon start"; "a member Docker refused for a missing network is started at a later daemon start"; "Docker attaches a detached member it starts on a dynamic address while the address note owes another"; "the fix drops a member whose retries are spent"; "a member deferred for its address is started at a later daemon start" |
| Retry(1) | 26,153 | `STARTS_ONLY_ON_BRIDGE`, `NOTED_BEFORE_STOP`, `ADDRESS_NOTED_BEFORE_DETACH` | `STARTS_ONLY_OWED`, by route 3 and by a refusal | "Docker attaches a detached member it starts on a dynamic address while the address note owes another" |

Drop keeps `STARTS_ONLY_ON_BRIDGE`, `NOTED_BEFORE_STOP` and `ADDRESS_NOTED_BEFORE_DETACH`, and breaks `STARTS_ONLY_OWED` only by route 3: `first_break` finds no other break, and the route 3 row is reached. It gives up the rows that start a member at a later daemon start after its start was refused or deferred. Retry keeps those rows and breaks `STARTS_ONLY_OWED` by a refusal too. No policy keeps both. Route 3 does not pass through the policy. A write before Docker's start would close it, but would open a window the module's doc rules out: a start that dies between the note's update and Docker's start leaves the member stopped with no note, so no later start runs it (`stopped.rs:1-3`, "A start that dies … leaves the note for the next start"). The row "a member Ployz stopped is started at a later daemon start after a start that died" stays reached under that variant, by a crash earlier in the start, so the row cannot show the loss; a scratch variant in review reached the row and also left M0 exited with no note after a crash in the window. A property for it would say a member Ployz stopped stays noted until Docker runs it. Route 3 needs a design E-125 must choose.

## Read a counterexample

A story is a numbered list of steps, cut at the last state of a path. Each row names who steps, `ployzd`, `Operator, M0` or `Docker`, and prints the statement it copies with its lines. The line under it is the state after the step: the bridge, the stopped note, the address note, each member's run state and endpoint ("operator stopped it" when the ghost is set, "retried n" under Retry), and the process. `follow` prints "a schedule" where `breaks` prints the property.

These are the last rows of `current_code_starts_a_refused_member_the_operator_stopped`'s replay:

```
20. ployzd: inspect_network: the bridge fits (:731-741)
    bridge New | stopped note {M0} | address note none | M0: Exited, pinned A, operator stopped it | ployzd: start 2
21. ployzd: pin pass: bridge_containers lists the members, and Pins::restore gives each detached one its noted address (:915, :925-928)
    bridge New | stopped note {M0} | address note none | M0: Exited, pinned A, operator stopped it | ployzd: start 2
22. ployzd: pin pass: Pins::finish keeps nothing, and no address note stands to remove (pins.rs:82-88)
    bridge New | stopped note {M0} | address note none | M0: Exited, pinned A, operator stopped it | ployzd: start 2
23. ployzd: Stopped::start starts M0 (loaded, kept after Docker refused its start; the operator stopped it since) (:714, stopped.rs:64): Docker runs it
    bridge New | stopped note {M0} | address note none | M0: Running, pinned A, operator stopped it | ployzd: start 2
```

These tests print a story as they run, so a failing one still shows it, unless it fails because no path breaks: the four finding tests, `current_code_breaks_both_properties_and_reaches_every_row` (one story per finding), the five `first_property_*` tests, the four `fix_replays_*` tests (one story per policy), `stop_before_note_breaks_the_note_before_the_stop`, `detach_unrecorded_breaks_the_address_note_before_the_detach`, `fix_starts_detached_breaks_the_second_property`, `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal` and `fix_with_retry_breaks_the_first_property_by_a_refusal`. The rest print none. To see the story of a property that breaks, put this test at the end of `mod tests`, before the file's last closing brace, with the model and property you want:

```rust
    #[test]
    fn scratch_story() {
        let model = broken(None, Broken { stop_before_note: true, ..Broken::default() });
        breaks(model, NOTED_BEFORE_STOP);
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=bridge_notes::tests::scratch_story)' --no-capture
```

`breaks` prints the first path in BFS order whose last state breaks the property, and panics with "a path breaks the property" when none does. For a `sometimes`, such as a `COVERAGE` row's name, it prints the first path that reaches the row. Delete the test before you commit.

The story's words come from `reads_as`. An edit to a model step needs its `reads_as` arm edited with it, or the story goes stale.

A failure prints the property's full string, not its constant's name:

| Printed string | Constant |
|---|---|
| "Stopped::start starts only a container Ployz stopped and nobody stopped since" | `STARTS_ONLY_OWED` |
| "Stopped::start starts a member only while Docker shows it on the bridge" | `STARTS_ONLY_ON_BRIDGE` |
| "the start stops a member it must start again only once the stopped note names it" | `NOTED_BEFORE_STOP` |
| "the start detaches a member only while the address note names an address for it" | `ADDRESS_NOTED_BEFORE_DETACH` |

A `COVERAGE` or `FIX_COVERAGE` row prints its `name`.

## Real bug or model error

A counterexample is a lead, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `actions` and `offers`, which decide what each actor may do in each state;
  - `BridgeNotes::daemon`, `stop`, `disconnect`, `connect` and `list`;
  - `operator`, and the `Who::Docker` arm of `next_state`.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

## Breaking a guard yourself

Each `Broken` flag is already set by its own test, so setting one in a test of your own changes no other test and turns none red. To check the flag table below, run `scratch_flag_kinds`, after it. Add `--no-fail-fast` to a nextest command, so every red test is listed.

Which kind of property each flag breaks, from a scratch run that checked each flagged model in full against the same model unflagged. It lists the `always` properties that break on the flagged model but not on its unflagged one, and the rows the unflagged model reaches but the flagged one does not. On the code as it is, `STARTS_ONLY_OWED` and `STARTS_ONLY_ON_BRIDGE` break with or without a flag.

| Flag | `always` properties that break | `sometimes` rows left unreached |
|---|---|---|
| `stop_before_note`, on as_is | `NOTED_BEFORE_STOP` | "a readdress detach fails, and the bridge is not replaced" |
| `detach_unrecorded`, on as_is | `ADDRESS_NOTED_BEFORE_DETACH` | "the pin pass's record fails, and it skips an attached member" |
| `replace_after_failed_detach`, on as_is | none | "a readdress detach fails, and the bridge is not replaced" |
| `restore_held`, on as_is, two members | none | none |
| `netless_gone`, on as_is | none | "Docker answers the netless 404, and the member stays written down"; "a member Docker refused for a missing network is started at a later daemon start" |
| `fix_starts_detached`, on Drop | `STARTS_ONLY_ON_BRIDGE` | "the fix defers the start of a member Docker shows detached" |
| `fix_keeps_unpaid`, on Drop | none new: `STARTS_ONLY_OWED` already breaks on Drop by route 3. `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal` finds it breaking off route 3, by a refusal | "the fix drops a member whose start it cannot pay" |

`replace_after_failed_detach` and `restore_held` break no property here: no property needs either guard. Their tests pin what each flag changes. Without the first, a failed readdress detach goes on to remove the bridge with a member still on it, and the model takes both of Docker's answers, a removal that detaches the member or a refusal (`replace_after_failed_detach_removes_a_bridge_a_member_is_on`). Without the second, `Pins::restore` gives a detached member an address another member holds (`restore_held_restores_an_address_another_member_holds`, two members).

The scratch test that printed it follows. Put it at the end of `mod tests`, before the file's last closing brace, and run it alone; it takes 1.769 s of nextest time at this PR's head, after 4.18 s to rebuild the test binary with the scratch test in it, once the main command above has built its dependencies. The test profile is enough: the checks run in seconds.

```rust
    #[test]
    fn scratch_flag_kinds() {
        let two = BridgeNotes { members: 2, ..as_is() };
        let runs = [
            ("stop_before_note on as_is", as_is(), Broken { stop_before_note: true, ..Broken::default() }),
            ("detach_unrecorded on as_is", as_is(), Broken { detach_unrecorded: true, ..Broken::default() }),
            ("replace_after_failed_detach on as_is", as_is(), Broken { replace_after_failed_detach: true, ..Broken::default() }),
            ("restore_held on as_is, two members", two, Broken { restore_held: true, ..Broken::default() }),
            ("netless_gone on as_is", as_is(), Broken { netless_gone: true, ..Broken::default() }),
            ("fix_starts_detached on Drop", fixed(OnRefusal::Drop), Broken { fix_starts_detached: true, ..Broken::default() }),
            ("fix_keeps_unpaid on Drop", fixed(OnRefusal::Drop), Broken { fix_keeps_unpaid: true, ..Broken::default() }),
        ];
        for (name, base, broken) in runs {
            let baseline = check(base);
            let model = BridgeNotes { broken, ..base };
            let checker = check(model);
            println!("FLAG {name}: {} states", checker.unique_state_count());
            for property in model.properties() {
                let found = checker.discovery(property.name).is_some();
                match property.expectation {
                    Expectation::Always if found && baseline.discovery(property.name).is_none() => {
                        println!("FLAG   always breaks: {}", property.name);
                    }
                    Expectation::Sometimes if !found && baseline.discovery(property.name).is_some() => {
                        println!("FLAG   sometimes unreached: {}", property.name);
                    }
                    _ => {}
                }
            }
        }
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=bridge_notes::tests::scratch_flag_kinds)' --no-capture
```

An edit to the model's code changes every model at once. For this PR, each edit below was made alone at this PR's head, run with `cd core && cargo nextest run -p ployz-verify bridge_notes --no-fail-fast`, and reverted. Where the quoted text repeats, the line number says which.

| Edit, by function | Tests that went red |
|---|---|
| K01. `BridgeNotes::defers`, line 938: `self.fix.is_some()` read as `false`: fix part, the start's attached check: `defers` returns false, so the fix never defers (= fix_starts_detached) | `fix_defers_a_start_debt_detached`, `fix_defers_an_address_then_start_debt_detached`, `fix_handles_an_address_then_start_debt_whose_address_another_member_holds`, `fix_replays_the_early_return`, `fix_replays_the_false_rejoin`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal` |
| K02. `BridgeNotes::daemon`, `At::Start`: `Some(OnRefusal::Drop) => false,` read as `Some(OnRefusal::Drop) => true,`: fix part, the Drop policy: Drop keeps an unpaid start (= fix_keeps_unpaid under Drop) | `fix_defers_a_start_debt_detached`, `fix_drops_or_retries_a_netless_refusal`, `fix_drops_or_retries_a_start_docker_refuses`, `fix_replays_the_refusal`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows` |
| K03. `BridgeNotes::daemon`, `At::Start`: `Some(OnRefusal::Retry) => tries < RETRIES,` read as `Some(OnRefusal::Retry) => true,`: fix part, Retry's limit: Retry keeps an unpaid start at every daemon start | `fix_drops_or_retries_a_start_docker_refuses`, `fix_with_retry_breaks_the_first_property_by_a_refusal` |
| K04. `BridgeNotes::daemon`, `At::Start`, line 1456: `tries + 1` read as `tries`: Retry's count: a kept start does not count a retry | `fix_drops_or_retries_a_start_docker_refuses`, `fix_with_retry_breaks_the_first_property_by_a_refusal` |
| K05. `BridgeNotes::stop`, `Step::Note`: `Write::Failed => Step::Again,` read as `Write::Failed => stop,`: code guard, note before stop: a failed Stopped::note goes on to the stop (:1044-1047) | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K06. `BridgeNotes::daemon`, `At::Record`: `Phase::Readdress if !ok && !self.broken.detach_unrecorded => {` read as `Phase::Readdress if false => {`: code guard, record before detach in the readdress: a failed Pins::record does not abandon it (:819-822) | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K07. `BridgeNotes::daemon`, `At::Detach`: `&& !state.pass.recorded` read as `&& false`: code guard, record before detach in the pin pass: `!recorded` does not skip an attached member (:946-948) | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K08. `BridgeNotes::daemon`, `At::Detach`, line 1245: `Phase::Readdress => self.start_rejoins(state),` read as `Phase::Readdress => At::Remove,`: code guard, replace only after every detach: a failed readdress detach goes on to remove the bridge (:836-842) | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K09. `BridgeNotes::list`: `(self.broken.restore_held || !held.has(address))` read as `true`: code guard, restore only an unheld address (pins.rs:54-55) | `restore_held_restores_an_address_another_member_holds` |
| K10. `BridgeNotes::daemon`, `At::Start`: `Started::Netless => Some(Why::Netless),` read as `Started::Netless => None,`: code guard, gone keeps the netless 404 (stopped.rs:104-110) | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_drops_or_retries_a_netless_refusal`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K11. `BridgeNotes::operator`, line 1668: `*state.operator_stopped.get_mut(member) = true;` deleted: P1 ghost: an operator stop, hold or remove does not set operator_stopped | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it`, `current_code_starts_a_refused_member_the_operator_stopped`, `first_property_fails_for_a_member_the_operator_held`, `first_property_fails_for_a_member_the_operator_started_then_stopped`, `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal`, `fix_replays_the_failed_update`, `fix_replays_the_refusal`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K12. `BridgeNotes::stop`, `Step::Stop`, line 1047: `*state.operator_stopped.get_mut(member) = false;` deleted: P1 ghost: Ployz's stop does not clear operator_stopped | `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows` |
| K13. `BridgeNotes::daemon`, `At::Update`, line 1491: `Why::Unupdated` read as `Why::Noted`: route 3's provenance: a failed update keeps its ids as Noted, not Unupdated | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it`, `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal`, `fix_replays_the_failed_update`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| K14. `BridgeNotes::daemon`, `At::Start`: `detached: !container.endpoint.attached(),` read as `detached: false,`: P2's read: a start never reads its member as detached | `current_code_starts_a_member_pin_members_returned_before_rejoining`, `current_code_starts_a_member_whose_rejoin_failed`, `second_property_fails_for_a_detached_member_with_an_address_noted`, `second_property_fails_for_a_detached_member_with_no_address_noted` |
| K15. `BridgeNotes::stop`, `Step::Stop`: `noted: state.stopped.has(member),` read as `noted: true,`: P3's read: Ployz's stop always reads its member as noted | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds`, `stop_before_note_breaks_the_note_before_the_stop` |
| K16. `properties`, line 1866: `}) => state.pins.get(member).is_some(),` read as `}) => true,`: P4's read: a detach always reads an address as noted | `detach_unrecorded_breaks_the_address_note_before_the_detach` |

K01 is the same as deleting the fix's deferral, and matches `fix_starts_detached`; K02 is the same as deleting the Drop policy, and matches `fix_keeps_unpaid` under Drop. A side-effect failure is a test whose pinned path or expected rows the edit moved, such as `restore_held_restores_an_address_another_member_holds` under K05, whose two-member check expects every row reached.

**Making a row unreachable.** Put `false && ` inside the row's closure, before its body. The file uses these forms:

```rust
        reached: |_, state| false && took(state, Branch::Crashed),
```

```rust
        reached: |_, state| false && matches!(state.took, Some(Branch::Flipped { to: Run::Held, .. })),
```

```rust
        reached: |_, state| false && ran_loaded(state, Why::Refused),
```

```rust
        reached: |_, state| false && {
            matches!(
                state.took,
                // the row's pattern, unchanged
            )
        },
```

Do not write `reached: false && |_, state| …`: it fails to compile (E0308). Then run:

```
cd core && cargo nextest run -p ployz-verify bridge_notes --no-fail-fast
```

Each of the 57 rows was made unreachable alone this way, and each turned these tests red:

| Rows | Tests that went red |
|---|---|
| "the start finds the bridge's IPv6 shape wrong and readdresses it"; "the start finds the bridge fitting"; "the start finds no bridge and creates one"; "the readdress's listing fails, and it returns Ok"; "the readdress writes the address note before any detach"; "the readdress's address note write fails, and it detaches nothing"; "the readdress's address note lands but returns Err, and it detaches nothing"; "the readdress detaches an attached member at once"; "Docker refuses a disconnect as between restarts"; "settle answers StopAndNote"; "settle answers Stop, and the member under no restart policy is stopped without a note"; "settle answers Leave for a member that exited before stop's inspect"; "Stopped::note writes the member down, and stop_container stops it"; "Stopped::note's write fails, and the member is left alone"; "Stopped::note's write lands but returns Err, and the member is left alone"; "the disconnect after a stop detaches the member"; "the readdress replaces the bridge"; "a readdress rejoin pins the member at its address"; "a readdress rejoin fails, and the member stays detached"; "Docker refuses a connect as between restarts, and stop runs"; "the pin pass's listing fails, and it returns before finish"; "the pin pass records an attached member's address before its detach"; "the pin pass's record fails, and it skips an attached member"; "the pin pass detaches a member on a dynamic address"; "the pin pass rejoins a detached member at its noted address"; "a pin pass rejoin fails, and the member goes into unjoined"; "Pins::finish removes the address note"; "Pins::finish keeps an unjoined member's address"; "Pins::finish fails, and the old address note stays"; "Stopped::start starts a member this start stopped"; "Stopped::start finds a noted member already running"; "Stopped::start drops a member the operator removed"; "Stopped::start removes the stopped note"; "Stopped::start's update fails, and the old stopped note stays"; "a member Ployz stopped is started at a later daemon start after a start that died"; "Stopped::start starts a member the operator stopped, kept because the note's update failed"; "a refused pinned connect falls back to a dynamic address while the address note owes another"; "a daemon start dies"; "Docker's restart policy runs a held member again"; "Docker's restart policy holds a running member between restarts"; "a held member under no restart policy exits"; "the operator starts a member at rest"; "the operator stops a running member"; "the operator holds a member at rest"; "the operator removes a member" | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| "a readdress detach fails, and the bridge is not replaced" | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `restore_held_restores_an_address_another_member_holds` |
| "Docker refuses a start, and the member stays written down"; "Stopped::start rewrites the stopped note with the members Docker did not start"; "a member Docker refused is started at a later daemon start" | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| "Docker answers the netless 404, and the member stays written down"; "a member Docker refused for a missing network is started at a later daemon start" | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| "Docker attaches a detached member it starts on a dynamic address while the address note owes another" | `current_code_breaks_both_properties_and_reaches_every_row`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` |
| "Stopped::start starts a member attached on a dynamic address while the address note owes another" | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds`, `second_property_holds_for_a_member_attached_on_another_address_than_owed` |
| "the fix defers the start of a member Docker shows detached"; "the fix drops a member whose start it cannot pay" | `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal` |
| "the fix drops a member whose retries are spent"; "a member deferred for its address is started at a later daemon start" | `fix_with_retry_breaks_the_first_property_by_a_refusal` |

## Bounds and blind spots

**The model has no clock.** Nothing in this code waits on time: each step is a call, a write or another actor's move, and the bounds count events, not seconds. `extend_systemd_start_timeout` (`core/crates/ployzd/src/daemon.rs:939-964`) extends a timeout and bounds no duration; a start that runs past it is killed, which the model's crash covers.

**The bounds,** each with the state counts and the tests that go red when it drops or lifts by one, from fresh runs at this PR's head. Every lift but the two noted below kept every test green, and `scratch_rows` printed the same broken `always` properties and unreached rows as the clean head for each model.

| Bound | Value | What needs it | Dropped by one: states as is / Drop / Retry | Dropped by one: tests that went red | Lifted by one: states as is / Drop / Retry |
|---|---|---|---|---|---|
| `STARTS` | 2 | the first finding: the start whose `Stopped::start` keeps M0, and the start after the operator's stop | 995 / 930 / 968 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it`, `current_code_starts_a_member_whose_rejoin_failed`, `current_code_starts_a_refused_member_the_operator_stopped`, `first_property_fails_for_a_member_the_operator_held`, `first_property_fails_for_a_member_the_operator_started_then_stopped`, `first_property_holds_for_a_member_nobody_touched`, `first_property_holds_for_a_member_the_operator_only_started`, `first_property_holds_for_a_member_the_operator_removed`, `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal`, `fix_replays_the_failed_update`, `fix_replays_the_false_rejoin`, `fix_replays_the_refusal`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 113,452 / 83,314 / 121,180 |
| `CRASHES` | 1 | the false rejoin, and the crash-safety row | 15,683 / 11,778 / 15,996 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_whose_rejoin_failed`, `fix_replays_the_false_rejoin`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 26,355 / 21,204 / 26,912 |
| `REFUSED_STARTS` | 1 | the refusal, and the refused and netless rows | 14,845 / 15,260 / 15,746 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_refused_member_the_operator_stopped`, `first_property_fails_for_a_member_the_operator_held`, `first_property_fails_for_a_member_the_operator_started_then_stopped`, `first_property_holds_for_a_member_nobody_touched`, `first_property_holds_for_a_member_the_operator_only_started`, `first_property_holds_for_a_member_the_operator_removed`, `fix_drops_or_retries_a_netless_refusal`, `fix_drops_or_retries_a_start_docker_refuses`, `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal`, `fix_replays_the_refusal`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 27,422 / 20,847 / 27,835 |
| `FAILED_LISTINGS` | 1 | the early return | 9,949 / 7,985 / 10,074 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_pin_members_returned_before_rejoining`, `fix_replays_the_early_return`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 32,028 / 25,747 / 32,392 |
| `REFUSED_CONNECTS` | 1 | both routes of the second finding | 7,775 / 6,366 / 7,785 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_pin_members_returned_before_rejoining`, `current_code_starts_a_member_whose_rejoin_failed`, `fix_replays_the_early_return`, `fix_replays_the_false_rejoin`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 60,188 / 44,294 / 57,978 |
| `FLIPS` | 1 | the restart-policy rows | 8,274 / 6,511 / 8,466 | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 49,893 / 40,734 / 51,584 |
| `FAILED_WRITES` | 1 | route 3, and the failed-write rows | 8,968 / 7,086 / 8,987 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it`, `detach_unrecorded_breaks_the_address_note_before_the_detach`, `fix_replays_the_failed_update`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 42,178 / 34,334 / 43,594 |
| `OPERATOR_STARTS` | 1 | the refusal's operator start, and the 304 edge | 15,112 / 12,280 / 14,772 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_refused_member_the_operator_stopped`, `first_property_fails_for_a_member_the_operator_started_then_stopped`, `first_property_holds_for_a_member_the_operator_only_started`, `fix_replays_the_refusal`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 28,353 / 22,647 / 29,364 |
| `OPERATOR_STOPS` | 1 | every route of the first finding | 13,887 / 11,601 / 13,804 | `current_code_breaks_both_properties_and_reaches_every_row`, `current_code_starts_a_member_whose_note_update_failed_after_the_operator_stopped_it`, `current_code_starts_a_refused_member_the_operator_stopped`, `first_property_fails_for_a_member_the_operator_held`, `first_property_fails_for_a_member_the_operator_started_then_stopped`, `first_property_holds_for_a_member_the_operator_removed`, `fix_keeps_unpaid_breaks_the_first_property_by_a_refusal`, `fix_replays_the_failed_update`, `fix_replays_the_refusal`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on`, `restore_held_restores_an_address_another_member_holds` | 38,786 / 30,592 / 40,063 |
| `RETRIES` | 1 | Retry's keep, and its drop once the retries are spent | 25,538 / 20,445 / 20,445 | `fix_defers_an_address_then_start_debt_detached`, `fix_drops_or_retries_a_netless_refusal`, `fix_drops_or_retries_a_start_docker_refuses`, `fix_replays_the_refusal`, `fix_with_retry_breaks_the_first_property_by_a_refusal` | 25,538 / 20,445 / 26,203; red: `fix_with_retry_breaks_the_first_property_by_a_refusal` (below) |
| `hosts` | `members + 1` | a free address for every member, which the dynamic fallback and Docker's attach on start take | 17,105 / 13,583 / 17,078 | `current_code_breaks_both_properties_and_reaches_every_row`, `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows`, `fix_with_retry_breaks_the_first_property_by_a_refusal`, `netless_gone_never_starts_a_member_refused_for_its_network`, `replace_after_failed_detach_removes_a_bridge_a_member_is_on` | 33,953 / 27,235 / 35,124; red: `restore_held_restores_an_address_another_member_holds` (below) |

Each drop or lift edits one constant at the top of the file (`const STARTS: u8 = 2;` to `1` or `3`, and so on). The address pool is computed: in `fn hosts`, `.get(..usize::from(self.members) + 1)` becomes `+ 0` for the drop and `+ 2` for the lift. `members` is 1, and 2 only in the tests `restore_held` needs (`restore_held_restores_an_address_another_member_holds`, `fix_handles_an_address_then_start_debt_whose_address_another_member_holds`).

Two lifts turn a test red, by its bounds, not by a new break:
- `RETRIES` to 2: under Retry, "the fix drops a member whose start it cannot pay" and "the fix drops a member whose retries are spent" go unreached, and `fix_with_retry_breaks_the_first_property_by_a_refusal`, which lists the unreached rows, goes red. A member must be refused or deferred `RETRIES + 1` times to be dropped. With `STARTS` at 3 the rows stay unreached; with `STARTS` at 3 and `REFUSED_STARTS` at 3 every test passes and the table is Retry(1)'s.
- The pool to `members + 2`: `restore_held_restores_an_address_another_member_holds` panics with "one more host than members", since `HOSTS` holds three addresses. `fix_handles_an_address_then_start_debt_whose_address_another_member_holds` steps a hand-built state and never reads the pool.

**A scratch row lister.** It runs each model's checker once and prints every broken `always` and every unreached row. Put it at the end of `mod tests`, before the file's last closing brace:

```rust
    /// Scratch: each model's broken `always` properties and unreached rows.
    #[test]
    fn scratch_rows() {
        for model in [as_is(), fixed(OnRefusal::Drop), fixed(OnRefusal::Retry)] {
            let checker = check(model);
            println!("ROWS {:?}: {} states", model.fix, checker.unique_state_count());
            for property in [
                STARTS_ONLY_OWED,
                STARTS_ONLY_ON_BRIDGE,
                NOTED_BEFORE_STOP,
                ADDRESS_NOTED_BEFORE_DETACH,
            ] {
                if checker.discovery(property).is_some() {
                    println!("ROWS   broken: {property}");
                }
            }
            for name in unreached(model, &checker) {
                println!("ROWS   unreached: {name}");
            }
        }
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=bridge_notes::tests::scratch_rows)' --no-capture
```

On the clean head it prints:

```
ROWS None: 25538 states
ROWS   broken: Stopped::start starts only a container Ployz stopped and nobody stopped since
ROWS   broken: Stopped::start starts a member only while Docker shows it on the bridge
ROWS Some(Drop): 20445 states
ROWS   broken: Stopped::start starts only a container Ployz stopped and nobody stopped since
ROWS   unreached: Docker refuses a start, and the member stays written down
ROWS   unreached: Docker answers the netless 404, and the member stays written down
ROWS   unreached: Stopped::start rewrites the stopped note with the members Docker did not start
ROWS   unreached: a member Docker refused is started at a later daemon start
ROWS   unreached: a member Docker refused for a missing network is started at a later daemon start
ROWS   unreached: Docker attaches a detached member it starts on a dynamic address while the address note owes another
ROWS   unreached: the fix drops a member whose retries are spent
ROWS   unreached: a member deferred for its address is started at a later daemon start
ROWS Some(Retry): 26153 states
ROWS   broken: Stopped::start starts only a container Ployz stopped and nobody stopped since
ROWS   unreached: Docker attaches a detached member it starts on a dynamic address while the address note owes another
```

**Code-as-is facts, and what the model does with each.**
- **A failed note write**: modelled, `FAILED_WRITES`, for `Stopped::note` (the member is left alone, `:1044-1047`, `stopped.rs:49-53`), `Pins::record` (`:819-822`, `:946-948`), `Pins::finish` and `Stopped::start`'s update (the old file stays, `pins.rs:94-99`, `stopped.rs:88-93`). `Stopped::note` and `Pins::record` are each also offered as a write that returns Err after its rename landed (`Pick::AfterRename`, `core/crates/ployzd/src/filesystem.rs:24-25`). `finish` and the update are not: each returns nothing and only prints "could not update {path}: {error}" on Err (`pins.rs:94-99`, `stopped.rs:88-93`), so an Err after the rename leaves the files and the run a success leaves. Their removals use `remove_file`, with no rename.
- **Docker refuses a start**: modelled, a 500 or the netless 404 (`REFUSED_STARTS`). "No such container" drops the id: modelled, as the operator's remove.
- **Docker starts a detached member**: modelled both ways, above. Docker 29.1.3 runs it with no network; other versions are unchecked.
- **A failed listing**, in the readdress and in the pin pass, each returning its own way: modelled, `FAILED_LISTINGS`.
- **Docker refuses a connect**, then answers `connect(None)` either way: modelled, `REFUSED_CONNECTS`.
- **`settle`'s three answers**: modelled through `Policy` and the run state. A running member under no restart policy is stopped and never noted (`:1185`).
- **`ensure_docker_network_shape` returning Err** (`:712`). The `?` skips the pin pass and `Stopped::start`, and `NetworkPlane::start`'s Err fails the daemon start (`core/crates/ployzd/src/daemon.rs:274`). Not modelled; each Err reaches the states of a crash at that step. An inspect that fails with anything but a 404 (`:779`), and a found bridge that is `Stale` and not removed, `Attached` or `Unowned` (`:743-776`), return before any write. Docker refusing the bridge in both shapes (`:880-889`), via `:781` or the readdress's `:739`, leaves no bridge, the state of a crash just after `At::Remove`; the readdress's rejoins before it return (`:846-849`), and with no bridge they connect nothing.

**Left out**, with nothing in the model that stands for it:
- **The owed address as a property.** The fallback's success is modelled, and so is `finish` clearing the note for a member it rejoined (`pins.rs:83`), so a member attached on a dynamic address while the note owes another is reached as a row, "Stopped::start starts a member attached on a dynamic address while the address note owes another". No property says the owed address is kept.
- A `docker` command someone runs on the Machine outside Ployz.
- Docker's own restart of its containers when the Docker daemon restarts.
- A corrupt or unreadable note (`stopped.rs:27-43`, `pins.rs:26-42`).
- `complete_reset`, which removes every `data_dir` file but the state, lock and prepare entries (`core/crates/ployzd/src/machine/store.rs:737-749`).
- Any second Machine.

A green model says nothing about them. Cover the code with:

```
cd core && cargo nextest run -p ployzd network::linux
```

Expected: `35 tests run: 35 passed, 1784 skipped`. From an empty `core/target` the build took 3m 26s here and the run 0.051 s; reviewers saw about 5 and 10 minutes for the whole command, mostly the `ployzd` build.

## Keep the model in step

When a change alters what a daemon start writes to either note, when it stops, detaches or starts a member, or what the operator's steps do to a member the notes name, change the model in the same PR.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `ensure_docker_network` and `ensure_docker_network_shape`: the `At::Load`, `At::Shape` and `At::Create` arms of `BridgeNotes::daemon`;
   - `readdress_docker_network`, `pin_members`, `detach`, `rejoin` and `rejoin_with`: the `At::List`, `At::Record`, `At::Detach`, `At::Remove`, `At::Rejoin` and `At::Finish` arms, `disconnect`, `connect`, `next_detach`, `next_pin` and `start_rejoins`;
   - `stop` and `settle`: `BridgeNotes::stop` and `settle`;
   - `Pins::restore`, `record` and `finish`: `list`, `recorded`, and the `At::Finish` arm;
   - `Stopped::start` and `gone`: the `At::Start` and `At::Update` arms;
   - `ContainerStep::Start`, `Stop`, `Hold`: `operator`.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps, or pinning what it changes when no property needs it.
3. If the change ships the fix (E-125), model it as the code writes it and make it the default. Then turn the finding tests into a `Broken` flag that restores the old code, keep the tests, since their printed paths are the record of what the fix keeps out, and turn `fix_with_drop_breaks_only_route_3_and_gives_up_the_named_rows` (or the policy E-125 picks) into the holds test.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires what its name says.
5. Every `State` field is read: `bridge`, `docker`, `stopped`, `why` (by the rows and the route 3 filter), `retries` (by Retry), `pins`, `process`, `pass`, `starts`, `used`, `operator_stopped` (by `STARTS_ONLY_OWED`) and `took` (by the properties and rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/network/linux.rs`: `ensure_docker_network`, `ensure_docker_network_shape`, `readdress_docker_network` and its doc comment, `replace_docker_network`, `create_bridge`, `bridge_members`, `pin_members`, `detach`, `stop` and its doc comment, `Settle` and `settle`, `rejoin`, `rejoin_with`, `addresses`, `between_restarts`.
- `core/crates/ployzd/src/network/linux/stopped.rs`: the module doc, `Stopped::load`, `note` and its doc comment, `start`, `gone`.
- `core/crates/ployzd/src/network/linux/pins.rs`: `Pins::load`, `restore`, `record` and its doc comment, `finish`.
- `core/crates/ployzd/src/docker/bridge_address.rs`: `next`.
- `core/crates/ployzd/src/machine/local_machine/container.rs`: `ContainerStep::Start`, `Stop` and `Hold`.
- `core/crates/ployzd/src/docker/lifecycle.rs`: `stop`, `stop_if` and `idempotent_lifecycle_result`.
- `core/crates/ployzd/src/daemon.rs`: `NetworkPlane::start`'s place before the Machine API.

Sync commit: `9bc50a3ce7bfb2295f1ae704ff3b7fa8a68b2d43`, the `main` this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
