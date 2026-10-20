//! Golden fixtures for the lines the CLI prints for a Drain report.

use ployz_core::{
    Machine, MachineId, MachineName, QualifiedService, RpcErrorCode, WireGuardPublicKey,
};

use super::{incomplete, line, remaining};
use crate::drain::{
    DrainOutcome, DrainReport, DrainStop, MachineRef, Move, MoveFailure, OldCopyEnd, Remaining,
    ServiceDrain, ServicesRole, StayReason, Unconfirmed,
};

fn server(hex: char, name: &str) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
            .parse()
            .unwrap(),
        public_key: WireGuardPublicKey([hex as u8; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

fn service(name: &str) -> QualifiedService {
    QualifiedService::parse(format!("app/{name}")).unwrap()
}

fn step(to: &Machine, old: OldCopyEnd) -> Move {
    Move {
        from: MachineRef::from(&server('b', "web-2")),
        to: MachineRef::from(to),
        old,
    }
}

fn entry(name: &str, outcome: DrainOutcome) -> ServiceDrain {
    ServiceDrain {
        service: service(name),
        outcome,
    }
}

fn proxies(servers: &[&Machine]) -> Unconfirmed {
    Unconfirmed::Proxies {
        servers: servers
            .iter()
            .map(|server| MachineRef::from(*server))
            .collect(),
    }
}

fn report(services: Vec<ServiceDrain>, remaining: Remaining) -> DrainReport {
    DrainReport {
        server: server('b', "web-2"),
        services_role: ServicesRole::AlreadyOff,
        services,
        stopped: None,
        remaining,
    }
}

fn observed(names: &[&str]) -> Remaining {
    Remaining::Observed {
        services: names.iter().map(|name| service(name)).collect(),
        unchosen: Vec::new(),
    }
}

#[test]
fn each_way_a_move_ends_prints_its_plan_line() {
    let drained = server('b', "web-2");
    let web1 = server('a', "web-1");
    let web3 = server('c', "web-3");
    let line = |outcome| line(&entry("web", outcome), &drained.name);
    assert_eq!(
        line(DrainOutcome::Moved {
            moves: vec![step(&web1, OldCopyEnd::Stopped)]
        }),
        "app/web  moved to web-1, old copy stopped  done"
    );
    assert_eq!(
        line(DrainOutcome::Stays {
            reason: StayReason::LastIn
        }),
        "app/web  stays on web-2 (last copy in rotation; deploy app/web elsewhere first)"
    );
    assert_eq!(
        line(DrainOutcome::Moved {
            moves: vec![step(
                &web1,
                OldCopyEnd::Running {
                    unconfirmed: proxies(&[&web3])
                }
            )]
        }),
        "app/web  moved to web-1, old copy left running on web-2 (proxy on web-3 did not confirm). The next drain cleans it up."
    );
    assert_eq!(
        line(DrainOutcome::Held),
        "app/web  stays on web-2 (stopped by you; start or remove it first)"
    );
    assert_eq!(
        line(DrainOutcome::Moved {
            moves: vec![step(
                &web1,
                OldCopyEnd::In {
                    unconfirmed: proxies(&[&web1, &web3])
                }
            )]
        }),
        "app/web  moved to web-1, old copy still in rotation on web-2 (proxies on web-1, web-3 did not confirm). Both copies are serving. The next drain cleans it up.",
        "a copy still In names the draining Machine and says both copies serve"
    );
    assert_eq!(
        line(DrainOutcome::Moved {
            moves: vec![
                step(&web1, OldCopyEnd::Stopped),
                step(
                    &web3,
                    OldCopyEnd::Running {
                        unconfirmed: Unconfirmed::Cancelled
                    }
                ),
            ]
        }),
        "app/web  moved to web-1, old copy stopped  done\n\
         app/web  moved to web-3, old copy left running on web-2 (the drain was cancelled before it confirmed). The next drain cleans it up.",
        "one line per move"
    );
    assert_eq!(
        line(DrainOutcome::OldCopies {
            ends: vec![
                OldCopyEnd::Stopped,
                OldCopyEnd::Running {
                    unconfirmed: Unconfirmed::Failed {
                        detail: "machine web-2 is unreachable".into()
                    }
                }
            ]
        }),
        "app/web  old copy stopped  done\n\
         app/web  old copy left running on web-2 (machine web-2 is unreachable). The next drain cleans it up.",
        "an Out copy found on the Machine ends like a move's old copy, without a move"
    );
}

#[test]
fn every_other_outcome_keeps_its_line() {
    let drained = server('b', "web-2");
    let web1 = server('a', "web-1");
    let line = |name, outcome| line(&entry(name, outcome), &drained.name);
    let failure = MoveFailure::NotServing {
        from: MachineRef::from(&drained),
        to: MachineRef::from(&web1),
        detail: "deploy cancelled".into(),
        replacement_removed: true,
    };
    assert_eq!(
        [
            line("metrics", DrainOutcome::Retired),
            line(
                "probe",
                DrainOutcome::NotRetired {
                    error: "still running on web-2".into()
                }
            ),
            line("idle", DrainOutcome::NothingToMove),
            line("late", DrainOutcome::NotAttempted),
            line(
                "api",
                DrainOutcome::Failed {
                    moves: vec![step(&web1, OldCopyEnd::Stopped)],
                    failure: failure.clone(),
                }
            ),
            line(
                "solo",
                DrainOutcome::Failed {
                    moves: Vec::new(),
                    failure: MoveFailure::Cancelled {
                        from: MachineRef::from(&drained),
                        to: MachineRef::from(&web1),
                        replacement_removed: false,
                    },
                }
            ),
            line("web", DrainOutcome::Interrupted { moves: Vec::new() }),
            line(
                "web",
                DrainOutcome::Interrupted {
                    moves: vec![step(&web1, OldCopyEnd::Stopped)]
                }
            ),
            line(
                "db",
                DrainOutcome::Stays {
                    reason: StayReason::Volume {
                        server: MachineRef::from(&drained)
                    }
                }
            ),
        ],
        [
            "app/metrics: global, retired on web-2",
            "app/probe: global, failed to retire on web-2: still running on web-2",
            "app/idle: nothing to move",
            "app/late: not attempted",
            "app/api: moved 1 from web-2 to web-1; failed: moving it from web-2 to web-1: deploy cancelled",
            "app/solo: failed: moving it from web-2 to web-1: cancelled",
            "app/web: interrupted",
            "app/web: moved 1 from web-2 to web-1; interrupted",
            "app/db  stays on web-2 (its Volume is on web-2)",
        ]
    );
}

#[test]
fn the_drain_is_complete_once_every_copy_moved_and_every_old_copy_is_gone() {
    let web1 = server('a', "web-1");
    let moved = |old| {
        entry(
            "web",
            DrainOutcome::Moved {
                moves: vec![step(&web1, old)],
            },
        )
    };
    let done = report(vec![moved(OldCopyEnd::Stopped)], observed(&[]));
    assert!(done.complete());
    assert_eq!(
        remaining(&done),
        Ok("Nothing runs on web-2 now.".to_owned())
    );

    let still_there = report(vec![moved(OldCopyEnd::Stopped)], observed(&["db", "web"]));
    assert_eq!(
        remaining(&still_there),
        Ok("Still on web-2: app/db, app/web".to_owned())
    );
    assert!(
        !still_there.complete(),
        "a Service still observed on the Machine keeps the drain open"
    );

    let left_running = report(
        vec![moved(OldCopyEnd::Running {
            unconfirmed: proxies(&[&server('c', "web-3")]),
        })],
        observed(&[]),
    );
    assert!(!left_running.complete());
    assert_eq!(
        incomplete(&left_running).report().code,
        RpcErrorCode::Unavailable,
        "an old copy left behind is the next drain's: Unavailable"
    );
    assert_eq!(
        incomplete(&left_running).to_string(),
        "an old copy of app/web is left on web-2. The next drain cleans it up."
    );

    let both_serving = report(
        vec![moved(OldCopyEnd::In {
            unconfirmed: Unconfirmed::Cancelled,
        })],
        observed(&[]),
    );
    assert!(!both_serving.complete());
    assert_eq!(
        incomplete(&both_serving).report().code,
        RpcErrorCode::Unavailable
    );

    let last_in = report(
        vec![entry(
            "web",
            DrainOutcome::Stays {
                reason: StayReason::LastIn,
            },
        )],
        observed(&["web"]),
    );
    assert!(!last_in.complete());
    let error = incomplete(&last_in);
    assert_eq!(
        (error.report().code, error.to_string()),
        (RpcErrorCode::Conflict, "app/web stays on web-2.".to_owned()),
        "the last copy in rotation is the user's to resolve: Conflict"
    );

    let held = report(vec![entry("web", DrainOutcome::Held)], observed(&["web"]));
    assert!(!held.complete());
    assert_eq!(incomplete(&held).report().code, RpcErrorCode::Conflict);

    let let_go = report(
        vec![entry(
            "web",
            DrainOutcome::OldCopies {
                ends: vec![OldCopyEnd::Stopped],
            },
        )],
        observed(&[]),
    );
    assert!(let_go.complete(), "an Out copy sent Gone is done");

    let failed = report(
        vec![entry(
            "web",
            DrainOutcome::Failed {
                moves: Vec::new(),
                failure: MoveFailure::Cancelled {
                    from: MachineRef::from(&server('b', "web-2")),
                    to: MachineRef::from(&web1),
                    replacement_removed: true,
                },
            },
        )],
        observed(&["web"]),
    );
    assert!(!failed.complete());
    assert_eq!(
        incomplete(&failed).report().code,
        RpcErrorCode::Internal,
        "a failure already printed its line; the exit is partial"
    );
}

#[test]
fn a_stopped_drain_leaves_what_remains_unchecked() {
    let drained = server('b', "web-2");
    let report = DrainReport {
        server: drained,
        services_role: ServicesRole::TurnedOff,
        services: vec![ServiceDrain {
            service: service("web"),
            outcome: DrainOutcome::NotAttempted,
        }],
        stopped: Some(DrainStop::Cancelled),
        remaining: Remaining::Unobserved {
            error: "no terminal response".into(),
        },
    };
    assert_eq!(
        remaining(&report),
        Err("Cannot observe Services on Server web-2: no terminal response".to_owned())
    );
    assert!(!report.complete());
}

#[test]
fn an_old_copy_refused_out_as_the_last_in_is_a_conflict() {
    let value = report(
        vec![entry(
            "web",
            DrainOutcome::Failed {
                moves: Vec::new(),
                failure: MoveFailure::Refused {
                    reason: StayReason::LastIn,
                },
            },
        )],
        observed(&["web"]),
    );
    assert_eq!(incomplete(&value).report().code, RpcErrorCode::Conflict);
}
