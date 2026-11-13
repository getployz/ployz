---
name: verify-acme-issuance
description: Model-check when each Machine's certificate loop pushes an Ingress Hostname with no Certificate Material to to_order, the rank delay that staggers Machines, against the shared backoff clock in the replicated certificate row, each Machine's own Hostname Verdict, a Certificate Policy that is refused or has no directory, an Err before the gate, failed orders, lost records and daemon restarts, with the Stateright model in core/crates/ployz-verify/src/acme_issuance.rs. Use when a change touches core/crates/ployzd/src/certificates.rs (run, read_pass, issue_wanted, Host, held, advance, obtain), core/crates/ployz-decide/src/issuance.rs (Issuance, step, wake, issued, action_due, issuance_action, rank_delay, issuance_step, machine_rank, RANK_STEP, Held, Pass, Prev), core/crates/ployz-core/src/domain/issuance.rs (issuance_gate, IssuanceGate, issuance_failure_clock, issuance_backoff, IssuanceClock), the ACME writers in core/crates/ployzd/src/corrosion/store.rs (acme_row, write_acme_row, publish_certificate, publish_certificate_challenge, record_certificate_failure, record_certificate_error), or CertificateRow's issued, with_backoff, with_error and with_challenge in core/crates/ployzd/src/corrosion/certificate.rs.
---

# Verify ACME issuance scheduling

The model in `core/crates/ployz-verify/src/acme_issuance.rs` explores when each Machine's certificate loop pushes one Ingress Hostname with no Certificate Material to `to_order`. A bare `:n` below is a line of `core/crates/ployzd/src/certificates.rs`. Every other path is written in full from the repo root, and every command runs from the repo root.

## What the model copies

**What each Machine hosts.** Each daemon runs one certificate loop, `run` (`:314-370`), once it participates (`core/crates/ployzd/src/daemon.rs:490-503`). The loop keeps `hosts`, a map from hostname to its `Host`, an `Issuance` and the instant its rank delay started (`:120-133`, `:322`), and runs `issue_wanted` (`:452-607`) once per wake. A pass that pushed the hostname awaits `account` (`:527`) and then `join_all` over `obtain` (`:528-539`), so the same Machine passes again only once its obtains end. A daemon restart builds an empty `hosts` (`:322`) and ends an obtain in flight.

**`Issuance`** (`core/crates/ployz-decide/src/issuance.rs:111-132`), one per wanted hostname, lives only in the loop's memory, in `Host` beside the instant its rank delay started (`:120-133`; the model's `orderable_since`):
- `Issued { renew_due }`: the row holds material whose renewal is not due (`issued`, `core/crates/ployz-decide/src/issuance.rs:134-151`);
- `Waiting`: the gate let the pass through, and the rank delay runs from `Host::since`;
- `Held(clock)`: the shared clock holds the hostname;
- `Refused { refusal, clock }`: this Machine's Hostname Verdict refuses it, and `clock` is the backoff it records;
- `Order(route)` and `Renew(route)`: the pass pushes the hostname. `Order` keeps the start of its rank delay in `Host::since`.

`step` (`core/crates/ployz-decide/src/issuance.rs:181-212`) moves one hostname's state, and says whether the rank delay keeps its start; `advance` (`:147-163`) keeps `Host::since` accordingly. The gate comes first: Hold answers `Held` and Refuse answers `Refused` (`core/crates/ployz-decide/src/issuance.rs:185-189`). Through the gate, material answers `Renew` (`core/crates/ployz-decide/src/issuance.rs:192-194`). Otherwise the elapsed time is the one a `Waiting` or `Order` held, and every other state, or none, starts it at this pass (`core/crates/ployz-decide/src/issuance.rs:195-203`). `issuance_action` then answers `Order` or `Waiting` against the pass's own rank and policy (`core/crates/ployz-decide/src/issuance.rs:204-207`), so an `Order` whose rank delay has since grown waits again. So a held pass, a refused pass, or a pass the policy or a missing directory stopped ends the rank delay, and the next pass the gate lets through starts it again. A read that returns Err does not.

**The pass**, `Action::Pass` and `AcmeIssuance::pass`, copies `read_pass` (`:391-450`) and the loop in `issue_wanted` (`:461-525`) for one hostname:
- the policy refusal (`:404-419`): every Machine calls `record_certificate_error` for every wanted hostname (`:407-417`), and `read_pass` answers `None`;
- no directory (`:421-423`): `read_pass` answers `None` too;
- on `None`, `issue_wanted` clears `hosts` and returns (`:471-474`), and `run` sleeps `RETRY_INTERVAL`, 60 s (`:473`);
- the reads (`:424-425`), `hosts.retain` (`:475`), which keeps the one wanted hostname, the rank (`:430-435`), and `now` and `wall` (`:476-477`);
- material whose renewal is not due: `issued` answers `Issued`, and the pass skips the probe (`:481-482`);
- otherwise the probe and verdict (`:484-490`) and `step` (`:491-492`);
- `Refused`: the record, at rank 0 only, with `record_certificate_failure` (`:497-516`);
- `Order`: the push (`:600-602`).

**An Err before the gate**, `Action::Errs`. A store read (`:400`, `:405-406`, `:424-425`, `:430`) or `HostnameProbe::new` in `read_pass` can return Err. `read_pass` returns it through `?`, `issue_wanted` returns it through `?` with `hosts` as it was (`:470`), and `run` prints "failed to obtain certificates: …" and waits `RETRY_INTERVAL` (`:349-355`).

**The obtain**, `Action::Obtain` and `AcmeIssuance::obtain`, is one step: `obtain` (`:618-674`) and the failure record after `join_all` (`:540-598`). Its outcomes:
- the re-read (`:627`) holds material, and the recheck returns (`:628-639`);
- the order is issued, and `publish_certificate` writes `CertificateRow::issued`, material with no challenge and no refusal (`:664-672`, `core/crates/ployzd/src/corrosion/store.rs:966-986`, `core/crates/ployzd/src/corrosion/certificate.rs:259-261`);
- the order fails. `issue_wanted` prints "failed to obtain certificate for …" (`:542-549`). `issuance_failure_clock` counts from the pass's snapshot clock (`:551`) and the pass's `wall` (`:553`), and `record_certificate_failure` gets the snapshot's material (`:564-576`), which in the model is none. Its no-material arm writes nothing when the row it reads holds material, and otherwise writes `with_backoff` (`core/crates/ployzd/src/corrosion/store.rs:1186-1196`), which keeps the row's material and challenge and replaces only the refusal (`core/crates/ployzd/src/corrosion/certificate.rs:281-293`, `:171-379` of that file);
- the failure record returns Err, which is only logged (`:593-595`), and nothing is written.

**The wait.** After the obtains, `issue_wanted` answers the least `wake` over `hosts` (`:662-668`). `wake` (`core/crates/ployz-decide/src/issuance.rs:221-259`) answers for `Waiting` the time left of its rank delay, no later than `RANK_STEP`, so a Waiting loop asks to sleep no longer than a step (`core/crates/ployz-decide/src/issuance.rs:238-245`), and `RANK_STEP` for `Order` and `Renew` (`core/crates/ployz-decide/src/issuance.rs:257`). For `Held` and `Refused` it answers when the clock passes, and no later than the next probe: `RANK_STEP` after a refusal, which a changed verdict can lift early, and `RETRY_INTERVAL` after an authority failure (`core/crates/ployz-decide/src/issuance.rs:246-256`). A clock never holds longer than the policy's `backoff.cap`, so an operator's small cap wakes the loop that often: a 1 s cap means a pass, and at rank 0 maybe a record, every second. A container change wakes the loop sooner (`:357-364`). The story prints the wait, and `WAKES_BY_THE_DELAY` reads it for a `Waiting` hostname.

**The real functions it calls.** `issuance_gate`, `issuance_failure_clock`, `issuance_backoff`, `IssuanceClock::new` and its accessors (`core/crates/ployz-core/src/domain/issuance.rs:14-126`), and `CertificatePolicy::built_in(None)`'s `backoff_base`, `backoff_cap` and `probe_timeout` (`core/crates/ployz-core/src/domain/certificate_policy.rs:93-103`).

**Called, not copied.** The model calls `ployz-decide`'s `core/crates/ployz-decide/src/issuance.rs` for the scheduling: `machine_rank` (`:38-45`), `rank_delay` (`:47-51`), `issuance_step` (`:53-58`), `action_due` (`:60-97`), `issued`, `step` and `wake`, with `RETRY_INTERVAL` (`:15`) and `RANK_STEP` (`:17-19`). `ployzd`'s `issue_wanted` calls the same functions. `ployz-decide` depends only on `ployz-core`, and `ployz-verify` depends on both (`core/crates/ployz-verify/Cargo.toml`). `ployzd` keeps the part that reads a certificate: `renew_at` (`:63-76`) and `held`, which turns a row into `Held`. The model's `Held::Material { renew_at: None }` is renewal left out: with material present the model answers `(Nothing, RETRY_INTERVAL)`, the arm `action_due` takes when `renew_at` cannot read the certificate (`core/crates/ployz-decide/src/issuance.rs:95`).

**Model-local types.** `IssuanceClock`, `IssuanceFailure`, `Refusal`, `HostnameVerdict` and `ClusterRoute` derive no `Hash` (`core/crates/ployz-core/src/domain/issuance.rs:8`, `:15` of that file; `core/crates/ployz-core/src/domain/hostname_verdict.rs:27`, `:40`, `:46` of that file), and Stateright needs `State: Hash`. So the state holds `Clock`, `Failure`, `Verdict`, `Recorded` and `Row`, and each call rebuilds the real value (`Clock::real`, `Verdict::real`, `Failure::real`) and reads the answer back (`Clock::of`, `Failure::of`). A Machine's `Host` is `Machine::orderable_since`, the age of `Host::since`: `Some` while the `Issuance` is `Waiting` or `Order`, and `None` for every other state or none. The model's ranks and policy never change, so an `Order` stays past its delay.

**The row.** `Row` is the hostname's `CertificateRow::Acme` (`core/crates/ployzd/src/corrosion/certificate.rs:221-235`): whether it holds material, and its refusal, a reason and an optional clock (`RecordedRefusal`, `core/crates/ployzd/src/corrosion/certificate.rs:214-219`). Its challenge is left out, because no step reads it: the model's `obtain` is atomic, so no other step can see a challenge it published. `Reason` keeps only which reason a refusal holds, because only `record_certificate_error` compares it (`core/crates/ployzd/src/corrosion/store.rs:1243-1244`).

## The code as it was

Before `Host`, the loop kept `first_seen`, the instant its pass first saw the hostname with no material. A refusal removed it, but a hold of the shared clock and a pass that returned before the gate kept it. `AS_IT_WAS` is that code: the flags `hold_keeps_orderable_since` and `early_return_keeps_orderable_since`. `as_it_was()` in the tests builds it. Only rank 0 recorded a refusal, as it still does, and an Err kept `first_seen`, as it still keeps `orderable_since`.

On it, `orderable_since` aged through a hold and through an early return, so once the gate opened, or the early return cleared, rank 1 could push the hostname in the same tick as rank 0, though it waited no rank delay since its last blocked pass. The checker finds three routes. Each test now replays its schedule on both models: on `as_it_was()` rank 1 pushes early, and on `as_is()` rank 1's last pass waits, and a tick later it pushes with its rank delay kept. Each story shows one possible schedule: the real loops pass in either order within a step.

- **The authority hold** (`an_authority_hold_restarts_rank_1_s_delay`). At 0 s rank 0's pass pushes the hostname, and its order fails. The record writes an Authority clock with one failure and `next_attempt_at` 60 s after the pass's `wall` (`DEFAULT_BACKOFF_BASE`, `core/crates/ployz-core/src/domain/certificate_policy.rs:20-21`). Rank 1's passes are held. At 60 s the clock has passed, and rank 0 pushes the hostname. On the code as it was, rank 1's held passes kept `orderable_since`, and it pushed too.
- **The verdict change** (`a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`). Both probes find the hostname does not resolve. Rank 0 refuses and records a DoesNotResolve clock, and rank 1's pass is held. At 30 s both probes reach the Cluster. That verdict differs from the clock's refusal, so `issuance_gate` opens with 30 s still on the clock (`core/crates/ployz-core/src/domain/issuance.rs:77-84`), and rank 0 pushes the hostname.
- **The policy refusal** (`a_policy_refusal_restarts_rank_1_s_delay`). Rank 0 is on time. At 0 s rank 1's pass waits out its rank delay. The operator sets a Certificate Policy that `resolve_certificate_policy` refuses, and rank 0's first pass, also at 0 s, records the policy's reason and stops before the gate; `run` sleeps 60 s (`:473`). At 30 s rank 1's pass stops the same way. The operator repairs the policy. At 60 s rank 0's sleep ends and it pushes the hostname, and a container change wakes rank 1's loop at that moment (`:357-364`). The refusal held rank 0 back as long as rank 1, so on the code as it was neither Machine's delay separated them.

An Err before the gate is not a route. It keeps `orderable_since`, so a read that fails now and then cannot starve issuance, and it is no blocked pass. `a_read_error_keeps_rank_1_s_delay` replays the policy refusal's schedule with an Err in place of each refusal, rank 0's at 0 s and rank 1's at 30 s: rank 1 pushes with rank 0 on both models, after an outage both Machines saw, as on the code as it was.

A rank 0 whose first pass comes late pushes with rank 1 on the code as it is too, with no early return at all: that is risk 2's family (What the property is not, below), not these routes.

Each old-code replay asserts its branches, and then asks the checker for its first path to the same broken state (`breaks_where`). The checker keeps one path to each state, the first in BFS order, and a shorter path can reach the same state, so the replay shows the route's own schedule, and the second check shows the checker discovers its end state.

**The timing condition, on the code as it was.** Rank 1's `orderable_since` had to be at least one `issuance_step` old at its first pass after the gate opened. `the_old_route_survived_a_late_recheck` has both Machines pass 60 s after the gate opens: on the code as it was rank 1 still pushed with rank 0, and on the code as it is it waits. A removal of the entry ended the route only when no held pass started it again at least one issuance step before the gate opened: `the_old_route_survived_an_early_restart` restarts rank 1's daemon right after its first held pass, and `a_late_restart_ended_the_old_route` restarts it after the gate opens.

**The property** is `RANK_DELAY_KEPT`, an `always`: "a Machine of rank r pushes the hostname to to_order only r issuance steps or more after its first pass since its last blocked pass". A blocked pass is one whose gate answered Hold or Refuse, one the policy refusal or no directory stopped before the gate, or one that found material present. An Err before the gate is not one: the code keeps the delay across it. A Machine's first pass ever counts as unblocked. `Machine::unblocked` holds the ticks since that pass, and `Branch::Pushed { early }` compares them with the rank.

It is justified from the code. The doc comment on `RANK_STEP`, in full (`core/crates/ployz-decide/src/issuance.rs:17-18`):

> The rank delay's step. It must cover the 30 s ployzd waits for an HTTP-01 challenge to be served, so rank 1 cannot write a competing token while rank 0 is still probing.

`issuance_step`'s doc comment, in full (`core/crates/ployz-decide/src/issuance.rs:53-54`):

> One rank step under `policy`: a later rank cannot start a competing order while an earlier rank is still presenting, so the step is at least the probe timeout.

`step`'s doc comment says how the code keeps it (`core/crates/ployz-decide/src/issuance.rs:181-182`):

> The next state of a hostname this pass probed. A hold or a refusal ends the rank delay, so the first pass the gate lets through after one starts it again.

The property keeps one part of the first two comments, the delay itself: each Machine waits its rank delay, measured from its own first pass since it was last blocked. It says nothing about when rank 0 starts presenting.

**What the property is not.** The comments promise something across Machines: a later rank does not start a competing order while an earlier one is presenting. The property is per Machine, and each Machine measures its delay from its own first pass. With the built-in probe timeout, the delay equals the probe window exactly (`core/crates/ployz-decide/src/issuance.rs:17-19`, `core/crates/ployz-decide/src/issuance.rs:53-58`, and `wait_for_http01`'s deadline at `:890`). Rank 0's probe starts only after its account, `new_order` and challenge publish (`order_certificate`, `:866-870`, `:716`; `obtain`'s `present`, `:641-662`). So the cross-Machine promise can fail even with no hold, by however long those steps take. It can also fail after a hold both Machines saw: if rank 0's first pass after the clock passes returns Err, its loop waits `RETRY_INTERVAL` (`:349-355`), rank 1 starts its delay at its own pass, and a container change can wake rank 0 just as rank 1's delay ends, so both push in one tick. That is risk 2, and it is out of scope. A cross-Machine property, such as "rank 1 never pushes in the same tick as rank 0", would fail on the code as it is too: the model lets either Machine skip a tick.

## Run it

```
cd core && cargo nextest run -p ployz-verify acme_issuance --no-capture
```

Expected: the summary line ends with `18 tests run: 18 passed, 193 skipped`. `the_code_keeps_every_property` prints `as is: 245640 states`, `the_code_as_it_was_breaks_only_the_rank_delay` prints `as it was: 431688 states`, and `a_read_error_keeps_rank_1_s_delay` prints `an Err clears hosts: 207756 states`. Each story prints under its test's name. `--no-capture` runs the tests one at a time, so a warm run took about 16 s of nextest time; without it, about 5 s. CI runs `ployz-verify` in its workspace nextest run.

Read the tests in four groups.
- **The code as it is.** `the_code_keeps_every_property` runs `as_is()` with `assert_properties`: every `always` holds, and every row is reached. The three route tests and `a_read_error_keeps_rank_1_s_delay` replay each schedule on it, and `a_pass_on_each_edge_state` steps one pass of rank 1 from each state `step` must handle, on the code and on the code as it was: no clock, an Authority clock not yet due, an unchanged DoesNotResolve clock not yet due, a clock that is due, a DoesNotResolve clock whose verdict changed, a DoesNotResolve verdict with no clock, and material present, where the pass skips at `:481-482`. It then steps rank 0 and rank 1 from a state where neither has `orderable_since`: rank 0 owes no delay and pushes, and rank 1 waits.
- **The code as it was.** The three route tests' old-code replays; `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` and `a_late_restart_ended_the_old_route`, which pin the timing condition; and `the_code_as_it_was_breaks_only_the_rank_delay`, which expects `RANK_DELAY_KEPT` to break, `HOLD_KEPT`, `NO_ORDER_OVER_MATERIAL` and `AUTHORITY_BACKOFF_KEPT` to hold, and every `COVERAGE` row whose `on` keeps for `AS_IT_WAS` to be reached. It also prints the checker's first broken path.
- **The guards**, each with one flag set on the code as it is through `without(..)`:
  - `the_gate_holds_while_the_clock_holds` sets `hold_orders` and expects `HOLD_KEPT` to break. It then keeps to a path whose last pass's probe finds the hostname does not resolve: a held Refused verdict has no route to order along, so the break orders along Direct, the model's only route.
  - `obtain_rechecks_material` sets `obtain_skips_recheck` and expects `NO_ORDER_OVER_MATERIAL` to break.
  - `a_held_pass_restarts_the_rank_delay` sets `hold_keeps_orderable_since`: the authority hold and the verdict change push early.
  - `a_refusal_restarts_the_rank_delay` sets `refusal_keeps_orderable_since`: rank 1 refuses, its verdict changes, and it is Order-due at once.
  - `a_pass_that_stops_before_the_gate_restarts_the_rank_delay` sets `early_return_keeps_orderable_since`: the policy refusal and no directory push early.
  - `a_read_error_keeps_rank_1_s_delay` sets `error_clears_hosts`: rank 1's passes that alternate with an Err never push, and the row "a call before the gate returns Err, and orderable_since stays" is never reached. No `always` breaks, since a cleared delay only waits longer.
  - `only_rank_0_records_a_refusal` sets `any_rank_records` and expects `AUTHORITY_BACKOFF_KEPT` to break (Only rank 0 records, below).
- **The bounds.** `an_order_s_clock_counts_from_its_pass` and `a_tick_is_one_issuance_step` pin the failure clock's `wall` and the tick (Bounds).

## The change

```text
a pass whose gate holds                 Held: the next pass the gate lets through starts orderable_since (core/crates/ployz-decide/src/issuance.rs:186, core/crates/ployz-decide/src/issuance.rs:197-203)
a pass whose gate refuses               Refused: the same (core/crates/ployz-decide/src/issuance.rs:187-189, core/crates/ployz-decide/src/issuance.rs:197-203), and the record at rank 0 (:497-516)
an open pass after Order                checks Order's orderable_since against this pass's rank and policy (core/crates/ployz-decide/src/issuance.rs:196-211)
a pass the policy or no directory stops hosts.clear() (:471-474)
a read that returns Err                 returns it with hosts as it was (:470)
a Waiting hostname                      wakes the loop no later than RANK_STEP (core/crates/ployz-decide/src/issuance.rs:241-245)
a held or refused hostname              wakes the loop when its clock passes, or at its next probe (core/crates/ployz-decide/src/issuance.rs:246-256)
```

| Part | What it keeps | Flag | Test, and what breaks without it |
|---|---|---|---|
| A held pass ends the rank delay | The authority hold and the verdict change: rank 1's next pass after the hold starts `orderable_since` and waits its rank delay | `hold_keeps_orderable_since` | `a_held_pass_restarts_the_rank_delay`, `RANK_DELAY_KEPT` |
| A refused pass ends the rank delay, as `first_seen.remove` did | A refusal whose verdict then changes | `refusal_keeps_orderable_since` | `a_refusal_restarts_the_rank_delay`, `RANK_DELAY_KEPT` |
| `hosts.clear()` on a pass the policy or no directory stops | The policy refusal and no directory | `early_return_keeps_orderable_since` | `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `RANK_DELAY_KEPT`, by replays of the policy refusal and no directory |
| A read Err keeps `hosts` | Issuance while reads fail now and then: the delay rank 1 earned survives an Err | `error_clears_hosts` | `a_read_error_keeps_rank_1_s_delay`: the row "a call before the gate returns Err, and orderable_since stays" |
| Only rank 0 records a refusal | Rank 0's Authority backoff, which a refusal from rank 1 would reset | `any_rank_records` | `only_rank_0_records_a_refusal`, `AUTHORITY_BACKOFF_KEPT` |
| `Order` keeps its start, and each pass checks it | A rank or a policy that grows after an `Order` | none: the model's ranks and policy are fixed (Bounds) | `ployzd`'s `a_rank_that_rises_after_an_order_waits_its_new_delay` |
| `wake` for a `Waiting`, held or refused hostname | A `Waiting` hostname wakes the loop by the end of its rank delay (`WAKES_BY_THE_DELAY`); a held or refused hostname's wake is the story's wait only | none | `core/crates/ployz-decide/src/issuance.rs`'s `a_waiting_hostname_wakes_by_the_end_of_its_rank_delay`, `a_waiting_rank_meets_a_hold_of_one_step` and `a_held_hostname_wakes_when_its_clock_passes_or_at_its_next_probe` |

**Only rank 0 records.** A refusal from a Machine above rank 0 would replace the row's clock with its own, counted from its own snapshot. Over a passed Authority clock, that resets rank 0's next Authority failure to one (`core/crates/ployz-core/src/domain/issuance.rs:117-120`), and with `any_rank_records` the model reaches it: in `only_rank_0_records_a_refusal`, rank 0's second failure backs off 60 s instead of 120 s. Two Machines whose probes refuse for different reasons would also replace each other's clock at one failure on every pass; the model has one refusal kind, so the `ployzd` test `only_rank_0_records_a_refusal` is the proof that rank 1 writes nothing.

The cost is the row while rank 0 runs no loop: no Machine records the refusal, the row holds no reason ("rank 1 refuses and records nothing, and the row holds no reason"), and `redact_certificate` reports Unknown for a row with no material, no challenge, no clock and no reason (`core/crates/ployzd/src/runtime_watch.rs:467-477`). That is not fixed here; it is parked as E-46b.

What the change keeps as it is:
- **Renewals.** It does not touch the renewal arm (Bounds).
- **Risk 2.** The cross-Machine promise above.

## Read a counterexample

A story is a numbered list of steps cut at the last state of a path. Each row is one step at the time it ran, `[N s]` from the start, named for who takes it: Rank 0, Rank 1, Clock (a tick), DNS (a probe change) or Operator. A bare `:n` in a row is a line of `core/crates/ployzd/src/certificates.rs`. The line under each row is the state after the step:
- `row` is the replicated row: material or none, then its reason and clock, or no reason;
- `policy` is what `resolve_certificate_policy` makes of the Certificate Policy;
- each Machine's `probe` is what its next probe sees. `orderable_since` and `unblocked` are ages, "or more" where the age is capped, and `obtain in flight` gives how long ago its pass ran.

A row that skips a guard says "(under Broken)" when a flag is set. A story that starts mid-path starts with "from the state above". `follow` prints "a schedule" where `breaks` prints the property.

These are the last three rows of `an_authority_hold_restarts_rank_1_s_delay`'s replay on the code as it was:

```
 7. [60 s] Clock: 30 s pass
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, orderable_since 30 s or more, no obtain, unblocked none | Rank 1: probe Direct, orderable_since 30 s or more, no obtain, unblocked none
 8. [60 s] Rank 0: pass: its probe sees the hostname reach the Cluster directly (:484-490); issuance_gate answers Order, as the clock has passed (core/crates/ployz-core/src/domain/issuance.rs:77-86); the rank delay runs from orderable_since, 30 s or more old (core/crates/ployz-decide/src/issuance.rs:196-203), and issuance_action answers Order (core/crates/ployz-decide/src/issuance.rs:204-207); the hostname is pushed to to_order (:600-602), and account and obtain are next (:526-539)
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, orderable_since 30 s or more, obtain in flight from a pass 0 s ago, unblocked 0 s | Rank 1: probe Direct, orderable_since 30 s or more, no obtain, unblocked none
 9. [60 s] Rank 1: pass: its probe sees the hostname reach the Cluster directly (:484-490); issuance_gate answers Order, as the clock has passed (core/crates/ployz-core/src/domain/issuance.rs:77-86); the rank delay runs from orderable_since, 30 s or more old (core/crates/ployz-decide/src/issuance.rs:196-203), and issuance_action answers Order (core/crates/ployz-decide/src/issuance.rs:204-207); the hostname is pushed to to_order (:600-602), less than its rank delay after its first pass since its last blocked pass, and account and obtain are next (:526-539)
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, orderable_since 30 s or more, obtain in flight from a pass 0 s ago, unblocked 0 s | Rank 1: probe Direct, orderable_since 30 s or more, obtain in flight from a pass 0 s ago, unblocked 0 s
```

and on the code as it is:

```
 7. [60 s] Clock: 30 s pass
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, orderable_since none, no obtain, unblocked none | Rank 1: probe Direct, orderable_since none, no obtain, unblocked none
 8. [60 s] Rank 0: pass: its probe sees the hostname reach the Cluster directly (:484-490); issuance_gate answers Order, as the clock has passed (core/crates/ployz-core/src/domain/issuance.rs:77-86); the rank delay runs from this pass (core/crates/ployz-decide/src/issuance.rs:196-211), and issuance_action answers Order (core/crates/ployz-decide/src/issuance.rs:204-207); the hostname is pushed to to_order (:600-602), and account and obtain are next (:526-539)
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, orderable_since 0 s, obtain in flight from a pass 0 s ago, unblocked 0 s | Rank 1: probe Direct, orderable_since none, no obtain, unblocked none
 9. [60 s] Rank 1: pass: its probe sees the hostname reach the Cluster directly (:484-490); issuance_gate answers Order, as the clock has passed (core/crates/ployz-core/src/domain/issuance.rs:77-86); the rank delay runs from this pass (core/crates/ployz-decide/src/issuance.rs:196-211), and issuance_action answers Nothing, as it has not passed (core/crates/ployz-decide/src/issuance.rs:204-207, core/crates/ployz-decide/src/issuance.rs:101-108), so the hostname is Waiting and not pushed; wake answers 30 s (core/crates/ployz-decide/src/issuance.rs:226-259)
    row: no material, reason Authority, an Authority clock, 1 failure, passed | policy BuiltIn | Rank 0: probe Direct, orderable_since 0 s, obtain in flight from a pass 0 s ago, unblocked 0 s | Rank 1: probe Direct, orderable_since 0 s, no obtain, unblocked 0 s
```

Some tests print a story before they fail, and some print none. To see the story of a property that breaks, put this test at the end of `mod tests`, before the file's last closing brace, with the model and property you want:

```rust
    #[test]
    fn scratch_story() {
        let model = without(Broken { hold_keeps_orderable_since: true, ..Broken::default() });
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

Each `Broken` flag is already set by its own test, so setting one in a test of your own changes no other test and turns none red. To check the flag table below, run `scratch_flag_kinds`, after it. To break a guard in a new test, set its flag through `without(..)`. That changes one model, so only the tests that set the flag see it. Add `--no-fail-fast` to the nextest command, so every red test is listed.

A failure prints the property's full string, not its constant's name:
- `RANK_DELAY_KEPT`: "a Machine of rank r pushes the hostname to to_order only r issuance steps or more after its first pass since its last blocked pass";
- `HOLD_KEPT`: "no pass pushes the hostname to to_order while its snapshot's clock holds and its verdict leaves the clock standing";
- `NO_ORDER_OVER_MATERIAL`: "no obtain orders while the row it re-reads holds material that is not due for renewal";
- `AUTHORITY_BACKOFF_KEPT`: "no Machine above rank 0 writes its refusal over an Authority clock, so none resets the Authority backoff to one failure".

A `COVERAGE` row prints its `name`.

Which kind of property each flag breaks, from a scratch run that checked each flagged model in full. It lists the `always` properties that break with the flag on the code as it is, and the rows the code reaches but the flagged model does not.

| Flag | `always` properties that break | `sometimes` rows left unreached |
|---|---|---|
| `hold_orders` | `HOLD_KEPT` | "the gate holds a Machine still in its rank delay" and "the gate holds an Order-due Machine" and "a held pass drops orderable_since" |
| `refusal_keeps_orderable_since` | `RANK_DELAY_KEPT` | none |
| `hold_keeps_orderable_since` | `RANK_DELAY_KEPT` | "a held pass drops orderable_since" |
| `early_return_keeps_orderable_since` | `RANK_DELAY_KEPT` | "a pass that stops before the gate drops orderable_since" |
| `error_clears_hosts` | none | "a call before the gate returns Err, and orderable_since stays" |
| `any_rank_records` | `AUTHORITY_BACKOFF_KEPT` | "rank 1 refuses and records nothing, and the row holds no reason" |
| `obtain_skips_recheck` | `NO_ORDER_OVER_MATERIAL` | "obtain's recheck finds material and returns" |

`error_clears_hosts` breaks no `always`, since a delay cleared on an Err only waits longer; the row its test expects goes unreached. The `ployz-decide` tests named under The change cover `wake`, and `WAKES_BY_THE_DELAY` holds the model to its answer for a `Waiting` hostname.

The scratch test that printed it follows. Put it at the end of `mod tests`, before the file's last closing brace, and run it alone; it took about 10 s of nextest time in release, after the build.

```rust
    #[test]
    fn scratch_flag_kinds() {
        let runs = [
            ("hold_orders", Broken { hold_orders: true, ..Broken::default() }),
            ("refusal_keeps_orderable_since", Broken { refusal_keeps_orderable_since: true, ..Broken::default() }),
            ("hold_keeps_orderable_since", Broken { hold_keeps_orderable_since: true, ..Broken::default() }),
            ("early_return_keeps_orderable_since", Broken { early_return_keeps_orderable_since: true, ..Broken::default() }),
            ("error_clears_hosts", Broken { error_clears_hosts: true, ..Broken::default() }),
            ("any_rank_records", Broken { any_rank_records: true, ..Broken::default() }),
            ("obtain_skips_recheck", Broken { obtain_skips_recheck: true, ..Broken::default() }),
        ];
        let baseline = check(as_is());
        for (name, broken) in runs {
            let model = without(broken);
            let checker = check(model);
            println!("{name}: {} states", checker.unique_state_count());
            for property in as_is().properties() {
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

An edit to the model's code changes every model at once. For this change, each edit below was made alone in the model at this change's head, run with `cd core && cargo nextest run -p ployz-verify acme_issuance --no-fail-fast`, and reverted.

| Edit, by function | Tests that went red |
|---|---|
| K01. `AcmeIssuance::pass`: every held pass orders along Direct: `IssuanceGate::Hold(_) if self.broken.hold_orders =>` replaced by `IssuanceGate::Hold(_) if true =>` | `a_held_pass_restarts_the_rank_delay`, `a_late_restart_ended_the_old_route`, `a_pass_on_each_edge_state`, `a_read_error_keeps_rank_1_s_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property`, `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` |
| K02. `AcmeIssuance::pass`: a refused pass keeps orderable_since: `machine.orderable_since = self.broken.refusal_keeps_orderable_since.then_some(age);` replaced by `machine.orderable_since = Some(age);` | `a_held_pass_restarts_the_rank_delay`, `a_pass_on_each_edge_state`, `a_read_error_keeps_rank_1_s_delay`, `a_refusal_restarts_the_rank_delay`, `the_code_keeps_every_property` |
| K03. `AcmeIssuance::pass`: refusal_keeps_orderable_since does nothing: `machine.orderable_since = self.broken.refusal_keeps_orderable_since.then_some(age);` replaced by `machine.orderable_since = None;` | `a_refusal_restarts_the_rank_delay` |
| K04. `AcmeIssuance::pass`: every rank records a refusal: `let record = if rank == 0 \|\| self.broken.any_rank_records {` replaced by `let record = if true {` | `a_pass_on_each_edge_state`, `a_read_error_keeps_rank_1_s_delay`, `a_refusal_restarts_the_rank_delay`, `only_rank_0_records_a_refusal`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K05. `AcmeIssuance::pass`: any_rank_records does nothing: `let record = if rank == 0 \|\| self.broken.any_rank_records {` replaced by `let record = if rank == 0 {` | `only_rank_0_records_a_refusal` |
| K06. `AcmeIssuance::obtain`: no recheck: `let rechecks = reread.material && !self.broken.obtain_skips_recheck;` replaced by `let rechecks = false;` | `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K07. `AcmeIssuance::pass`: a held pass keeps orderable_since: `let reset = !self.broken.hold_keeps_orderable_since;` replaced by `let reset = false;` | `a_pass_on_each_edge_state`, `a_read_error_keeps_rank_1_s_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `the_code_keeps_every_property`, `the_old_route_survived_a_late_recheck` |
| K08. `AcmeIssuance::pass`: hold_keeps_orderable_since does nothing: `let reset = !self.broken.hold_keeps_orderable_since;` replaced by `let reset = true;` | `a_held_pass_restarts_the_rank_delay`, `a_late_restart_ended_the_old_route`, `a_pass_on_each_edge_state`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` |
| K09. `early_return`: keeps `orderable_since`: `!self.broken.early_return_keeps_orderable_since && machine.orderable_since.take().is_some()` replaced by `false && machine.orderable_since.take().is_some()` | `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay`, `a_read_error_keeps_rank_1_s_delay`, `the_code_keeps_every_property` |
| K10. `early_return`: early_return_keeps_orderable_since does nothing: `!self.broken.early_return_keeps_orderable_since && machine.orderable_since.take().is_some()` replaced by `machine.orderable_since.take().is_some()` | `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay` |
| K11. `AcmeIssuance::pass`: RANK_DELAY_KEPT never fires: `early: usize::from(since) < rank,` replaced by `early: false,` | `a_held_pass_restarts_the_rank_delay`, `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay`, `a_refusal_restarts_the_rank_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` |
| K12. `AcmeIssuance::obtain`: the failure clock counts from the order's end: `let wall = now() - tick() * u32::from(order.ago);` replaced by `let wall = now();` | `a_read_error_keeps_rank_1_s_delay`, `an_order_s_clock_counts_from_its_pass`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K13. `AcmeIssuance::tick`: no clock passes: `clock.left = clock.left.saturating_sub(1);` deleted | `a_held_pass_restarts_the_rank_delay`, `a_late_restart_ended_the_old_route`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `an_order_s_clock_counts_from_its_pass`, `only_rank_0_records_a_refusal`, `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` |
| K14. `AcmeIssuance::pass`, the policy refusal: record_certificate_error writes over its own reason: `if state.row.last_error() == Some(Reason::Policy) {` replaced by `if false {` | `a_policy_refusal_restarts_rank_1_s_delay`, `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K15. `AcmeIssuance::pass`, material present: `machine.orderable_since = None;` before `block(machine);` deleted | `a_pass_on_each_edge_state` |
| K16. `AcmeIssuance::restart`: `machine.orderable_since = None;` deleted | `a_late_restart_ended_the_old_route`, `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K17. `AcmeIssuance::pass`, the Hold arm: its `block(machine);` deleted, so a held pass does not restart the property's count | `a_pass_on_each_edge_state` |
| K18. `Clock::holds`: its body `self.left > 0 && !(self.last == Failure::DoesNotResolve && verdict != Verdict::DoesNotResolve)` replaced by `self.left > 0`, so the model's reading of the gate ignores a verdict change | `a_pass_on_each_edge_state`, `a_read_error_keeps_rank_1_s_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K19. `actions`: no Err before the gate: `actions.push(Action::Errs(who));` deleted | `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K20. `AcmeIssuance::pass`, the Order arm: `machine.orderable_since = Some(age);` deleted, so an Order pass does not start orderable_since | `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay`, `a_read_error_keeps_rank_1_s_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `obtain_rechecks_material`, `the_code_keeps_every_property` |
| K21. `next_state`, `Action::Errs`: a read Err drops orderable_since: `let reset = self.broken.error_clears_hosts && {` replaced by `let reset = true && {` | `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| K22. `next_state`, `Action::Errs`: error_clears_hosts does nothing: `let reset = self.broken.error_clears_hosts && {` replaced by `let reset = false && {` | `a_read_error_keeps_rank_1_s_delay` |
| K23. `AcmeIssuance::pass`, the Refuse arm: no refusal counts as over an Authority clock: `over_authority: over_authority && record == Record::Wrote,` replaced by `over_authority: false,` | `only_rank_0_records_a_refusal` |

A side-effect failure is a test whose pinned path or expected rows your edit already moved, such as the old-code replays under K01. A guard that no longer earns its keep fails with its flag alone, on a model with no other edit.

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

Each of the 30 rows was made unreachable alone this way. Every row turned red the tests that expect it, and only those: `the_code_keeps_every_property` for every row it has; `the_code_as_it_was_breaks_only_the_rank_delay` for every row but the two that need a guard `AS_IT_WAS` takes out, "a held pass drops orderable_since" and "a pass that stops before the gate drops orderable_since"; and `a_read_error_keeps_rank_1_s_delay` for every row but "a call before the gate returns Err, and orderable_since stays", which its flag takes out.

## Bounds and blind spots

**Time, first.** One tick is 30 s, `issuance_step` under the built-in policy: `RANK_STEP` is 30 s, the 30 s the probe waits (`core/crates/ployz-decide/src/issuance.rs:17-19`), and `DEFAULT_PROBE_TIMEOUT` is 30 s (`core/crates/ployz-core/src/domain/certificate_policy.rs:26-27`). `DEFAULT_BACKOFF_BASE` is 60 s, 2 ticks, and the backoff doubles up to the 3600 s cap (`core/crates/ployz-core/src/domain/issuance.rs:102-106`). `a_tick_is_one_issuance_step` pins each of these.
- Every time is held relative to now, so the model needs no tick bound. `orderable_since` and `unblocked` are ages in ticks, capped at `AGE_CAP`, the largest rank delay, 1 tick: past it, every `action_due` answer and every property compare is the same. A clock holds its failure count, its last failure and the ticks left until `next_attempt_at`, zero once it has passed. An order in flight carries its snapshot clock's failures and kind and the ticks since its pass, because the failure record counts from the pass's `wall` (`:553`), not from when the order ends. That age is capped at `ago_cap`, the longest backoff a kept clock can hold, 120 s, 4 ticks: an order that old or older writes a clock that has already passed (`an_order_s_clock_counts_from_its_pass`).
- Every pass in a tick reads the same instant as its `now` and `wall` (`:476-477`), and the model rebuilds each `SystemTime` from it (`now()`). So the model explores runs whose passes land on the 30 s grid. Under the built-in policy every wait `wake` for a `Waiting` hostname or `run` asks for is a multiple of 30 s: `RANK_STEP`, a rank delay less the whole ticks elapsed, capped at `RETRY_INTERVAL` (`core/crates/ployz-decide/src/issuance.rs:241-245`), and `RETRY_INTERVAL`, 60 s (`:349-355`, `:473`). `wake` for a held or refused hostname asks for the time until the clock passes, which is a whole number of ticks only when the clock was written on the grid. What happens between grid points is left out: how long a pass, a probe or an order takes, and a pass a container change wakes off the grid (`:357-364`).
- No pass is fair. A container change wakes every Machine's loop (`:357-364`), so the model offers a pass at every tick, and also lets a Machine skip ticks. In the code a Machine skips ticks while a pass and its obtains run, and between passes for as long as `wake` (`:662-668`) or an error's `RETRY_INTERVAL` (`:349-355`) says. This over-approximates, which is safe for the holds tests. The old-code replays use only schedules a container change allows.
- A rank 0 that runs no loop is reached this way, by offering it no pass. Rank counts every `machines` row (`:430-435`), and `machines` keeps every row `accepts_machine` accepts, with no filter on Down or leaving rows (`core/crates/ployzd/src/corrosion/store.rs:719-733`), while the loop runs only after `wait_for_participation` (`core/crates/ployzd/src/daemon.rs:490-503`).

**The policy bound.** The model explores only the built-in policy's values (`CertificatePolicy::built_in`, `core/crates/ployz-core/src/domain/certificate_policy.rs:93-103`), plus the operator's refuse, no-directory and repair actions below. A probe timeout above 30 s makes `issuance_step` longer than `RANK_STEP` (`core/crates/ployz-decide/src/issuance.rs:56-58`), and a probe timeout or backoff base that is not a multiple of 30 s does not fit the tick, so neither is explored. The backoff cap is never reached: the longest kept backoff is 120 s.

**The bounds,** each with the test that needs its value. A drop, from a fresh run at this change's head, turns those tests red. The lifts were run by E-36 at `9a3a0708749bc98d14fda4f06025d4aa6acc43f3`, on the model before this change, and were not run again here.

| Bound | Value | What needs it | Dropped by one: tests that went red |
|---|---|---|---|
| `MACHINES` | 2 | every route needs a rank 1 | to 1: `a_held_pass_restarts_the_rank_delay`, `a_late_restart_ended_the_old_route`, `a_pass_on_each_edge_state`, `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay`, `a_read_error_keeps_rank_1_s_delay`, `a_refusal_restarts_the_rank_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `obtain_rechecks_material`, `only_rank_0_records_a_refusal`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property`, `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` |
| `MAX_FAILURES` | 2 | the row "a second failure of the same kind doubles the backoff" | to 1: `a_read_error_keeps_rank_1_s_delay`, `a_tick_is_one_issuance_step`, `an_order_s_clock_counts_from_its_pass`, `only_rank_0_records_a_refusal`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| `AGE_CAP` | `MACHINES - 1`, 1 tick | rank 1's delay | to 0: `a_held_pass_restarts_the_rank_delay`, `a_pass_on_each_edge_state`, `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay`, `a_read_error_keeps_rank_1_s_delay`, `a_refusal_restarts_the_rank_delay`, `a_verdict_change_after_a_refused_hold_restarts_rank_1_s_delay`, `an_authority_hold_restarts_rank_1_s_delay`, `obtain_rechecks_material`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property`, `the_old_route_survived_a_late_recheck`, `the_old_route_survived_an_early_restart` |
| `ago_cap` | `issuance_backoff(MAX_FAILURES, ..)`, 4 ticks | `an_order_s_clock_counts_from_its_pass` | to 3: `a_tick_is_one_issuance_step`, `an_order_s_clock_counts_from_its_pass` |
| The operator refuses the policy | at most once, from the built-in policy | `THE_POLICY_REFUSAL` and the policy rows | to never: `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay`, `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| The operator empties the directory | at most once, from the built-in policy | the row "with no directory, the pass returns before the gate" | to never: `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_read_error_keeps_rank_1_s_delay`, `the_code_as_it_was_breaks_only_the_rank_delay`, `the_code_keeps_every_property` |
| The operator repairs the policy | at most once | `THE_POLICY_REFUSAL` | to never: `a_pass_that_stops_before_the_gate_restarts_the_rank_delay`, `a_policy_refusal_restarts_rank_1_s_delay` |

A drop is one edit, then a run. `AGE_CAP` is `(MACHINES - 1) as u8`, so it moves with `MACHINES`; drop it alone by writing `(MACHINES - 2) as u8`.
- `MACHINES` and `MAX_FAILURES`: edit the constant.
- `ago_cap`, which is computed: append ` - 1` after the `))` that closes `ticks(issuance_backoff(..))` in `fn ago_cap`.
- The operator bounds live in `actions`, in its `match state.policy`. To drop refusing, delete `Action::Operator(Policy::Refused),` from the `Policy::BuiltIn` arm's `extend([...])`. To drop emptying, delete `Action::Operator(Policy::NoDirectory),` from it. To drop repair, delete the `actions.push(Action::Operator(Policy::Repaired));` in the `Policy::Refused | Policy::NoDirectory` arm.

To lift a bound, raise the constant, or let a repaired policy be refused or emptied again by replacing the `Policy::Repaired => {}` arm in `actions` with

```rust
            Policy::Repaired => actions.extend([
                Action::Operator(Policy::Refused),
                Action::Operator(Policy::NoDirectory),
            ]),
```

and run this test, put at the end of `mod tests`, before the file's last closing brace:

```rust
    #[test]
    fn scratch_lift() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        checker.assert_properties();
        let model = as_it_was();
        let checker = check(model);
        println!("as it was: {} states", checker.unique_state_count());
        assert!(checker.discovery(RANK_DELAY_KEPT).is_some());
        assert!(checker.discovery(HOLD_KEPT).is_none(), "{HOLD_KEPT} breaks");
        assert!(checker.discovery(NO_ORDER_OVER_MATERIAL).is_none(), "{NO_ORDER_OVER_MATERIAL} breaks");
        assert!(checker.discovery(AUTHORITY_BACKOFF_KEPT).is_none(), "{AUTHORITY_BACKOFF_KEPT} breaks");
        harness::assert_reached(COVERAGE, &model, &checker);
    }
```

```
cd core && cargo nextest run -p ployz-verify -E 'test(=acme_issuance::tests::scratch_lift)' --no-capture --release
```

Without an edit it prints `as is: 245640 states` and `as it was: 431688 states`. E-36's lift to three Machines peaked at about 11 GB of memory.

**Unbounded.** Each Machine's verdict flips without bound (`Action::Probe`), so a verdict can be older than the latest DNS change on one Machine and not the other (`core/CONTEXT.md:584-585`). Restarts, lost records and Errs before the gate are unbounded too. The model explores one refusal kind, DoesNotResolve, and one route, Direct.

**Each read, and why the model's steps reach the same states.** Every write between two reads of one Machine is another Machine's step.
- **The pass's `certificate_state`** (`:425`). The pass decides from it, and its only write is the refusal record, whose `write_acme_row` reads the row again (`core/crates/ployzd/src/corrosion/store.rs:1089-1106`). Between the two, another Machine can publish material, record a failure or record the policy's reason. If material lands, the record writes nothing (`core/crates/ployzd/src/corrosion/store.rs:1189-1190`), and the model reaches the same row by taking the pass first and the publish after, since `issued` replaces the whole row (`core/crates/ployzd/src/corrosion/certificate.rs:259-261`). If another record lands, the pass's record writes over it with the clock from its own snapshot. The model reaches that row by taking the pass first and losing the other record, which is why every record kind has a lost outcome.
- **The policy refusal's reads** (`:400`, `:405-406`) decide only which hostnames are wanted. `record_certificate_error` then reads the row through `write_acme_row`, compares its reason, and writes (`core/crates/ployzd/src/corrosion/store.rs:1237-1250`). The model takes it as one step.
- **`obtain`'s `certificate_row`** (`:627`). Between it and `publish_certificate`'s read (`core/crates/ployzd/src/corrosion/store.rs:978-985`), other Machines can write anything. `publish_certificate` writes `issued` over whatever it reads, unless the row already equals it (`core/crates/ployzd/src/corrosion/store.rs:980-981`) or is Published (`acme_row`, `core/crates/ployzd/src/corrosion/store.rs:1053-1056`), so the row ends `issued` either way, the row the model reaches by taking the obtain last. Two orders that both re-read no material reach the same end states as the model's: an issued order and a failed one end with the material (the failure record writes nothing once material landed, and `issued` replaces a record that landed first), and two issued orders end with material, which the model reaches with the second obtain's recheck.
- **The failure record's read** through `write_acme_row`. The record takes its clock from the pass's snapshot (`:551`), not from this read, and the model's obtain does too (`Order::snapshot`). So the model explores a record written from a stale snapshot over a newer row.
- **Inside each writer**, `write_acme_row` reads the row with `acme_row` and writes with `compare_and_set` only over the body text it read (`core/crates/ployzd/src/corrosion/store.rs:1089-1106`, `:1112` of that file). A write that lands on this replica between the read and the write makes the writer read again, up to `ACME_WRITE_ATTEMPTS` reads (`core/crates/ployzd/src/corrosion/store.rs:1788`), and a re-read can still answer `Kept` or `Changed`. A writer that misses on every read answers `Moved` with nothing written, which the call sites log (`:413-415`, `:505-510`, `:587-592`, `:650-652`, `:669-671`). `record_certificate_failure` with tried material writes with a conditional `json_set` (`core/crates/ployzd/src/corrosion/store.rs:1197-1227`). On the model's one row, which every Machine reads as soon as it is written, the compare always matches, so a writer that answers `Moved` is not modelled.
- **Each Machine reads its own Corrosion replica.** The model's one row, which every Machine reads as soon as it is written, leaves out a read of an older row.

**Code-as-is facts, and what the model does with each.**
- **A daemon restart** empties `hosts` (`:322`) and ends an obtain in flight: modelled, `Action::Restart`.
- **`record_certificate_failure` and `record_certificate_error` can return Err**, which is only logged for a refusal or a failed order (`:511-514`, `:593-595`) and returned for the policy refusal (`:408-410`): modelled, each as a lost outcome.
- **An Err before the gate**: modelled, `Action::Errs`.
- **`account` can fail after the push** (`:527`). `issue_wanted` then returns Err before the obtains and before `wake`: no obtain runs, no clock is written, `hosts` keeps `Order`, and `run` waits `RETRY_INTERVAL` (`:349-355`). The model's `Ends::Failed { recorded: false }` reaches the same row and Machine state, and `Ends::Rechecked` the same where the re-read holds material.
- **A policy refusal or a missing directory** stops the pass before the gate, and `issue_wanted` clears `hosts` (`:404-423`, `:471-474`): modelled, with `Action::Operator`. The policy refusal's `record_certificate_error` replaces the row's refusal with a reason and no clock (`with_error`, `core/crates/ployzd/src/corrosion/certificate.rs:356-364`), unless the row's `last_error` already equals that reason (`core/crates/ployzd/src/corrosion/store.rs:1243-1244`). So it drops the shared clock from a row whose recorded reason differs. `with_refusal` keeps the material and the challenge (`core/crates/ployzd/src/corrosion/certificate.rs:366-379`).
- **The hostname can leave the wanted set, or a Published cover can appear** (`:426`, `:475`). Each prunes the hostname from `hosts` at the next pass, which then has no work for it. That changes the Machine's state as a restart does, without ending an obtain. When no hostname is left, `read_pass` answers `None` (`:427-429`) and `hosts` is cleared.

**Left out**, with nothing in the model that stands for it:
- **Renewals,** where material is present. A renewal's delay is measured from each Machine's own `renew_at` (with its `machine_jitter`, `:62-74`) plus the rank delay (`core/crates/ployz-decide/src/issuance.rs:80-94`), not from `orderable_since`, and a hold does not move it: `step` answers `Renew` as soon as the gate opens (`core/crates/ployz-decide/src/issuance.rs:192-194`). So every Machine whose renewal came due during a hold is Renew-due when the gate opens, and this change does not touch that.
- **Risk 2**, the `RANK_STEP` margin over the 30 s the probe waits, above.
- **The challenge slot.** With an atomic `obtain`, no other step sees a challenge, so two challenges overwriting each other is argued from the code, not shown by the model.
- **More than one hostname.** Each hostname has its own `hosts` entry and row. Other hostnames change only when this Machine's next pass comes, through `join_all`, `account` and the least `wake`, which the unfair schedule already covers.
- **Membership changes and policy edits that change the delay.** Each Machine's rank is fixed, and so is the built-in `probe_timeout`. A lower-id Machine that joins, or a longer `probe_timeout`, lengthens the delay of a Machine already in `Order`; `step` checks `Order`'s start against the pass's own rank and policy (`core/crates/ployz-decide/src/issuance.rs:195-211`), and the `ployz-decide` test `a_rank_that_rises_after_an_order_waits_its_new_delay` is the proof.
- **Ticks shorter than a hold.** The model offers every Machine a pass every tick, and a tick is `RANK_STEP`, so it cannot show a Machine that sleeps through a whole hold. A `Waiting` hostname asks the loop to sleep no longer than `RANK_STEP`, so a Machine whose loop keeps that sleep passes during a hold of a step or more; the `ployz-decide` test `a_waiting_rank_meets_a_hold_of_one_step` is the proof. A hold shorter than a step, from a backoff base under 30 s, can still pass between two of a Machine's passes, and so can a longer one while the loop waits `RETRY_INTERVAL` after an Err or runs its obtains for another hostname; then that Machine orders on the delay it had. A Machine that does pass during a hold waits its rank delay from its own first open pass after it, which spaces it from no other Machine (What the property is not, above).
- The other refusals (Unreachable, RedirectsToHttps, ReachesElsewhere) and the ViaProxy route. So two Machines whose probes refuse for different reasons are not explored (Only rank 0 records, above).

A green model says nothing about them. Cover the code ones with:

```
cd core && cargo nextest run -p ployz-core issuance
cd core && cargo nextest run -p ployzd certificates
```

Expected: `9 tests run: 9 passed` for `ployz-core issuance`, and `41 tests run: 41 passed` for `ployzd certificates`.

## Keep the model in step

When a change alters when a pass pushes a hostname, what it records, or what `obtain` reads, change the model in the same PR. Examples: a new guard before the gate, a new `Issuance` state or transition, a new early return.

1. Update the function that copies the product function you changed, and the `reads_as` arm that prints it:
   - `read_pass`, `issue_wanted`, `advance` and `step`: `AcmeIssuance::pass`, `early_return`, and the `Action::Errs` arm of `next_state`;
   - `obtain` and the failure record: `AcmeIssuance::obtain` and `record_failure`;
   - `run`: `AcmeIssuance::restart`, and the offers in `actions`;
   - `action_due`, `wake`, `rank_delay`, `issuance_step` and `machine_rank`: `AcmeIssuance::pass`, which calls them, and `wait_words`.
2. If the change adds a guard, add a `Broken` flag that removes it, and a test naming the property it keeps.
3. If the change fixes a finding, model it as the code writes it and make it the default. Then add the old code as `Broken` flags, and keep the finding tests as replays on that model, since their printed paths are the record of what the fix keeps out.
4. If the change adds a branch, add a `COVERAGE` row whose predicate requires the branch it names and reads the field the name claims, and give it the `on` of the models that take it, `everywhere` for every model.
5. Every `State` field is read: `row`, `policy`, each Machine's `verdict`, `orderable_since`, `obtaining` and `unblocked` (by `RANK_DELAY_KEPT`), and `took` (by the properties and rows). A field that is only written still splits states, so remove one that nothing reads.
6. Move the sync commit below forward only after you confirmed each covered function still reads as the model says.

## Coverage

Each model step is a copy of code at the sync commit, or a call into `ployz-decide`. Reread these when a diff touches them:

- `core/crates/ployzd/src/certificates.rs`: `Host`, `held`, `advance`; `run`; `read_pass`; `issue_wanted`; `obtain`; `wanted_certificate_hosts`.
- `core/crates/ployz-decide/src/issuance.rs`: `RETRY_INTERVAL`, `RANK_STEP` and its doc comment; `machine_rank`, `rank_delay`, `issuance_step` and its doc comment, `action_due` and `issuance_action`; `Issuance`, `issued`, `step` and its doc comment, and `wake`.
- `core/crates/ployz-core/src/domain/issuance.rs`: `IssuanceClock`, `IssuanceGate`, `issuance_gate`, `issuance_backoff` and `issuance_failure_clock`.
- `core/crates/ployz-core/src/domain/certificate_policy.rs`: `DEFAULT_BACKOFF_BASE`, `DEFAULT_BACKOFF_CAP`, `DEFAULT_PROBE_TIMEOUT`, `CertificatePolicy::built_in` and `resolve_certificate_policy`.
- `core/crates/ployzd/src/corrosion/store.rs`, the ACME writers: `machines`, `publish_certificate`, `acme_row`, `stored_row`, `write_acme_row`, `compare_and_set`, `publish_certificate_challenge`, `record_certificate_failure` and `record_certificate_error`.
- `core/crates/ployzd/src/corrosion/certificate.rs`: `RecordedRefusal`, `CertificateRow`, `issued`, `with_backoff`, `clock`, `last_error`, `with_challenge`, `with_error` and `with_refusal`.
- `core/crates/ployzd/src/daemon.rs`: the certificate loop's start after `wait_for_participation`.
- `core/crates/ployzd/src/runtime_watch.rs`: `redact_certificate`.

Sync commit: `main` at `8307a523654f3c99eee0685a4e7f790088a375e7` with the change that introduced `HostIssuance` on top, and every line cited above is at that change's head. Every function in the list above was reread at it.
