//! The CLI projection of a Drain report. The fixtures are the bytes the handler printed
//! before the report was typed, so every outcome that existed then prints the same now.

use ployz_core::{Machine, MachineId, MachineName, QualifiedService, WireGuardPublicKey};
use serde_json::json;

use super::{NOTHING_MOVES_BACK, closing_lines, line, report_json, server_json};
use crate::drain::{
    DrainOutcome, DrainReport, DrainStop, MachineRef, Move, MoveFailure, Remaining, ServiceDrain,
    ServicesRole, StayReason,
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

fn step(to: &Machine) -> Move {
    Move {
        from: MachineRef::from(&server('b', "web-2")),
        to: MachineRef::from(to),
    }
}

/// Each outcome that existed before the report was typed.
fn every_legacy_outcome() -> Vec<ServiceDrain> {
    let web1 = server('a', "web-1");
    let web3 = server('c', "web-3");
    let failure = MoveFailure::NotServing {
        from: MachineRef::from(&server('b', "web-2")),
        to: MachineRef::from(&web1),
        detail: "deploy cancelled".into(),
        replacement_removed: true,
    };
    let reason = StayReason::Volume {
        volume: serde_json::from_value(json!("data")).unwrap(),
        server: MachineRef::from(&server('b', "web-2")),
    };
    [
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
    ]
    .into_iter()
    .map(|(service, outcome)| ServiceDrain { service, outcome })
    .collect()
}

/// `--json` as the handler printed it before the report was typed.
const LEGACY_JSON: &str = r#"{
  "note": "Turning the services role back on does not move anything back.",
  "remaining": [
    "app/db",
    "ployz-system/ingress"
  ],
  "server": {
    "machine": {
      "accepts_builds": true,
      "accepts_ingress": true,
      "accepts_services": true,
      "advertised_endpoints": [],
      "build_concurrency": null,
      "id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
      "labels": {},
      "name": "web-2",
      "public_ip": null,
      "public_key": "YmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmI=",
      "runtime": {
        "architecture": "",
        "daemon_version": "",
        "docker_version": "",
        "hostname": "",
        "kernel_version": "",
        "memory_total_bytes": null,
        "os_pretty_name": "",
        "running_builds": 0
      },
      "subnet": "10.210.11.0/24"
    }
  },
  "services": [
    {
      "result": "retired",
      "service": "app/metrics"
    },
    {
      "error": "still running on web-2",
      "result": "failed",
      "service": "app/probe"
    },
    {
      "failed": null,
      "moved": [
        {
          "from": "web-2",
          "to": "web-1"
        },
        {
          "from": "web-2",
          "to": "web-3"
        },
        {
          "from": "web-2",
          "to": "web-1"
        }
      ],
      "result": "moved",
      "service": "app/web"
    },
    {
      "failed": null,
      "moved": [],
      "result": "moved",
      "service": "app/idle"
    },
    {
      "failed": "moving it from web-2 to web-1: deploy cancelled",
      "moved": [
        {
          "from": "web-2",
          "to": "web-1"
        }
      ],
      "result": "moved",
      "service": "app/api"
    },
    {
      "failed": "moving it from web-2 to web-1: deploy cancelled",
      "moved": [],
      "result": "moved",
      "service": "app/solo"
    },
    {
      "reason": "Volume data is on web-2",
      "result": "stays",
      "service": "app/db"
    }
  ]
}"#;

#[test]
fn every_outcome_that_existed_prints_the_same_json() {
    let report = DrainReport {
        server: server('b', "web-2"),
        services_role: ServicesRole::TurnedOff,
        services: every_legacy_outcome(),
        stopped: None,
        remaining: Remaining::Observed {
            services: vec![service("db"), QualifiedService::system_ingress()],
        },
    };
    assert_eq!(
        serde_json::to_string_pretty(&report_json(&report)).unwrap(),
        LEGACY_JSON,
        "the CLI writes `--json` pretty-printed"
    );
    assert!(!report.complete(), "anything left on the Server is partial");
}

#[test]
fn every_outcome_that_existed_prints_the_same_line() {
    let drained = server('b', "web-2");
    let lines = every_legacy_outcome()
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
    let failed = |failure| {
        line(
            &ServiceDrain {
                service: service("web"),
                outcome: DrainOutcome::Failed {
                    moves: Vec::new(),
                    failure,
                },
            },
            &drained.name,
        )
    };
    let from = MachineRef::from(&drained);
    let to = MachineRef::from(&server('a', "web-1"));
    assert_eq!(
        [
            failed(MoveFailure::Cancelled {
                from: from.clone(),
                to: to.clone(),
                replacement_removed: false,
            }),
            failed(MoveFailure::CancelledBeforeMove { from: from.clone() }),
            failed(MoveFailure::OldNotRemoved {
                from,
                to,
                detail: "remove failed".into(),
                old_stopped: true,
            }),
        ],
        [
            "app/web: failed: moving it from web-2 to web-1: deploy cancelled",
            "app/web: failed: cancelled before moving it off web-2",
            "app/web: failed: moving it from web-2 to web-1: remove failed",
        ],
        "what a move now records about its Containers stays out of the line"
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
