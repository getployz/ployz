//! The CLI projection of a Drain report. `legacy` is the handler's output types before
//! the report was typed, kept verbatim so every outcome that existed then prints the same
//! bytes now.

use ployz_core::{Machine, MachineId, MachineName, QualifiedService, WireGuardPublicKey};
use serde_json::json;

use super::{NOTHING_MOVES_BACK, closing_lines, line, report_json, server_json};
use crate::drain::{
    DrainOutcome, DrainReport, DrainStop, MachineRef, Move, MoveFailure, Remaining, ServiceDrain,
    ServicesRole, StayReason,
};

mod legacy {
    use ployz_core::{MachineName, QualifiedService};
    use serde::Serialize;

    #[derive(Serialize)]
    pub(super) struct ServiceReport {
        pub(super) service: QualifiedService,
        #[serde(flatten)]
        pub(super) outcome: Outcome,
    }

    #[derive(Serialize)]
    #[serde(untagged)]
    pub(super) enum Outcome {
        Replicated(Convergence),
        Global(Retirement),
    }

    #[derive(Serialize)]
    #[serde(tag = "result", rename_all = "snake_case")]
    pub(super) enum Retirement {
        Retired,
        Failed { error: String },
    }

    #[derive(Serialize)]
    pub(super) struct Move {
        pub(super) from: MachineName,
        pub(super) to: MachineName,
    }

    #[derive(Serialize)]
    #[serde(tag = "result", rename_all = "snake_case")]
    pub(super) enum Convergence {
        Stays {
            reason: String,
        },
        Moved {
            moved: Vec<Move>,
            failed: Option<String>,
        },
    }
}

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

fn step(to: &Machine) -> Move {
    Move {
        from: MachineRef::from(&server('b', "web-2")),
        to: MachineRef::from(to),
    }
}

fn legacy_step(to: &str) -> legacy::Move {
    legacy::Move {
        from: MachineName::parse("web-2").unwrap(),
        to: MachineName::parse(to).unwrap(),
    }
}

/// Each outcome that existed before, as the report types it and as the old handler did.
fn every_legacy_outcome() -> (Vec<ServiceDrain>, Vec<legacy::ServiceReport>) {
    let web1 = server('a', "web-1");
    let web3 = server('c', "web-3");
    let failure = MoveFailure::NotServing {
        from: MachineRef::from(&server('b', "web-2")),
        to: MachineRef::from(&web1),
        detail: "deploy cancelled".into(),
    };
    let reason = StayReason::Volume {
        volume: serde_json::from_value(json!("data")).unwrap(),
        server: MachineRef::from(&server('b', "web-2")),
    };
    let typed = vec![
        (service("metrics"), DrainOutcome::Retired),
        (
            service("probe"),
            DrainOutcome::NotRetired {
                error: "still running on web-2".into(),
            },
        ),
        (
            service("web"),
            DrainOutcome::Moved {
                moves: vec![step(&web1), step(&web3), step(&web1)],
            },
        ),
        (service("idle"), DrainOutcome::NothingToMove),
        (
            service("api"),
            DrainOutcome::Failed {
                moves: vec![step(&web1)],
                failure: failure.clone(),
            },
        ),
        (
            service("solo"),
            DrainOutcome::Failed {
                moves: Vec::new(),
                failure,
            },
        ),
        (service("db"), DrainOutcome::Stays { reason }),
    ];
    let failed = Some("moving it from web-2 to web-1: deploy cancelled".to_owned());
    let old = vec![
        legacy::Outcome::Global(legacy::Retirement::Retired),
        legacy::Outcome::Global(legacy::Retirement::Failed {
            error: "still running on web-2".into(),
        }),
        legacy::Outcome::Replicated(legacy::Convergence::Moved {
            moved: vec![
                legacy_step("web-1"),
                legacy_step("web-3"),
                legacy_step("web-1"),
            ],
            failed: None,
        }),
        legacy::Outcome::Replicated(legacy::Convergence::Moved {
            moved: Vec::new(),
            failed: None,
        }),
        legacy::Outcome::Replicated(legacy::Convergence::Moved {
            moved: vec![legacy_step("web-1")],
            failed: failed.clone(),
        }),
        legacy::Outcome::Replicated(legacy::Convergence::Moved {
            moved: Vec::new(),
            failed,
        }),
        legacy::Outcome::Replicated(legacy::Convergence::Stays {
            reason: "Volume data is on web-2".into(),
        }),
    ];
    let reports = typed
        .iter()
        .zip(old)
        .map(|((service, _), outcome)| legacy::ServiceReport {
            service: service.clone(),
            outcome,
        })
        .collect();
    let typed = typed
        .into_iter()
        .map(|(service, outcome)| ServiceDrain { service, outcome })
        .collect();
    (typed, reports)
}

#[test]
fn every_outcome_that_existed_prints_the_same_json() {
    let drained = server('b', "web-2");
    let (services, reports) = every_legacy_outcome();
    let remaining = vec![service("db"), QualifiedService::system_ingress()];
    let report = DrainReport {
        server: drained.clone(),
        services_role: ServicesRole::TurnedOff,
        services,
        stopped: None,
        remaining: Remaining::Observed {
            services: remaining.clone(),
        },
    };
    let before = json!({
        "server": server_json(&drained),
        "services": reports,
        "remaining": remaining,
        "note": NOTHING_MOVES_BACK,
    });
    assert_eq!(
        serde_json::to_string(&report_json(&report)).unwrap(),
        serde_json::to_string(&before).unwrap()
    );
    assert!(!report.complete(), "anything left on the Server is partial");
}

#[test]
fn every_outcome_that_existed_prints_the_same_line() {
    let drained = server('b', "web-2");
    let (services, _) = every_legacy_outcome();
    let lines = services
        .iter()
        .map(|entry| line(entry, &drained.name))
        .collect::<Vec<_>>();
    assert_eq!(
        lines,
        [
            "app/metrics: global, retired on web-2",
            "app/probe: global, failed to retire on web-2: still running on web-2",
            "app/web: moved 2 from web-2 to web-1, 1 from web-2 to web-3",
            "app/idle: nothing to move",
            "app/api: moved 1 from web-2 to web-1; failed: moving it from web-2 to web-1: deploy cancelled",
            "app/solo: failed: moving it from web-2 to web-1: deploy cancelled",
            "app/db: stays: Volume data is on web-2",
        ]
    );
    let report = |remaining| DrainReport {
        server: drained.clone(),
        services_role: ServicesRole::AlreadyOff,
        services: Vec::new(),
        stopped: None,
        remaining,
    };
    assert_eq!(
        closing_lines(&report(Remaining::Observed {
            services: vec![service("db")]
        })),
        ["Still on web-2: app/db", NOTHING_MOVES_BACK]
    );
    let empty = report(Remaining::Observed {
        services: Vec::new(),
    });
    assert_eq!(
        closing_lines(&empty),
        ["Nothing runs on web-2 now.", NOTHING_MOVES_BACK]
    );
    assert!(empty.complete());
}

#[test]
fn a_stopped_drain_adds_what_used_to_be_lost() {
    let drained = server('b', "web-2");
    let report = DrainReport {
        server: drained.clone(),
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
        report_json(&report),
        json!({
            "server": server_json(&drained),
            "services": [{ "service": "app/web", "result": "not_attempted" }],
            "stopped": "cancelled",
            "remaining": null,
            "remaining_error": "no terminal response",
            "note": NOTHING_MOVES_BACK,
        })
    );
    assert_eq!(
        closing_lines(&report),
        [
            "Drain stopped: cancelled",
            "Cannot observe Services on Server web-2: no terminal response",
            NOTHING_MOVES_BACK,
        ]
    );
    assert!(!report.complete());
}
