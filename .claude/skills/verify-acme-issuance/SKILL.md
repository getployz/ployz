---
name: verify-acme-issuance
description: Model-check when each Machine's certificate loop pushes an Ingress Hostname with no Certificate Material to to_order, the rank delay that staggers Machines, against the shared backoff clock in the replicated certificate row, each Machine's own Hostname Verdict, a Certificate Policy that is refused or has no directory, an Err before the gate, failed orders, lost records and daemon restarts, with the Stateright model in core/crates/ployz-verify/src/acme_issuance.rs. Use when a change touches core/crates/ployzd/src/certificates.rs (run, issue_wanted, obtain, first_seen, action_due, issuance_action, poll_wait, rank_delay, issuance_step, machine_rank, RANK_STEP), core/crates/ployz-core/src/domain/issuance.rs (issuance_gate, issuance_failure_clock, issuance_backoff, IssuanceClock), the ACME writers in core/crates/ployzd/src/corrosion/store.rs (acme_row, publish_certificate, publish_certificate_challenge, record_certificate_failure, record_certificate_error), or CertificateRow's issued, with_backoff, with_error and with_challenge in core/crates/ployzd/src/corrosion/certificate.rs.
---

# Verify ACME issuance scheduling

The model in `core/crates/ployz-verify/src/acme_issuance.rs` explores when each Machine's certificate loop pushes one Ingress Hostname with no Certificate Material to `to_order`. A bare `:n` below is a line of `core/crates/ployzd/src/certificates.rs`. Every other path is written in full from the repo root, and every command runs from the repo root.

## What the model copies

**What each Machine hosts.** Each daemon runs one certificate loop, `run` (`:359-415`), once it participates (`core/crates/ployzd/src/daemon.rs:494-507`). The loop keeps `first_seen`, a map from hostname to the instant its pass first saw the hostname with no material (`:367`, `:480-485`), and runs `issue_wanted` (`:424-603`) once per wake. A pass that pushed the hostname awaits `account` (`:539`) and then `join_all` over `obtain` (`:540-551`), so the same Machine passes again only once its obtains end. A daemon restart builds an empty `first_seen` (`:367`) and ends an obtain in flight.

**The pass**, `Action::Pass` and `AcmeIssuance::pass`, copies `:431-536` for one hostname:
- the policy refusal (`:436-445`): every Machine calls `record_certificate_error` for every wanted hostname (`:439-443`), then returns before `first_seen.retain` and the gate;
- no directory (`:447-449`): it returns before them too;
- the read (`:450-451`) and `first_seen.retain` (`:453`), which keeps the one wanted hostname;
- the rank (`:465-468`), `now` and `wall` (`:469-470`);
- the elapsed match (`:474-488`): material present removes `first_seen` (`:476`), and otherwise the entry is inserted only when absent (`:480-485`);
- `issuance_action` (`:489`), then the skip when material is present and nothing is due (`:490-494`);
- the probe and verdict (`:495-501`) and `issuance_gate` (`:502-508`);
- Nothing: `continue` (`:510`). Refuse: `first_seen.remove` (`:512`), and only `rank == 0` records the refusal with `record_certificate_failure` (`:513-528`);
- Order: the push when `action_due` answers Order (`:534-536`).

**An Err before the gate**, `Action::Errs`. A store read (`:432`, `:437-438`, `:450-451`, `:457`) or `probe_client` (`:464`) can return Err. `issue_wanted` then returns it through `?`, `run` prints "failed to obtain certificates: …" and waits `RETRY_INTERVAL`, 60 s (`:394-400`), and `first_seen` keeps its entry.

**The obtain**, `Action::Obtain` and `AcmeIssuance::obtain`, is one step: `obtain` (`:614-658`) and the failure record after `join_all` (`:552-578`). Its outcomes:
- the re-read (`:623`) holds material, and the recheck returns (`:624-635`);
- the order is issued, and `publish_certificate` writes `CertificateRow::issued`, material with no challenge and no refusal (`:654-656`, `core/crates/ployzd/src/corrosion/store.rs:851-865`, `core/crates/ployzd/src/corrosion/certificate.rs:255-259`);
- the order fails. `issue_wanted` prints "failed to obtain certificate for …" (`:554-557`). `issuance_failure_clock` counts from the pass's snapshot clock (`:559`) and the pass's `wall` (`:561`), and `record_certificate_failure` gets the snapshot's material (`:568-569`), which in the model is none. Its no-material arm writes nothing when the row it reads holds material, and otherwise upserts `with_backoff` (`core/crates/ployzd/src/corrosion/store.rs:987-994`), which keeps the row's material and challenge and replaces only the refusal (`core/crates/ployzd/src/corrosion/certificate.rs:279-291`, `:364-377` of that file);
- the failure record returns Err, which is only logged (`:574-578`), and nothing is written.

**The real functions it calls.** `issuance_gate`, `issuance_failure_clock`, `issuance_backoff`, `IssuanceClock::new` and its accessors (`core/crates/ployz-core/src/domain/issuance.rs:14-125`), and `CertificatePolicy::built_in(None)`'s `backoff_base`, `backoff_cap` and `probe_timeout` (`core/crates/ployz-core/src/domain/certificate_policy.rs:93-103`).

**Hand copies, each citing its line.** `CHALLENGE_WAIT` (`:40`), `RETRY_INTERVAL` (`:42`), `RANK_STEP` (`:44-45`), `machine_rank` (`:54-61`), `rank_delay` (`:79-81`), `issuance_step` (`:83-85`), `action_due` (`:87-122`) and `poll_wait` (`:195-207`). `ployz-verify`'s only Ployz dev-dependency is `ployz-core` (`core/crates/ployz-verify/Cargo.toml`). `action_due`'s `None` arm (`:97-105`) is copied. Its `Some(material)` arm is renewal (`:106-120`), which the model leaves out: with material present the model answers `(Nothing, RETRY_INTERVAL)`, the arm's answer when `renew_at` cannot read the certificate (`:119`).

**Model-local types.** `IssuanceClock`, `IssuanceFailure`, `Refusal`, `HostnameVerdict` and `ClusterRoute` derive no `Hash` (`core/crates/ployz-core/src/domain/issuance.rs:8`, `:15` of that file; `core/crates/ployz-core/src/domain/hostname_verdict.rs:27`, `:37`, `:46` of that file), and Stateright needs `State: Hash`. So the state holds `Clock`, `Failure`, `Verdict`, `Recorded` and `Row`, and each call rebuilds the real value (`Clock::real`, `Verdict::real`, `Failure::real`) and reads the answer back (`Clock::of`, `Failure::of`).

**The row.** `Row` is the hostname's `CertificateRow::Acme` (`core/crates/ployzd/src/corrosion/certificate.rs:221-233`): whether it holds material, and its refusal, a reason and an optional clock (`RecordedRefusal`, `core/crates/ployzd/src/corrosion/certificate.rs:214-219`). Its challenge is left out, because no step reads it: the model's `obtain` is atomic, so no other step can see a challenge it published. `Reason` keeps only which reason a refusal holds, because only `record_certificate_error` compares it (`core/crates/ployzd/src/corrosion/store.rs:1028-1030`).

## The finding

`first_seen` keeps aging through a hold of the shared clock, and through a pass that returns before the gate. So once the gate opens, or the early return clears, rank 1 can push the hostname in the same tick as rank 0, though it waited no rank delay since its last blocked pass. The checker finds four routes, each with its own test. Each story, and the PR's diagram, shows one possible schedule: the real loops pass in either order within a step.

- **The authority hold** (`current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`). At 0 s rank 0's pass pushes the hostname, and its order fails. The record writes an Authority clock with one failure and `next_attempt_at` 60 s after the pass's `wall` (`DEFAULT_BACKOFF_BASE`, `core/crates/ployz-core/src/domain/certificate_policy.rs:20-21`). Rank 1's pass inserts `first_seen` (`:483`), and the gate holds it (`:510`). At 30 s both Machines are held, and rank 1 is now Order-due. At 60 s the clock has passed, and both push the hostname.
- **The verdict change** (`current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`). Both probes find the hostname does not resolve. Rank 0 refuses and records a DoesNotResolve clock, and rank 1's pass inserts `first_seen` and is held. At 30 s both probes reach the Cluster. That verdict differs from the clock's refusal, so `issuance_gate` opens with 30 s still on the clock (`core/crates/ployz-core/src/domain/issuance.rs:76-83`), and both push the hostname.
- **The policy refusal** (`current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`). Rank 0 is on time. At 0 s rank 1's pass inserts `first_seen` and waits out its rank delay. The operator sets a Certificate Policy that `resolve_certificate_policy` refuses, and rank 0's first pass, also at 0 s, records the policy's reason and returns before the gate; `run` sleeps 60 s (`:444`). At 30 s rank 1's pass returns the same way. The operator repairs the policy. At 60 s rank 0's sleep ends and it pushes the hostname. A container change wakes rank 1's loop at that moment (`:370`, `:402-409`), and rank 1 pushes too. The refusal held rank 0 back as long as rank 1, so neither Machine's delay separates them.
- **An Err before the gate** (`current_code_lets_rank_1_order_with_rank_0_after_an_error_before_the_gate`). It is the policy refusal's schedule, with a call before the gate returning Err in place of each refusal, rank 0's at 0 s and rank 1's at 30 s; `run` waits `RETRY_INTERVAL`, 60 s (`:394-400`).

A rank 0 whose first pass comes late pushes with rank 1 on the fix too, with no early return at all: that is risk 2's family (What the property is not, below), not this finding.

Each test replays its schedule with `follow`, asserts each branch, and then asks the checker for its first path to the same broken state (`breaks_where`). The checker keeps one path to each state, the first in BFS order, and a shorter path can reach the same state, so the replay shows the route's own schedule, and the second check shows the checker discovers its end state. Each pass in a replay comes when its Machine's requested sleep ends, which its story row prints, except rank 1's last pass in the policy refusal and the Err, which a container change wakes.

**The timing condition.** Rank 1's `first_seen` must be at least one `issuance_step` old at its first pass after the gate opens. While rank 1 is Order-due and held, `poll_wait` asks for a 30 s sleep, `RANK_STEP` (`:204`). That is a requested sleep, not a cadence: a pass takes time, store outages and probes delay the next check, and a container change wakes the loop early (`:402-409`). The replays meet at the opening, which is one possible schedule when passes are short. Meeting there is not needed: in `a_late_recheck_still_lets_rank_1_order_with_rank_0` both Machines recheck 60 s after the gate opens, and rank 1 still pushes with rank 0. It recurs on every hold, since a hold leaves `first_seen` in place.

Four lines remove the entry: a restart (`:367`), the hostname leaving the wanted set (`:453`), material present (`:476`) and a refusal (`:512`). A removal ends the route only when no held pass inserts `first_seen` again at least one issuance step before the gate opens (`:480-485`). Early in a hold it does not: in `an_early_restart_does_not_end_the_route`, rank 1's daemon restarts right after its first held pass, its next held pass inserts `first_seen` again, and rank 1 still pushes with rank 0 when the gate opens. `a_restart_restarts_the_rank_delay` restarts rank 1 after the gate opens, so it covers only the late case.

**The property** is `RANK_DELAY_KEPT`, an `always`: "a Machine of rank r pushes the hostname to to_order only r issuance steps or more after its first pass since its last blocked pass". A blocked pass is one whose gate answered Nothing or Refuse, or one that returned before the gate: the policy refusal, no directory, an Err before the gate, or material present. A Machine's first pass ever counts as unblocked. `Machine::unblocked` holds the ticks since that pass, and `Branch::Pushed { early }` compares them with the rank.

It is justified from the code. The doc comment on `RANK_STEP`, in full (`:44`):

> Must cover `CHALLENGE_WAIT` so rank 1 cannot write a competing token while rank 0 is still probing.

`issuance_action`'s doc comment, in full (`:63-66`):

> Whether this Machine should order or renew now.
>
> Rank delay uses `RANK_STEP.max(policy.probe_timeout())` so a later rank cannot start a competing order while an earlier rank is still presenting.

`:512` already restarts the wait after a refusal: `first_seen.remove(hostname);`. A hold does not: `IssuanceGate::Nothing => continue` (`:510`). The property keeps one part of those comments, the delay itself: each Machine waits its rank delay, measured from its own first pass since it was last blocked. It says nothing about when rank 0 starts presenting.

**What the property is not.** The comments promise something across Machines: a later rank does not start a competing order while an earlier one is presenting. The property is per Machine, and each Machine measures its delay from its own first pass. With the built-in probe timeout, the delay equals the probe window exactly (`:45`, `:83-85`, and `wait_for_http01`'s deadline at `:879`). Rank 0's probe starts only after its account, `new_order` and challenge publish (`order_certificate`, `:676-680`, `:705`; `obtain`'s `present`, `:641-650`). So the cross-Machine promise can fail even with no hold, by however long those steps take. That is the model section's risk 2, and it is out of scope. A cross-Machine property, such as "rank 1 never pushes in the same tick as rank 0", would fail on the fix too: the model lets either Machine skip a tick.

## Run it

```
cd core && cargo nextest run -p ployz-verify acme_issuance --no-capture
```

Expected: the summary line ends with `19 tests run: 19 passed, 133 skipped`. `current_code_breaks_only_the_rank_delay` prints `as is: 370920 states`, `the_fix_keeps_the_rank_delay` prints `fixed: 205372 states`, and `only_rank_zero_records_a_refusal` prints `fixed, any rank records: 238600 states`. Each story prints under its test's name. `--no-capture` runs the tests one at a time, so a warm run takes about 12 to 20 s of nextest time; without it, about 4 s. CI runs `ployz-verify` in its workspace nextest run.

Read the tests in four groups.
- **The finding, on the code as it is.** The four route tests above; `a_late_recheck_still_lets_rank_1_order_with_rank_0` and `an_early_restart_does_not_end_the_route`, which pin the timing condition; and `current_code_breaks_only_the_rank_delay`, which expects `RANK_DELAY_KEPT` to break, `HOLD_KEPT` and `NO_ORDER_OVER_MATERIAL` to hold, and every `COVERAGE` row that is not `Reach::Fix` to be reached. It also prints the checker's first broken path: rank 1's pass waits out its rank delay, a container change wakes its loop at once and a call before the gate returns Err, and a tick later rank 1 pushes the hostname, measured from its pass after the Err.
- **The code's guards.**
  - `the_gate_holds_while_the_clock_holds` sets `hold_orders` and expects `HOLD_KEPT` to break. It then keeps to a path whose last pass's probe finds the hostname does not resolve: a held Refused verdict has no route to order along, so the break orders along Direct, the model's only route.
  - `obtain_rechecks_material` sets `obtain_skips_recheck` and expects `NO_ORDER_OVER_MATERIAL` to break.
  - `a_refusal_restarts_the_rank_delay` sets `refusal_keeps_first_seen` on the fix: rank 1 refuses with no clock recorded yet, its verdict changes, and it is Order-due at once.
  - `only_rank_zero_records_a_refusal` sets `any_rank_records` on the fix (The fix, below).
  - `a_restart_restarts_the_rank_delay`, `an_order_s_clock_counts_from_its_pass` and `a_tick_is_one_issuance_step` pin the late restart, the failure clock's `wall` and the tick (Bounds).
- **The fix.**
  - `the_fix_keeps_the_rank_delay` runs `fixed()` with `assert_properties`: every `always` holds, and every row is reached.
  - `the_fix_waits_on_each_finding_s_schedule` replays each route's schedule under the fix: rank 1's last pass inserts `first_seen` and waits, and a tick later it pushes the hostname.
  - `the_fix_on_each_edge_state` steps one pass of rank 1 from each state the fix's predicate must handle, under the fix and as the code is: no clock, an Authority clock not yet due, an unchanged DoesNotResolve clock not yet due, a clock that is due, a DoesNotResolve clock whose verdict changed, and material present, where the pass skips at `:490-494`. It then steps rank 0 and rank 1 from a state where neither has `first_seen`: rank 0 owes no delay and pushes, and rank 1 waits.
- **The fix's parts.** `the_fix_needs_its_hold_reset` and `the_fix_needs_its_early_return_reset` each set one flag on `fixed_without(..)`.

## The fix

The fix is the shape of a later PR, E-46. No product code has it yet. It changes only memory a pass keeps, `first_seen`, and keeps the replicated row and the routes as they are.

```text
a pass whose gate answers Nothing      drops first_seen, as a refused pass does (:512)
a pass that returns before the gate    drops first_seen, with Ok or Err
```

| Part | What it keeps | Flag | Test, and the `always` property that breaks without it |
|---|---|---|---|
| The drop on a held pass | The authority hold and the verdict change: rank 1's next pass after the hold inserts `first_seen` and waits its rank delay | `fix_hold_keeps_first_seen` | `the_fix_needs_its_hold_reset`, `RANK_DELAY_KEPT` |
| The drop on a pass that returns before the gate | The policy refusal, no directory and an Err before the gate | `fix_early_return_keeps_first_seen` | `the_fix_needs_its_early_return_reset`, `RANK_DELAY_KEPT`, by replays of the policy refusal, no directory and the Err |

The second part exists because the policy refusal and the Err are code paths that return before the gate (`:436-449`, `:394-400`). With rank 0 on time, the same early return holds rank 0 back too, and once it clears both Machines push in the same tick (`THE_POLICY_REFUSAL`, `THE_ERROR`). The drop makes rank 1's next pass wait its rank delay (`the_fix_waits_on_each_finding_s_schedule`). That is the harm the reset prevents, so E-46 must cover these routes too.

What the fix keeps as it is:
- **Only rank 0 records a refusal** (`:513`). No `always` property in this model needs that restriction: with `any_rank_records` on the fix, every `always` still holds. What changes is that the row "rank 1 refuses and records nothing, and the row holds no reason" goes unreached, and every other row is still reached. So with rank 0 running no loop, no Machine records the refusal. `redact_certificate` then reports Unknown for a row with no material, no challenge, no clock and no reason (`core/crates/ployzd/src/runtime_watch.rs:422-423`).
- **Renewals.** The fix does not touch the renewal arm (Bounds).
- **Risk 2.** The cross-Machine promise above.

## Read a counterexample

A story is a numbered list of steps cut at the last state of a path. Each row is one step at the time it ran, `[N s]` from the start, named for who takes it: Rank 0, Rank 1, Clock (a tick), DNS (a probe change) or Operator. A bare `:n` in a row is a line of `core/crates/ployzd/src/certificates.rs`. The line under each row is the state after the step:
- `row` is the replicated row: material or none, then its reason and clock, or no reason;
- `policy` is what `resolve_certificate_policy` makes of the Certificate Policy;
- each Machine's `probe` is what its next probe sees. `first_seen` and `unblocked` are ages, "or more" where the age is capped, and `obtain in flight` gives how long ago its pass ran.

A row that skips a guard says "(under Broken)" when a flag is set. A story that starts mid-path starts with "from the state above". `follow` prints "a schedule" where `breaks` prints the property.

These are the last three rows of `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`'s replay:

```
 7. [60 s] Clock: 30 s pass
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, first_seen 30 s or more, no obtain, unblocked none | Rank 1: probe Direct, first_seen 30 s or more, no obtain, unblocked none
 8. [60 s] Rank 0: pass: first_seen is 30 s or more old (:480-486), and action_due answers Order (:489, :95-104); its probe sees the hostname reach the Cluster directly (:495-501); issuance_gate answers Order, as the clock has passed (core/crates/ployz-core/src/domain/issuance.rs:76-85); the hostname is pushed to to_order (:534-536), and account and obtain are next (:538-551)
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, first_seen 30 s or more, obtain in flight from a pass 0 s ago, unblocked 0 s | Rank 1: probe Direct, first_seen 30 s or more, no obtain, unblocked none
 9. [60 s] Rank 1: pass: first_seen is 30 s or more old (:480-486), and action_due answers Order (:489, :95-104); its probe sees the hostname reach the Cluster directly (:495-501); issuance_gate answers Order, as the clock has passed (core/crates/ployz-core/src/domain/issuance.rs:76-85); the hostname is pushed to to_order (:534-536), less than its rank delay after its first pass since its last blocked pass, and account and obtain are next (:538-551)
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, first_seen 30 s or more, obtain in flight from a pass 0 s ago, unblocked 0 s | Rank 1: probe Direct, first_seen 30 s or more, obtain in flight from a pass 0 s ago, unblocked 0 s
```

Some tests print a story before they fail, and some print none. To see the story of a property that breaks, put this test at the end of `mod tests`, before the file's last closing brace, with the model and property you want:

```rust
    #[test]
    fn scratch_story() {
        let model = fixed_without(Broken { fix_hold_keeps_first_seen: true, ..Broken::default() });
        breaks(model, RANK_DELAY_KEPT);
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=acme_issuance::tests::scratch_story)' --no-capture
```

`breaks` prints the first path in BFS order whose last state breaks the property, and panics with "a path breaks the property" when none does. For a `sometimes`, such as a `COVERAGE` row's name, it prints the first path that reaches the row. Delete the test before you commit.

The story's words come from `reads_as`, which prints the statement each step copies. An edit to a model step needs `reads_as` edited with it, or its rows go stale.

## Real bug or model error

A counterexample is a lead, not a verdict. Before calling it a bug, confirm each step of the story against the line it cites. For each Machine's step, `reads_as` prints the lines it copies.

- If a step does something the code cannot, fix the model and say so. The usual suspects:
  - `actions`, which decides what is offered in each state;
  - `AcmeIssuance::pass`, `obtain`, `early_return`, `tick` and `restart`;
  - `record_failure`, which copies `record_certificate_failure`'s no-material arm;
  - `Clock::holds` and `Gate::of`, the model's own reading of the gate.
- If every step matches the code, it is a finding. Keep the property and the model as they are, since weakening either to go green hides the bug.

## Breaking a guard yourself

Each `Broken` flag is already set by its own test, so setting one in a test of your own changes no other test and turns none red. To check the flag table below, run `scratch_flag_kinds`, after it. To break a guard in a new test, set its flag through `as_is_without` or `fixed_without`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every red test is listed.

A failure prints the property's full string, not its constant's name:
- `RANK_DELAY_KEPT`: "a Machine of rank r pushes the hostname to to_order only r issuance steps or more after its first pass since its last blocked pass";
- `HOLD_KEPT`: "no pass pushes the hostname to to_order while its snapshot's clock holds and its verdict leaves the clock standing";
- `NO_ORDER_OVER_MATERIAL`: "no obtain orders while the row it re-reads holds material that is not due for renewal".

A `COVERAGE` row prints its `name`.

Which kind of property each flag breaks, from a scratch run that checked each flagged model in full. It lists the `always` properties that break on the flagged model but not on its unflagged one, and the rows the unflagged model reaches but the flagged one does not. On the code as it is, `RANK_DELAY_KEPT` breaks with or without a flag.

| Flag | `always` properties that break | `sometimes` rows left unreached |
|---|---|---|
| `hold_orders`, on the code as it is | `HOLD_KEPT` | "the gate holds a Machine still in its rank delay" and "the gate holds an Order-due Machine" |
| `refusal_keeps_first_seen`, on the fix | `RANK_DELAY_KEPT` | none |
| `any_rank_records`, on the fix | none | "rank 1 refuses and records nothing, and the row holds no reason" |
| `obtain_skips_recheck`, on the code as it is | `NO_ORDER_OVER_MATERIAL` | "obtain's recheck finds material and returns" |
| `fix_hold_keeps_first_seen` | `RANK_DELAY_KEPT` | "the fix drops first_seen on a held pass" |
| `fix_early_return_keeps_first_seen` | `RANK_DELAY_KEPT` | "the fix drops first_seen on a pass that returns before the gate" |

The scratch test that printed it follows. Put it at the end of `mod tests`, before the file's last closing brace, and run it alone; it takes about 9 to 12 s of nextest time in release.

```rust
    #[test]
    fn scratch_flag_kinds() {
        let runs = [
            ("hold_orders on as_is", as_is(), Broken { hold_orders: true, ..Broken::default() }),
            ("refusal_keeps_first_seen on fixed", fixed(), Broken { refusal_keeps_first_seen: true, ..Broken::default() }),
            ("any_rank_records on fixed", fixed(), Broken { any_rank_records: true, ..Broken::default() }),
            ("obtain_skips_recheck on as_is", as_is(), Broken { obtain_skips_recheck: true, ..Broken::default() }),
            ("fix_hold_keeps_first_seen on fixed", fixed(), Broken { fix_hold_keeps_first_seen: true, ..Broken::default() }),
            ("fix_early_return_keeps_first_seen on fixed", fixed(), Broken { fix_early_return_keeps_first_seen: true, ..Broken::default() }),
        ];
        for (name, base, broken) in runs {
            let baseline = check(base);
            let model = AcmeIssuance { broken, ..base };
            let checker = check(model);
            println!("{name}: {} states", checker.unique_state_count());
            for property in model.properties() {
                let found = checker.discovery(property.name).is_some();
                match property.expectation {
                    Expectation::Always if found && baseline.discovery(property.name).is_none() => {
                        println!("  always breaks: {}", property.name);
                    }
                    Expectation::Sometimes if !found && baseline.discovery(property.name).is_some() => {
                        println!("  sometimes unreached: {}", property.name);
                    }
                    _ => {}
                }
            }
        }
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=acme_issuance::tests::scratch_flag_kinds)' --no-capture --release
```

An edit to the model's code changes every model at once. For this PR, each edit below was made alone in the model at this PR's head, run with `cd core && cargo nextest run -p ployz-verify acme_issuance --no-fail-fast`, and reverted.

| Edit, by function | Tests that went red |
|---|---|
| K01. `AcmeIssuance::pass`: `IssuanceGate::Nothing if self.broken.hold_orders =>` read as `IssuanceGate::Nothing if true =>`, so every model's held pass orders along Direct | `a_late_recheck_still_lets_rank_1_order_with_rank_0`, `a_restart_restarts_the_rank_delay`, `an_early_restart_does_not_end_the_route`, `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_hold_reset`, `the_fix_on_each_edge_state`, `the_fix_waits_on_each_finding_s_schedule` |
| K02. `AcmeIssuance::pass`: `if !self.broken.refusal_keeps_first_seen {` read as `if false {`, so no refusal removes `first_seen` | `a_refusal_restarts_the_rank_delay`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_hold_reset` |
| K03. `AcmeIssuance::pass`: `if rank == 0 \|\| self.broken.any_rank_records {` read as `if true {`, so every rank records its refusal | `a_refusal_restarts_the_rank_delay`, `current_code_breaks_only_the_rank_delay`, `the_fix_keeps_the_rank_delay` |
| K04. `AcmeIssuance::obtain`: `let rechecks = reread.material && !self.broken.obtain_skips_recheck;` replaced by `let rechecks = false;` | `current_code_breaks_only_the_rank_delay`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay` |
| K05. `resets_on_hold`: its body `self.fix && !self.broken.fix_hold_keeps_first_seen` replaced by `self.fix`, so the flag does nothing | `the_fix_needs_its_hold_reset` |
| K06. `resets_on_hold`: its body replaced by `false`, so the fix keeps `first_seen` on a held pass | `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_on_each_edge_state`, `the_fix_waits_on_each_finding_s_schedule` |
| K07. `resets_on_hold`: its body replaced by `true`, so the code as it is drops `first_seen` on a held pass too | `a_late_recheck_still_lets_rank_1_order_with_rank_0`, `a_restart_restarts_the_rank_delay`, `an_early_restart_does_not_end_the_route`, `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `the_fix_needs_its_hold_reset`, `the_fix_on_each_edge_state` |
| K08. `resets_on_early_return`: its body `self.fix && !self.broken.fix_early_return_keeps_first_seen` replaced by `self.fix`, so the flag does nothing | `the_fix_needs_its_early_return_reset` |
| K09. `resets_on_early_return`: its body replaced by `false`, so the fix keeps `first_seen` on a pass that returns before the gate | `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_hold_reset`, `the_fix_waits_on_each_finding_s_schedule` |
| K10. `AcmeIssuance::pass`: `early: usize::from(since) < rank,` replaced by `early: false,`, so `RANK_DELAY_KEPT` never fires | `a_late_recheck_still_lets_rank_1_order_with_rank_0`, `a_refusal_restarts_the_rank_delay`, `an_early_restart_does_not_end_the_route`, `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`, `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`, `current_code_lets_rank_1_order_with_rank_0_after_an_error_before_the_gate`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `the_fix_needs_its_early_return_reset`, `the_fix_needs_its_hold_reset` |
| K11. `AcmeIssuance::obtain`: `let wall = now() - tick() * u32::from(order.ago);` replaced by `let wall = now();`, so the failure clock counts from the order's end | `an_order_s_clock_counts_from_its_pass`, `current_code_breaks_only_the_rank_delay`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`: the row "an order that ends ticks after its pass counts its clock from the pass" goes unreached |
| K12. `AcmeIssuance::tick`: `clock.left = clock.left.saturating_sub(1);` deleted, so no clock passes | `a_late_recheck_still_lets_rank_1_order_with_rank_0`, `a_restart_restarts_the_rank_delay`, `an_early_restart_does_not_end_the_route`, `an_order_s_clock_counts_from_its_pass`, `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `the_fix_needs_its_hold_reset`, `the_fix_waits_on_each_finding_s_schedule` |
| K13. `AcmeIssuance::pass`, the policy refusal: `if state.row.last_error() == Some(Reason::Policy) {` read as `if false {`, so `record_certificate_error` writes over its own reason | `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay` |
| K14. `AcmeIssuance::pass`, material present: `machine.first_seen = None;` before `block(machine);` deleted | `the_fix_on_each_edge_state` |
| K15. `AcmeIssuance::restart`: `machine.first_seen = None;` deleted | `a_restart_restarts_the_rank_delay`, `current_code_breaks_only_the_rank_delay`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay` |
| K16. `AcmeIssuance::pass`, the Nothing arm: its `block(machine);`, the one just before `let reset = self.resets_on_hold();`, deleted, so a held pass does not restart the property's count | `the_fix_on_each_edge_state` |
| K17. `Clock::holds`: its body `self.left > 0 && !(self.last == Failure::DoesNotResolve && verdict != Verdict::DoesNotResolve)` replaced by `self.left > 0`, so the model's reading of the gate ignores a verdict change | `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_on_each_edge_state` |
| K18. `actions`: `actions.push(Action::Errs(who));` deleted, so no call before the gate returns Err | `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_an_error_before_the_gate`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_early_return_reset`, `the_fix_waits_on_each_finding_s_schedule` |

K06 and K09 are the same as deleting the fix's drop itself. Deleting `if reset { machine.first_seen = None; }` from the Nothing arm turns K06's tests red: `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_on_each_edge_state` and `the_fix_waits_on_each_finding_s_schedule`. Reading `machine.first_seen.take().is_some()` in `early_return` as `machine.first_seen.is_some()` turns K09's red: `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_hold_reset` and `the_fix_waits_on_each_finding_s_schedule`.

A side-effect failure is a test whose pinned path or expected rows your edit already moved, such as `a_restart_restarts_the_rank_delay` under K01. A guard that no longer earns its keep fails with its flag alone, on a model with no other edit.

**Making a `COVERAGE` row unreachable.** Put `false && ` inside the row's closure, before its body, whether the body starts with `matches!(` or `took(`. For a row whose closure is one expression:

```rust
        reached: |_, state| false && matches!(state.took, Some(Branch::NoDirectory { .. })),
```

For a row whose closure is a block, inside the brace:

```rust
        reached: |_, state| {
            false && matches!(
                state.took,
                // the row's pattern, unchanged
            )
        },
```

or, for a row built on `took`:

```rust
        reached: |_, state| {
            false && took(
                state,
                // the row's branch, unchanged
            ) && state
                // the rest of the row, unchanged
        },
```

Then run:

```
cd core && cargo nextest run -p ployz-verify acme_issuance --no-fail-fast
```

Each of the 29 rows was made unreachable alone this way. Every row turned the tests that expect it red, and only those: `current_code_breaks_only_the_rank_delay` for a `Both` row, `the_fix_keeps_the_rank_delay` for every row, and `only_rank_zero_records_a_refusal` for every row but "rank 1 refuses and records nothing, and the row holds no reason", which it expects unreached.

## Bounds and blind spots

**Time, first.** One tick is 30 s, `issuance_step` under the built-in policy: `RANK_STEP` is `CHALLENGE_WAIT`, 30 s (`:40`, `:45`), and `DEFAULT_PROBE_TIMEOUT` is 30 s (`core/crates/ployz-core/src/domain/certificate_policy.rs:26-27`). `DEFAULT_BACKOFF_BASE` is 60 s, 2 ticks, and the backoff doubles up to the 3600 s cap (`core/crates/ployz-core/src/domain/issuance.rs:101-105`). `a_tick_is_one_issuance_step` pins each of these.
- Every time is held relative to now, so the model needs no tick bound. `first_seen` and `unblocked` are ages in ticks, capped at `AGE_CAP`, the largest rank delay, 1 tick: past it, every `action_due` answer and every property compare is the same. A clock holds its failure count, its last failure and the ticks left until `next_attempt_at`, zero once it has passed. An order in flight carries its snapshot clock's failures and kind and the ticks since its pass, because the failure record counts from the pass's `wall` (`:561`), not from when the order ends. That age is capped at `ago_cap`, the longest backoff a kept clock can hold, 120 s, 4 ticks: an order that old or older writes a clock that has already passed (`an_order_s_clock_counts_from_its_pass`).
- Every pass in a tick reads the same instant as its `now` and `wall` (`:469-470`), and the model rebuilds each `SystemTime` from it (`now()`). So the model explores runs whose passes land on the 30 s grid. Under the built-in policy every wait `poll_wait` or `run` asks for is a multiple of 30 s: `RANK_STEP` (`:204`), a rank delay less the whole ticks elapsed, capped at `RETRY_INTERVAL` (`:205`), and `RETRY_INTERVAL`, 60 s (`:394-400`, `:444`, `:448`). What happens between grid points is left out: how long a pass, a probe or an order takes, and a pass a container change wakes off the grid (`:402-409`).
- No pass is fair. A container change wakes every Machine's loop (`:402-409`), so the model offers a pass at every tick, and also lets a Machine skip ticks. In the code a Machine skips ticks while a pass and its obtains run, and between passes for as long as `poll_wait` (`:204-205`) or an error's `RETRY_INTERVAL` (`:394-400`) says. This over-approximates, which is safe for the fix's holds test. The finding's replays use only schedules those waits allow.
- A rank 0 that runs no loop is reached this way, by offering it no pass. Rank counts every `machines` row (`:465-468`), and `machines` has no filter on Down or leaving rows (`core/crates/ployzd/src/corrosion/store.rs:624-636`), while the loop runs only after `wait_for_participation` (`core/crates/ployzd/src/daemon.rs:494-507`).

**The policy bound.** The model explores only the built-in policy's values (`CertificatePolicy::built_in`, `core/crates/ployz-core/src/domain/certificate_policy.rs:93-103`), plus the operator's refuse, no-directory and repair actions below. A probe timeout above 30 s makes `issuance_step` longer than `RANK_STEP` (`:83-85`), and a probe timeout or backoff base that is not a multiple of 30 s does not fit the tick, so neither is explored. The backoff cap is never reached: the longest kept backoff is 120 s.

**The bounds,** each with the test that needs its value, from fresh runs at this PR's head. A drop turns those tests red, and a lift keeps every `always` on the code as it is except `RANK_DELAY_KEPT`, every `always` on the fix, and every row each model reaches.

| Bound | Value | What needs it | Dropped by one: tests that went red | Lifted by one: states as is / fixed |
|---|---|---|---|---|
| `MACHINES` | 2 | every finding needs a rank 1 | to 1: `a_late_recheck_still_lets_rank_1_order_with_rank_0`, `a_refusal_restarts_the_rank_delay`, `a_restart_restarts_the_rank_delay`, `an_early_restart_does_not_end_the_route`, `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`, `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`, `current_code_lets_rank_1_order_with_rank_0_after_an_error_before_the_gate`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `obtain_rechecks_material`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_early_return_reset`, `the_fix_needs_its_hold_reset`, `the_fix_on_each_edge_state`, `the_fix_waits_on_each_finding_s_schedule` | to 3: 124,505,516 / 32,941,384, in about 1,210 s in release |
| `MAX_FAILURES` | 2 | the row "a second failure of the same kind doubles the backoff" | to 1: `a_tick_is_one_issuance_step`, `an_order_s_clock_counts_from_its_pass`, `current_code_breaks_only_the_rank_delay`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay` | to 3: 1,197,056 / 729,052 |
| `AGE_CAP` | `MACHINES - 1`, 1 tick | rank 1's delay | to 0: `a_late_recheck_still_lets_rank_1_order_with_rank_0`, `a_refusal_restarts_the_rank_delay`, `an_early_restart_does_not_end_the_route`, `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`, `current_code_lets_rank_1_order_with_rank_0_after_an_authority_hold`, `current_code_lets_rank_1_order_with_rank_0_after_an_error_before_the_gate`, `current_code_lets_rank_1_order_with_rank_0_when_the_verdict_changes_during_a_refused_hold`, `obtain_rechecks_material`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_early_return_reset`, `the_fix_needs_its_hold_reset`, `the_fix_on_each_edge_state`, `the_fix_waits_on_each_finding_s_schedule` | to 2: 1,001,906 / 423,182 |
| `ago_cap` | `issuance_backoff(MAX_FAILURES, ..)`, 4 ticks | `an_order_s_clock_counts_from_its_pass` | to 3: `a_tick_is_one_issuance_step`, `an_order_s_clock_counts_from_its_pass` | to 5: 416,672 / 237,400 |
| The operator refuses the policy | at most once, from the built-in policy | `THE_POLICY_REFUSAL` and the policy rows | to never: `current_code_breaks_only_the_rank_delay`, `current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_early_return_reset`, `the_fix_waits_on_each_finding_s_schedule` | any number of times: 399,176 / 221,188 |
| The operator empties the directory | at most once, from the built-in policy | the row "with no directory, the pass returns before the gate" | to never: `current_code_breaks_only_the_rank_delay`, `only_rank_zero_records_a_refusal`, `the_fix_keeps_the_rank_delay`, `the_fix_needs_its_early_return_reset` | in the same lift |
| The operator repairs the policy | at most once | `THE_POLICY_REFUSAL` | to never: `current_code_lets_rank_1_order_with_rank_0_after_a_policy_refusal`, `the_fix_needs_its_early_return_reset`, `the_fix_waits_on_each_finding_s_schedule` | in the same lift |

A drop or a lift is one edit, then a run. `AGE_CAP` is `(MACHINES - 1) as u8`, so it moves with `MACHINES`; drop or lift it alone by writing `(MACHINES - 2) as u8` or `MACHINES as u8`.
- `MACHINES` and `MAX_FAILURES`: edit the constant.
- `ago_cap`, which is computed: append ` - 1` or ` + 1` after the `))` that closes `ticks(issuance_backoff(..))` in `fn ago_cap`.
- The operator bounds live in `actions`, in its `match state.policy`. To drop refusing, delete `Action::Operator(Policy::Refused),` from the `Policy::BuiltIn` arm's `extend([...])`. To drop emptying, delete `Action::Operator(Policy::NoDirectory),` from it. To drop repair, delete the `actions.push(Action::Operator(Policy::Repaired));` in the `Policy::Refused | Policy::NoDirectory` arm.
- The policy lift lets a repaired policy be refused or emptied again: in `actions`, replace the `Policy::Repaired => {}` arm with

```rust
            Policy::Repaired => actions.extend([
                Action::Operator(Policy::Refused),
                Action::Operator(Policy::NoDirectory),
            ]),
```

A drop runs the module. Each lift runs this test, put at the end of `mod tests`, before the file's last closing brace:

```rust
    #[test]
    fn scratch_lift() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(RANK_DELAY_KEPT).is_some());
        assert!(checker.discovery(HOLD_KEPT).is_none(), "{HOLD_KEPT} breaks");
        assert!(checker.discovery(NO_ORDER_OVER_MATERIAL).is_none(), "{NO_ORDER_OVER_MATERIAL} breaks");
        for row in COVERAGE.iter().filter(|row| row.reach == Reach::Both) {
            assert!(checker.discovery(row.name).is_some(), "{} is never reached", row.name);
        }
        let checker = check(fixed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=acme_issuance::tests::scratch_lift)' --no-capture --release
```

Without an edit it prints `as is: 370920 states` and `fixed: 205372 states`. Every lift passed. The lift to three Machines peaked at about 11 GB of memory in an earlier run of the same model.

**Unbounded.** Each Machine's verdict flips without bound (`Action::Probe`), so a verdict can be older than the latest DNS change on one Machine and not the other (`core/CONTEXT.md:576-577`). Restarts, lost records and Errs before the gate are unbounded too. The model explores one refusal kind, DoesNotResolve, and one route, Direct.

**Each read, and why the model's steps reach the same states.** Every write between two reads of one Machine is another Machine's step.
- **The pass's `certificate_state`** (`:451`). The pass decides from it, and its only write is the refusal record, whose `acme_row` reads the row again (`core/crates/ployzd/src/corrosion/store.rs:984`). Between the two, another Machine can publish material, record a failure or record the policy's reason. If material lands, the record writes nothing (`core/crates/ployzd/src/corrosion/store.rs:987-990`), and the model reaches the same row by taking the pass first and the publish after, since `issued` replaces the whole row (`core/crates/ployzd/src/corrosion/certificate.rs:255-259`). If another record lands, the pass's record writes over it with the clock from its own snapshot. The model reaches that row by taking the pass first and losing the other record, which is why every record kind has a lost outcome.
- **The policy refusal's reads** (`:432`, `:437-438`) decide only which hostnames are wanted. `record_certificate_error` then reads the row with `acme_row`, compares its reason, and upserts (`core/crates/ployzd/src/corrosion/store.rs:1020-1033`). The model takes it as one step.
- **`obtain`'s `certificate_row`** (`:623`). Between it and `publish_certificate`'s `acme_row` (`core/crates/ployzd/src/corrosion/store.rs:857`), other Machines can write anything. `publish_certificate` writes `issued` over whatever it reads, unless the row is Published or already equal (`core/crates/ployzd/src/corrosion/store.rs:857-864`), so the row ends `issued` either way, the row the model reaches by taking the obtain last. Two orders that both re-read no material reach the same end states as the model's: an issued order and a failed one end with the material (the failure record writes nothing once material landed, and `issued` replaces a record that landed first), and two issued orders end with material, which the model reaches with the second obtain's recheck.
- **The failure record's `acme_row`** (`core/crates/ployzd/src/corrosion/store.rs:984`). The record takes its clock from the pass's snapshot (`:559`), not from this read, and the model's obtain does too (`Order::snapshot`). So the model explores a record written from a stale snapshot over a newer row.
- **Inside each writer**, the read with `acme_row` and the upsert are separate calls (`core/crates/ployzd/src/corrosion/store.rs:943-954`). Another Machine's write can land between them, and the upsert then writes over it. The model takes each writer as one step. E-17 owns concurrent writes to the row.
- **Each Machine reads its own Corrosion replica.** The model's one row, which every Machine reads as soon as it is written, leaves out a read of an older row.

**Code-as-is facts, and what the model does with each.** Each scratch lift below ran `scratch_lift` at this PR's head.
- **A daemon restart** empties `first_seen` (`:367`) and ends an obtain in flight: modelled, `Action::Restart`.
- **`record_certificate_failure` and `record_certificate_error` can return Err**, which is only logged for a refusal or a failed order (`:524-527`, `:574-578`) and returned for the policy refusal (`:442`): modelled, each as a lost outcome.
- **An Err before the gate**: modelled, `Action::Errs`.
- **`account` can fail after the push** (`:539`). `issue_wanted` then returns Err: no obtain runs, no clock is written, `first_seen` is kept, and `run` waits `RETRY_INTERVAL` (`:394-400`). The model's `Ends::Failed { recorded: false }` reaches the same row and Machine state, and `Ends::Rechecked` the same where the re-read holds material. A scratch lift with an `Ends::Account` outcome added found no new `always` break and every row still reached (398,048 states as is, 219,516 fixed).
- **A policy refusal or a missing directory** returns before `first_seen.retain` and the gate (`:436-449`), so `first_seen` keeps aging through it: modelled, with `Action::Operator`. The policy refusal's `record_certificate_error` replaces the row's refusal with a reason and no clock (`with_error`, `core/crates/ployzd/src/corrosion/certificate.rs:354-362`), unless the row's `last_error` already equals that reason (`core/crates/ployzd/src/corrosion/store.rs:1028-1030`). So it drops the shared clock from a row whose recorded reason differs. `with_refusal` keeps the material and the challenge (`core/crates/ployzd/src/corrosion/certificate.rs:364-377`).
- **The hostname can leave the wanted set, or a Published cover can appear** (`:351`, `:453`). Each prunes `first_seen` at the next pass, which then has no work for the hostname. That changes the Machine's state as a restart does, without ending an obtain. A scratch lift with an `Action::Unwant` that drops `first_seen` and blocks found no new `always` break and every row still reached (389,204 states as is, 220,240 fixed).

**Left out**, with nothing in the model that stands for it:
- **Renewals,** where material is present. A renewal's delay is measured from each Machine's own `renew_at` (with its `machine_jitter`, `:138-150`) plus the rank delay (`:106-117`), not from `first_seen`, and a hold does not move it. So every Machine whose renewal came due during a hold is Renew-due when the gate opens, and the fix does not touch that.
- **Risk 2**, the `RANK_STEP == CHALLENGE_WAIT` margin, above.
- **The challenge slot.** With an atomic `obtain`, no other step sees a challenge, so two challenges overwriting each other is argued from the code in the PR body, not shown by the model.
- **More than one hostname.** Each hostname has its own `first_seen` entry and row. Other hostnames change only when this Machine's next pass comes, through `join_all` and `account`, which the unfair schedule already covers.
- **Membership changes**, which change ranks.
- The other refusals (Unreachable, RedirectsToHttps, ReachesElsewhere) and the ViaProxy route.

A green model says nothing about them. Cover the code ones with:

```
cd core && cargo nextest run -p ployz-core issuance
cd core && cargo nextest run -p ployzd certificates
```

Expected: `9 tests run: 9 passed` for `ployz-core issuance`, and `28 tests run: 28 passed` for `ployzd certificates`.

## Keep the model in step

When a change alters when a pass pushes a hostname, what it records, or what `obtain` reads, change the model in the same PR. Examples: a new guard before the gate, a write of `first_seen` on another branch, a record from any rank, a new early return.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `issue_wanted`: `AcmeIssuance::pass`, `early_return`, and the `Action::Errs` arm of `next_state`;
   - `obtain` and the failure record: `AcmeIssuance::obtain` and `record_failure`;
   - `run`: `AcmeIssuance::restart`, and the offers in `actions`;
   - `action_due`, `poll_wait`, `rank_delay`, `issuance_step` and `machine_rank`: their hand copies.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change ships the fix, model it as the code writes it and make it the default. Then turn the finding tests into a `Broken` flag that restores the old code, and keep the tests, since their printed paths are the record of what the fix keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names and reads the field the name claims.
5. Every `State` field is read: `row`, `policy`, each Machine's `verdict`, `first_seen`, `obtaining` and `unblocked` (by `RANK_DELAY_KEPT`), and `took` (by the properties and rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit. Reread these when a diff touches them:

- `core/crates/ployzd/src/certificates.rs`: `CHALLENGE_WAIT`, `RETRY_INTERVAL`, `RANK_STEP` and its doc comment; `machine_rank`, `issuance_action` and its doc comment, `rank_delay`, `issuance_step`, `action_due` and `poll_wait`; `run`; `issue_wanted`; `obtain`; `wanted_certificate_hosts`.
- `core/crates/ployz-core/src/domain/issuance.rs`: `IssuanceClock`, `IssuanceGate`, `issuance_gate`, `issuance_backoff` and `issuance_failure_clock`.
- `core/crates/ployz-core/src/domain/certificate_policy.rs`: `DEFAULT_BACKOFF_BASE`, `DEFAULT_BACKOFF_CAP`, `DEFAULT_PROBE_TIMEOUT`, `CertificatePolicy::built_in` and `resolve_certificate_policy`.
- `core/crates/ployzd/src/corrosion/store.rs`, the ACME writers E-17 will change: `machines`, `publish_certificate`, `acme_row`, `upsert_certificate`, `publish_certificate_challenge`, `record_certificate_failure` and `record_certificate_error`.
- `core/crates/ployzd/src/corrosion/certificate.rs`: `RecordedRefusal`, `CertificateRow`, `issued`, `with_backoff`, `clock`, `last_error`, `with_challenge`, `with_error` and `with_refusal`.
- `core/crates/ployzd/src/daemon.rs`: the certificate loop's start after `wait_for_participation`.
- `core/crates/ployzd/src/runtime_watch.rs`: `redact_certificate`.

Sync commit: `9a3a0708749bc98d14fda4f06025d4aa6acc43f3`, the `main` this model was checked against, and every line cited above is at it. Code in the list above that changed after it has not been rechecked.
