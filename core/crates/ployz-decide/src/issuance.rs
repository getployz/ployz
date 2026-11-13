//! When each Machine's certificate loop orders or renews a certificate for an Ingress
//! Hostname: the rank among Machines, the rank delay, and the state a pass leaves behind.
//!
//! Every function here is pure over plain facts. `ployzd` reads the row, probes the hostname
//! and keeps the clock; the verify model for ACME issuance calls the same functions.

use std::time::{Duration, SystemTime};

use ployz_core::{
    CertificatePolicy, ClusterRoute, IssuanceClock, IssuanceFailure, IssuanceGate, Refusal,
};

/// How long the certificate loop sleeps at most between passes while no clock holds the
/// hostname.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(60);

/// The rank delay's step. It must cover the 30 s ployzd waits for an HTTP-01 challenge to be
/// served, so rank 1 cannot write a competing token while rank 0 is still probing.
pub const RANK_STEP: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IssuanceAction {
    Nothing,
    Order,
    Renew,
}

/// What the hostname's row holds that scheduling reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Held {
    /// No Certificate Material, or material that does not parse.
    Nothing,
    /// Certificate Material, to renew at `renew_at`, which is `None` when the certificate's
    /// validity cannot be read.
    Material { renew_at: Option<SystemTime> },
}

/// Rank among Machine identifiers. Lowest id is 0 and may order immediately.
#[must_use]
pub fn machine_rank<'id, T: Ord + 'id>(
    this: &T,
    machines: impl IntoIterator<Item = &'id T>,
) -> usize {
    machines.into_iter().filter(|id| *id < this).count()
}

/// How long a Machine of `rank` waits, a `step` per rank, before it orders.
#[must_use]
pub fn rank_delay(rank: usize, step: Duration) -> Duration {
    step.saturating_mul(u32::try_from(rank).unwrap_or(u32::MAX))
}

/// One rank step under `policy`: a later rank cannot start a competing order while an earlier
/// rank is still presenting, so the step is at least the probe timeout.
#[must_use]
pub fn issuance_step(policy: &CertificatePolicy) -> Duration {
    RANK_STEP.max(policy.probe_timeout())
}

/// Whether this Machine should order or renew now, and how long until it should.
#[must_use]
pub fn action_due(
    held: Held,
    rank: usize,
    elapsed: Duration,
    now: SystemTime,
    policy: &CertificatePolicy,
) -> (IssuanceAction, Duration) {
    let delay = rank_delay(rank, issuance_step(policy));
    match held {
        Held::Nothing => {
            let wait = delay.saturating_sub(elapsed);
            let action = if wait.is_zero() {
                IssuanceAction::Order
            } else {
                IssuanceAction::Nothing
            };
            (action, wait)
        }
        Held::Material {
            renew_at: Some(renew_at),
        } => {
            let wait = renew_at
                .checked_add(delay)
                .unwrap_or(renew_at)
                .duration_since(now)
                .unwrap_or(Duration::ZERO);
            let action = if wait.is_zero() {
                IssuanceAction::Renew
            } else {
                IssuanceAction::Nothing
            };
            (action, wait)
        }
        Held::Material { renew_at: None } => (IssuanceAction::Nothing, RETRY_INTERVAL),
    }
}

/// `action_due`'s action alone.
#[must_use]
pub fn issuance_action(
    held: Held,
    rank: usize,
    elapsed: Duration,
    now: SystemTime,
    policy: &CertificatePolicy,
) -> IssuanceAction {
    action_due(held, rank, elapsed, now, policy).0
}

/// This Machine's view of one wanted Ingress Hostname after a pass of its certificate loop.
/// It lives only in the loop's memory: each Machine keeps its own, from its own row, verdict
/// and rank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Issuance {
    /// The row holds Certificate Material whose renewal is not due before `renew_due`.
    Issued { renew_due: SystemTime },
    /// The gate let the pass through, and the rank delay has not passed.
    Waiting,
    /// The shared backoff clock holds the hostname.
    Held(IssuanceClock),
    /// This Machine's Hostname Verdict refuses the hostname, and `clock` is the backoff it
    /// records.
    Refused {
        refusal: Refusal,
        clock: IssuanceClock,
    },
    /// Order a first certificate along the route, the rank delay passed.
    Order(ClusterRoute),
    /// Renew along the route.
    Renew(ClusterRoute),
}

/// `Issued` when the row holds material whose renewal is not due, so the pass needs no probe.
#[must_use]
pub fn issued(
    held: Held,
    rank: usize,
    wall: SystemTime,
    policy: &CertificatePolicy,
) -> Option<Issuance> {
    if held == Held::Nothing {
        return None;
    }
    match action_due(held, rank, Duration::ZERO, wall, policy) {
        (IssuanceAction::Nothing, wait) => Some(Issuance::Issued {
            renew_due: wall.checked_add(wait).unwrap_or(wall),
        }),
        (IssuanceAction::Order | IssuanceAction::Renew, _) => None,
    }
}

/// A hostname's state after this Machine's last pass, and how long it has been orderable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Prev {
    pub issuance: Issuance,
    /// Time since this Machine's first pass through the gate that no hold or refusal has
    /// ended since. Only a `Waiting` or `Order` state counts it.
    pub orderable_for: Duration,
}

/// What one pass read: the gate's answer on the row's clock and this Machine's Hostname
/// Verdict, the row's material, this Machine's rank, and the time.
#[derive(Clone, Copy, Debug)]
pub struct Pass<'policy> {
    pub gate: IssuanceGate,
    pub held: Held,
    pub rank: usize,
    pub wall: SystemTime,
    pub policy: &'policy CertificatePolicy,
}

/// The next state of a hostname, and whether it keeps the start of the rank delay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stepped {
    pub issuance: Issuance,
    /// The delay keeps the start the previous state had. Otherwise it starts with this pass.
    pub keeps_since: bool,
}

/// The next state of a hostname this pass probed. A hold or a refusal ends the rank delay, so
/// the first pass the gate lets through after one starts it again.
#[must_use]
pub fn step(prev: Option<Prev>, pass: Pass<'_>) -> Stepped {
    let route = match pass.gate {
        IssuanceGate::Hold(clock) => return restarted(Issuance::Held(clock)),
        IssuanceGate::Refuse { refusal, clock } => {
            return restarted(Issuance::Refused { refusal, clock });
        }
        IssuanceGate::Order(route) => route,
    };
    if pass.held != Held::Nothing {
        return restarted(Issuance::Renew(route));
    }
    // An order keeps its start, so a rank or a policy that changed since asks for its own
    // delay.
    let (elapsed, keeps_since) = match prev {
        Some(Prev {
            issuance: Issuance::Waiting | Issuance::Order(_),
            orderable_for,
        }) => (orderable_for, true),
        Some(_) | None => (Duration::ZERO, false),
    };
    let issuance = match issuance_action(pass.held, pass.rank, elapsed, pass.wall, pass.policy) {
        IssuanceAction::Order | IssuanceAction::Renew => Issuance::Order(route),
        IssuanceAction::Nothing => Issuance::Waiting,
    };
    Stepped {
        issuance,
        keeps_since,
    }
}

fn restarted(issuance: Issuance) -> Stepped {
    Stepped {
        issuance,
        keeps_since: false,
    }
}

/// How long the loop may sleep before `state` wants another pass. A clock that holds wakes
/// the loop when it passes, and no later than its next probe: `RANK_STEP` after a refusal,
/// which a changed verdict lifts early, and `RETRY_INTERVAL` after an authority failure, which
/// only time lifts.
#[must_use]
pub fn wake(
    state: Issuance,
    rank: usize,
    orderable_for: Duration,
    wall: SystemTime,
    policy: &CertificatePolicy,
) -> Duration {
    match state {
        Issuance::Issued { renew_due } => renew_due
            .duration_since(wall)
            .unwrap_or(Duration::ZERO)
            .min(RETRY_INTERVAL),
        // Asks to sleep at most one step, so it meets a hold of a step or more while the loop
        // keeps that sleep. A read Err's 60 s wait, or another hostname's obtain, can miss the
        // hold.
        Issuance::Waiting => match action_due(Held::Nothing, rank, orderable_for, wall, policy) {
            (IssuanceAction::Order | IssuanceAction::Renew, _) => RANK_STEP,
            (IssuanceAction::Nothing, due_in) => due_in.min(RETRY_INTERVAL),
        }
        .min(RANK_STEP),
        Issuance::Held(clock) | Issuance::Refused { clock, .. } => {
            let probe = match clock.last_failure() {
                IssuanceFailure::Refused(_) => RANK_STEP,
                IssuanceFailure::Authority => RETRY_INTERVAL,
            };
            clock
                .next_attempt_at()
                .duration_since(wall)
                .unwrap_or(Duration::ZERO)
                .min(probe)
        }
        Issuance::Order(_) | Issuance::Renew(_) => RANK_STEP,
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use ployz_core::resolve_certificate_policy;

    use super::*;

    fn policy() -> CertificatePolicy {
        CertificatePolicy::built_in(None)
    }

    fn wall() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_000_000)
    }

    fn seconds(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn reaches() -> IssuanceGate {
        IssuanceGate::Order(ClusterRoute::Direct)
    }

    fn authority(next_attempt_at: SystemTime) -> IssuanceClock {
        IssuanceClock::new(1, next_attempt_at, IssuanceFailure::Authority)
    }

    fn unresolved(next_attempt_at: SystemTime) -> IssuanceClock {
        IssuanceClock::new(
            1,
            next_attempt_at,
            IssuanceFailure::Refused(Refusal::DoesNotResolve),
        )
    }

    fn pass(gate: IssuanceGate, held: Held, rank: usize) -> Pass<'static> {
        static POLICY: std::sync::LazyLock<CertificatePolicy> = std::sync::LazyLock::new(policy);
        Pass {
            gate,
            held,
            rank,
            wall: wall(),
            policy: &POLICY,
        }
    }

    fn waiting_for(orderable_for: Duration) -> Option<Prev> {
        Some(Prev {
            issuance: Issuance::Waiting,
            orderable_for,
        })
    }

    fn action(held: Held, rank: usize, elapsed: Duration) -> IssuanceAction {
        issuance_action(held, rank, elapsed, wall(), &policy())
    }

    fn lifetime_renewal(renew_at: SystemTime) -> Held {
        Held::Material {
            renew_at: Some(renew_at),
        }
    }

    #[test]
    fn the_lowest_id_is_rank_zero() {
        assert_eq!(machine_rank(&1, &[2, 1]), 0);
        assert_eq!(machine_rank(&2, &[1]), 1);
        assert_eq!(machine_rank(&1, &[]), 0);
        assert_eq!(machine_rank(&5, &[1, 2, 3, 9]), 3);
    }

    #[test]
    fn a_rank_waits_a_step_per_rank_before_it_orders() {
        assert_eq!(RANK_STEP, seconds(30));
        for (rank, delay) in [(0, 0), (1, 30), (2, 60), (3, 90), (7, 210)] {
            assert_eq!(rank_delay(rank, RANK_STEP), seconds(delay), "rank {rank}");
            let almost = seconds(delay).saturating_sub(Duration::from_millis(1));
            assert_eq!(
                action(Held::Nothing, rank, almost),
                if delay == 0 {
                    IssuanceAction::Order
                } else {
                    IssuanceAction::Nothing
                },
                "rank {rank} just before its delay"
            );
            assert_eq!(
                action(Held::Nothing, rank, seconds(delay)),
                IssuanceAction::Order,
                "rank {rank} at its delay"
            );
        }
        assert_eq!(rank_delay(usize::MAX, Duration::MAX), Duration::MAX);
    }

    #[test]
    fn a_probe_timeout_longer_than_a_step_lengthens_the_rank_delay() {
        let long = resolve_certificate_policy(
            Some(r#"{"probe_timeout":120}"#),
            &CertificatePolicy::built_in(None),
        )
        .unwrap();
        assert_eq!(issuance_step(&policy()), RANK_STEP);
        assert_eq!(issuance_step(&long), seconds(120));
        assert_eq!(
            issuance_action(Held::Nothing, 1, seconds(30), wall(), &long),
            IssuanceAction::Nothing
        );
        assert_eq!(
            issuance_action(Held::Nothing, 1, seconds(120), wall(), &long),
            IssuanceAction::Order
        );
    }

    #[test]
    fn material_that_does_not_parse_is_left_alone_for_a_minute() {
        assert_eq!(
            action_due(
                Held::Material { renew_at: None },
                0,
                Duration::ZERO,
                wall(),
                &policy()
            ),
            (IssuanceAction::Nothing, RETRY_INTERVAL)
        );
    }

    #[test]
    fn a_renewal_waits_its_rank_delay_after_the_machines_own_jitter() {
        let due = wall() + seconds(3600);
        let held = lifetime_renewal(due);
        let renews = |rank, at| issuance_action(held, rank, Duration::ZERO, at, &policy());
        assert_eq!(renews(0, due - seconds(1)), IssuanceAction::Nothing);
        assert_eq!(renews(0, due), IssuanceAction::Renew);
        assert_eq!(renews(1, due), IssuanceAction::Nothing);
        assert_eq!(
            renews(1, due + RANK_STEP - Duration::from_millis(1)),
            IssuanceAction::Nothing
        );
        assert_eq!(renews(1, due + RANK_STEP), IssuanceAction::Renew);
        assert_eq!(renews(2, due + RANK_STEP), IssuanceAction::Nothing);
        assert_eq!(renews(2, due + RANK_STEP * 2), IssuanceAction::Renew);
        assert_eq!(
            action_due(held, 0, Duration::ZERO, due - seconds(10), &policy()),
            (IssuanceAction::Nothing, seconds(10))
        );
    }

    #[test]
    fn material_is_issued_until_its_renewal_is_due() {
        let due = wall() + seconds(10);
        assert_eq!(
            issued(lifetime_renewal(due), 0, wall(), &policy()),
            Some(Issuance::Issued { renew_due: due })
        );
        assert_eq!(issued(lifetime_renewal(due), 0, due, &policy()), None);
        assert_eq!(issued(Held::Nothing, 0, wall(), &policy()), None);
        assert_eq!(
            issued(Held::Material { renew_at: None }, 0, wall(), &policy()),
            Some(Issuance::Issued {
                renew_due: wall() + RETRY_INTERVAL
            })
        );
    }

    #[test]
    fn a_renewal_goes_through_the_gate() {
        let renew = Held::Material {
            renew_at: Some(wall()),
        };
        assert_eq!(
            step(None, pass(reaches(), renew, 0)).issuance,
            Issuance::Renew(ClusterRoute::Direct)
        );
        let clock = authority(wall() + seconds(60));
        assert_eq!(
            step(None, pass(IssuanceGate::Hold(clock), renew, 0)).issuance,
            Issuance::Held(clock)
        );
    }

    #[test]
    fn a_hold_or_a_refusal_starts_the_rank_delay_again_at_the_next_open_gate() {
        let refused = Issuance::Refused {
            refusal: Refusal::DoesNotResolve,
            clock: unresolved(wall()),
        };
        for after in [
            None,
            Some(Prev {
                issuance: Issuance::Held(authority(wall())),
                orderable_for: seconds(600),
            }),
            Some(Prev {
                issuance: refused,
                orderable_for: seconds(600),
            }),
        ] {
            assert_eq!(
                step(after, pass(reaches(), Held::Nothing, 1)),
                Stepped {
                    issuance: Issuance::Waiting,
                    keeps_since: false
                },
                "rank 1 after {after:?}"
            );
            assert_eq!(
                step(after, pass(reaches(), Held::Nothing, 0)),
                Stepped {
                    issuance: Issuance::Order(ClusterRoute::Direct),
                    keeps_since: false
                },
                "rank 0 owes no delay after {after:?}"
            );
        }
        assert_eq!(
            step(waiting_for(seconds(600)), pass(reaches(), Held::Nothing, 1)),
            Stepped {
                issuance: Issuance::Order(ClusterRoute::Direct),
                keeps_since: true
            },
            "a Waiting state keeps its start while the gate stays open"
        );
    }

    #[test]
    fn the_rank_delay_runs_from_the_first_open_pass() {
        let waiting = |orderable_for| {
            step(
                waiting_for(orderable_for),
                pass(reaches(), Held::Nothing, 1),
            )
        };
        assert_eq!(
            waiting(RANK_STEP - Duration::from_millis(1)),
            Stepped {
                issuance: Issuance::Waiting,
                keeps_since: true
            }
        );
        let ordered = Stepped {
            issuance: Issuance::Order(ClusterRoute::Direct),
            keeps_since: true,
        };
        assert_eq!(waiting(RANK_STEP), ordered);
        let order = Some(Prev {
            issuance: Issuance::Order(ClusterRoute::Direct),
            orderable_for: RANK_STEP,
        });
        assert_eq!(
            step(order, pass(reaches(), Held::Nothing, 1)),
            ordered,
            "an order whose failure left no clock is retried at once, its delay spent"
        );
    }

    /// An order keeps the start of its rank delay, and each open pass checks it against the
    /// current rank, so a Machine whose rank rose waits out the longer delay.
    #[test]
    fn a_rank_that_rises_after_an_order_waits_its_new_delay() {
        let order = |orderable_for| {
            Some(Prev {
                issuance: Issuance::Order(ClusterRoute::Direct),
                orderable_for,
            })
        };
        assert_eq!(
            step(order(RANK_STEP), pass(reaches(), Held::Nothing, 2)),
            Stepped {
                issuance: Issuance::Waiting,
                keeps_since: true
            },
            "a lower-id Machine joined, so rank 1 became rank 2"
        );
        assert_eq!(
            step(order(RANK_STEP * 2), pass(reaches(), Held::Nothing, 2)),
            Stepped {
                issuance: Issuance::Order(ClusterRoute::Direct),
                keeps_since: true
            }
        );
    }

    /// A Waiting hostname asks to sleep no longer than one step, so it meets a hold of a step or
    /// more and starts the delay again while the loop keeps that sleep. A 60 s wait after a read
    /// Err, or a loop busy with another hostname's obtain, can miss the hold. Rank 2 first
    /// passes at 1 s. Rank 0's order fails at 2 s, and its clock holds until 60 s. Rank 2 wakes
    /// at 31 s, inside the hold, and then waits its two steps from 60 s, rather than ordering
    /// at 61 s beside rank 0.
    #[test]
    fn a_waiting_rank_meets_a_hold_of_one_step() {
        let hold = authority(wall() + seconds(60));
        let at = |secs| Pass {
            wall: wall() + seconds(secs),
            ..pass(reaches(), Held::Nothing, 2)
        };

        let first = step(None, at(1));
        assert_eq!(first.issuance, Issuance::Waiting);
        let sleep = wake(first.issuance, 2, Duration::ZERO, at(1).wall, &policy());
        assert_eq!(sleep, RANK_STEP, "rank 2 slept through the hold");

        let held = Pass {
            gate: IssuanceGate::Hold(hold),
            ..at(1 + sleep.as_secs())
        };
        let second = step(
            Some(Prev {
                issuance: first.issuance,
                orderable_for: sleep,
            }),
            held,
        );
        assert_eq!(second.issuance, Issuance::Held(hold));
        let sleep = wake(second.issuance, 2, Duration::ZERO, held.wall, &policy());
        assert_eq!(sleep, seconds(29));

        assert_eq!(
            step(
                Some(Prev {
                    issuance: second.issuance,
                    orderable_for: seconds(29),
                }),
                at(60)
            ),
            Stepped {
                issuance: Issuance::Waiting,
                keeps_since: false
            },
            "rank 2 orders beside rank 0"
        );
    }

    #[test]
    fn a_clock_that_holds_ends_the_rank_delay() {
        let later = wall() + seconds(60);
        let held = |clock| Stepped {
            issuance: Issuance::Held(clock),
            keeps_since: false,
        };
        assert_eq!(
            step(
                waiting_for(seconds(600)),
                pass(IssuanceGate::Hold(authority(later)), Held::Nothing, 1)
            ),
            held(authority(later))
        );
        assert_eq!(
            step(
                waiting_for(seconds(600)),
                pass(IssuanceGate::Hold(unresolved(later)), Held::Nothing, 1)
            ),
            held(unresolved(later))
        );
        assert_eq!(
            step(
                Some(Prev {
                    issuance: Issuance::Held(unresolved(later)),
                    orderable_for: seconds(600),
                }),
                pass(reaches(), Held::Nothing, 1)
            ),
            Stepped {
                issuance: Issuance::Waiting,
                keeps_since: false
            },
            "a changed verdict lifts a refusal's clock early, and the delay starts then"
        );
    }

    #[test]
    fn a_refusal_carries_the_clock_to_record() {
        let clock = unresolved(wall() + policy().backoff_base());
        for rank in [0, 1, 2] {
            assert_eq!(
                step(
                    None,
                    pass(
                        IssuanceGate::Refuse {
                            refusal: Refusal::DoesNotResolve,
                            clock
                        },
                        Held::Nothing,
                        rank
                    )
                )
                .issuance,
                Issuance::Refused {
                    refusal: Refusal::DoesNotResolve,
                    clock
                },
                "rank {rank}"
            );
        }
    }

    #[test]
    fn a_held_hostname_wakes_when_its_clock_passes_or_at_its_next_probe() {
        let wake_of = |state, rank| wake(state, rank, Duration::ZERO, wall(), &policy());
        let held = |left| Issuance::Held(authority(wall() + left));
        assert_eq!(wake_of(held(seconds(10)), 1), seconds(10));
        assert_eq!(wake_of(held(seconds(3600)), 1), RETRY_INTERVAL);
        assert_eq!(wake_of(held(Duration::ZERO), 1), Duration::ZERO);
        assert_eq!(
            wake_of(Issuance::Held(unresolved(wall() + seconds(10))), 1),
            seconds(10)
        );
        assert_eq!(
            wake_of(Issuance::Held(unresolved(wall() + seconds(3600))), 2),
            RANK_STEP,
            "a changed verdict lifts a refusal's clock, so it is probed as often as before"
        );
        assert_eq!(
            wake_of(
                Issuance::Refused {
                    refusal: Refusal::DoesNotResolve,
                    clock: unresolved(wall() + seconds(3600)),
                },
                2
            ),
            RANK_STEP
        );
        assert_eq!(wake_of(Issuance::Waiting, 2), RANK_STEP);
        assert_eq!(wake_of(Issuance::Order(ClusterRoute::Direct), 1), RANK_STEP);
        assert_eq!(wake_of(Issuance::Renew(ClusterRoute::Direct), 1), RANK_STEP);
        assert_eq!(
            wake_of(
                Issuance::Issued {
                    renew_due: wall() + seconds(10)
                },
                0
            ),
            seconds(10)
        );
    }

    /// A Waiting hostname wakes no later than its rank delay ends, so the loop does not sleep
    /// past the pass that orders.
    #[test]
    fn a_waiting_hostname_wakes_by_the_end_of_its_rank_delay() {
        for rank in 1..6 {
            let delay = rank_delay(rank, issuance_step(&policy()));
            for elapsed in (0..delay.as_secs()).step_by(7) {
                let elapsed = seconds(elapsed);
                assert!(
                    wake(Issuance::Waiting, rank, elapsed, wall(), &policy()) <= delay - elapsed,
                    "rank {rank} after {elapsed:?}"
                );
            }
        }
    }
}
