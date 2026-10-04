//! The Drain's wire contract and scope, and how execution sequences what it observes.

use std::collections::VecDeque;

use ployz_core::{
    Machine, MachineId, MachineName, Namespace, QualifiedService, WireGuardPublicKey,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    DrainClient, DrainOutcome, DrainReport, DrainScope, DrainStop, Lost, MachineRef, Move,
    MoveFailure, Ready, Remaining, ServiceDrain, ServicesRole, StayReason, execute,
};
use crate::deploy::Converged;
use crate::drain::retirement::Retirement;

fn namespace(value: &str) -> Namespace {
    Namespace::parse(value).unwrap()
}

#[test]
fn the_scope_reads_as_cloud_sends_it() {
    let every: DrainScope = serde_json::from_value(json!({ "scope": "every_namespace" })).unwrap();
    assert_eq!(every, DrainScope::EveryNamespace);
    let owned: DrainScope =
        serde_json::from_value(json!({ "scope": "owned", "namespaces": ["shop"] })).unwrap();
    assert_eq!(
        owned,
        DrainScope::Owned {
            namespaces: vec![namespace("shop")]
        }
    );
    assert!(
        serde_json::from_value::<DrainScope>(json!({ "scope": "owned" })).is_err(),
        "an owned scope names its Namespaces"
    );
}

#[test]
fn an_empty_owned_scope_chooses_nothing_and_reserved_namespaces_never() {
    let nothing = DrainScope::Owned {
        namespaces: Vec::new(),
    };
    assert!(!nothing.includes(&namespace("shop")));
    let shop = DrainScope::Owned {
        namespaces: vec![namespace("shop")],
    };
    assert!(shop.includes(&namespace("shop")) && !shop.includes(&namespace("blog")));
    assert!(DrainScope::EveryNamespace.includes(&namespace("blog")));
    let system = QualifiedService::system_ingress().namespace;
    assert!(!DrainScope::EveryNamespace.includes(&system));
}

#[test]
fn an_outcome_is_flat_on_result() {
    let server = |hex: char, name: &str| MachineRef {
        id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
        name: MachineName::parse(name).unwrap(),
    };
    let entry = ServiceDrain {
        service: QualifiedService::parse("shop/web").unwrap(),
        outcome: DrainOutcome::Failed {
            moves: vec![Move {
                from: server('b', "web-2"),
                to: server('a', "web-1"),
            }],
            failure: MoveFailure::Refused {
                reason: StayReason::MidRollout,
            },
        },
    };
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        json!({
            "service": "shop/web",
            "result": "failed",
            "moves": [{
                "from": { "id": "b".repeat(32), "name": "web-2" },
                "to": { "id": "a".repeat(32), "name": "web-1" },
            }],
            "failure": { "stage": "refused", "reason": { "kind": "mid_rollout" } },
        })
    );
    assert!(!entry.outcome.complete());
    assert!(DrainOutcome::NothingToMove.complete() && !DrainOutcome::NotAttempted.complete());
    assert_eq!(
        serde_json::to_value(DrainOutcome::Interrupted { moves: Vec::new() }).unwrap(),
        json!({ "result": "interrupted", "moves": [] })
    );
}

/// Answers each call from its script, in order.
#[derive(Default)]
struct Scripted {
    retirements: Vec<(QualifiedService, Retirement)>,
    observations: VecDeque<Result<Vec<QualifiedService>, Lost>>,
    convergences: VecDeque<Converged>,
    /// Cancelled once the first Service converges.
    cancel_after_converge: Option<CancellationToken>,
    retired: bool,
}

impl DrainClient for Scripted {
    async fn still_on(&mut self, _id: &MachineId) -> Result<Vec<QualifiedService>, Lost> {
        self.observations
            .pop_front()
            .expect("one scripted observation per look")
    }

    async fn retire(
        &mut self,
        _server: &Machine,
        _globals: &[QualifiedService],
        _cancellation: &CancellationToken,
    ) -> Vec<(QualifiedService, Retirement)> {
        self.retired = true;
        std::mem::take(&mut self.retirements)
    }

    async fn converge(
        &mut self,
        _service: &QualifiedService,
        _cancellation: &CancellationToken,
    ) -> Converged {
        if let Some(cancellation) = &self.cancel_after_converge {
            cancellation.cancel();
        }
        self.convergences
            .pop_front()
            .expect("one scripted convergence per Service")
    }
}

fn drained() -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: false,
        id: MachineId::parse("b".repeat(32)).unwrap(),
        name: MachineName::parse("web-2").unwrap(),
        subnet: "10.210.11.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([b'b'; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

fn qualified(name: &str) -> QualifiedService {
    QualifiedService::parse(format!("app/{name}")).unwrap()
}

fn moved() -> DrainOutcome {
    DrainOutcome::Moved {
        moves: vec![Move {
            from: MachineRef::from(&drained()),
            to: MachineRef {
                id: MachineId::parse("a".repeat(32)).unwrap(),
                name: MachineName::parse("web-1").unwrap(),
            },
        }],
    }
}

async fn run(
    client: &mut Scripted,
    globals: &[&str],
    replicated: &[&str],
    cancellation: &CancellationToken,
) -> DrainReport {
    run_within(
        client,
        DrainScope::EveryNamespace,
        globals,
        replicated,
        cancellation,
    )
    .await
}

async fn run_within(
    client: &mut Scripted,
    scope: DrainScope,
    globals: &[&str],
    replicated: &[&str],
    cancellation: &CancellationToken,
) -> DrainReport {
    let ready = Ready {
        server: drained(),
        scope,
        services_role: ServicesRole::TurnedOff,
        globals: globals.iter().map(|name| qualified(name)).collect(),
        replicated: replicated.iter().map(|name| qualified(name)).collect(),
    };
    execute(client, ready, cancellation, &mut |_| {}).await
}

fn outcomes(report: &DrainReport) -> Vec<(String, DrainOutcome)> {
    report
        .services
        .iter()
        .map(|entry| (entry.service.to_string(), entry.outcome.clone()))
        .collect()
}

fn entry(name: &str, outcome: DrainOutcome) -> (String, DrainOutcome) {
    (format!("app/{name}"), outcome)
}

fn nothing_left() -> Result<Vec<QualifiedService>, Lost> {
    Ok(Vec::new())
}

#[tokio::test]
async fn cancelled_before_globals_attempts_nothing() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        ..Scripted::default()
    };
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let report = run(&mut client, &["metrics"], &["web"], &cancelled).await;
    assert!(!client.retired);
    assert_eq!(
        outcomes(&report),
        [
            entry("metrics", DrainOutcome::NotAttempted),
            entry("web", DrainOutcome::NotAttempted),
        ]
    );
    assert_eq!(report.stopped, Some(DrainStop::Cancelled));
}

#[tokio::test]
async fn cancelled_between_globals_keeps_what_retired_and_stops() {
    let mut client = Scripted {
        retirements: vec![
            (qualified("metrics"), Retirement::Retired),
            (qualified("probe"), Retirement::NotAttempted),
        ],
        observations: [nothing_left(), nothing_left()].into(),
        ..Scripted::default()
    };
    let report = run(
        &mut client,
        &["metrics", "probe"],
        &["web"],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes(&report),
        [
            entry("metrics", DrainOutcome::Retired),
            entry("probe", DrainOutcome::NotAttempted),
            entry("web", DrainOutcome::NotAttempted),
        ]
    );
    assert_eq!(report.stopped, Some(DrainStop::Cancelled));
}

#[tokio::test]
async fn cancelled_between_services_keeps_the_moves_made() {
    let cancellation = CancellationToken::new();
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Done(moved())].into(),
        cancel_after_converge: Some(cancellation.clone()),
        ..Scripted::default()
    };
    let report = run(&mut client, &[], &["web", "api"], &cancellation).await;
    assert_eq!(
        outcomes(&report),
        [
            entry("web", moved()),
            entry("api", DrainOutcome::NotAttempted),
        ]
    );
    assert_eq!(report.stopped, Some(DrainStop::Cancelled));
    assert_eq!(
        report.remaining,
        Remaining::Observed {
            services: Vec::new(),
            unchosen: Vec::new(),
        }
    );
    assert!(!report.complete());
}

#[tokio::test]
async fn losing_the_entry_after_globals_keeps_their_retirements_and_stops() {
    let mut client = Scripted {
        retirements: vec![(qualified("metrics"), Retirement::Retired)],
        observations: [
            Err(Lost::Entry("timed out".into())),
            Err(Lost::Entry("timed out".into())),
        ]
        .into(),
        ..Scripted::default()
    };
    let report = run(
        &mut client,
        &["metrics"],
        &["web"],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes(&report),
        [
            entry("metrics", DrainOutcome::Retired),
            entry("web", DrainOutcome::NotAttempted),
        ]
    );
    assert_eq!(
        report.stopped,
        Some(DrainStop::EntryUnreachable {
            detail: "timed out".into()
        })
    );
    assert_eq!(
        report.remaining,
        Remaining::Unobserved {
            error: "timed out".into()
        }
    );
}

#[tokio::test]
async fn losing_the_entry_mid_replicated_stops_at_that_service() {
    let lost = Converged::Stopped {
        moves: Vec::new(),
        stop: DrainStop::EntryUnreachable {
            detail: "timed out".into(),
        },
    };
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Done(moved()), lost].into(),
        ..Scripted::default()
    };
    let report = run(
        &mut client,
        &[],
        &["web", "api", "db"],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes(&report),
        [
            entry("web", moved()),
            entry("api", DrainOutcome::Interrupted { moves: Vec::new() }),
            entry("db", DrainOutcome::NotAttempted),
        ]
    );
    assert_eq!(
        report.stopped,
        Some(DrainStop::EntryUnreachable {
            detail: "timed out".into()
        })
    );
}

#[tokio::test]
async fn only_a_global_still_observed_undoes_its_retirement() {
    let retirements = || {
        vec![
            (qualified("metrics"), Retirement::Retired),
            (qualified("probe"), Retirement::Retired),
            (
                qualified("agent"),
                Retirement::NotRetired("conflict".into()),
            ),
        ]
    };
    let mut client = Scripted {
        retirements: retirements(),
        observations: [
            Err(Lost::Server("no terminal response".into())),
            nothing_left(),
        ]
        .into(),
        convergences: [Converged::Done(moved())].into(),
        ..Scripted::default()
    };
    let report = run(
        &mut client,
        &["metrics", "probe", "agent"],
        &["web"],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes(&report),
        [
            entry("metrics", DrainOutcome::Retired),
            entry("probe", DrainOutcome::Retired),
            entry(
                "agent",
                DrainOutcome::NotRetired {
                    error: "conflict".into()
                }
            ),
            entry("web", moved()),
        ],
        "a lost look at the Server keeps every retire result, and the Drain goes on"
    );
    assert_eq!(report.stopped, None);

    let mut client = Scripted {
        retirements: retirements(),
        observations: [Ok(vec![qualified("probe")]), Ok(vec![qualified("probe")])].into(),
        convergences: [Converged::Done(moved())].into(),
        ..Scripted::default()
    };
    let report = run(
        &mut client,
        &["metrics", "probe", "agent"],
        &["web"],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes(&report).get(..2).unwrap(),
        [
            entry("metrics", DrainOutcome::Retired),
            entry(
                "probe",
                DrainOutcome::NotRetired {
                    error: "still running on web-2".into()
                }
            ),
        ]
    );
}

#[tokio::test]
async fn a_full_drain_is_complete_and_names_the_user_services_its_scope_left_alone() {
    let blog = QualifiedService::parse("blog/web").unwrap();
    let mut client = Scripted {
        observations: [Ok(vec![blog.clone(), QualifiedService::system_ingress()])].into(),
        convergences: [Converged::Done(moved())].into(),
        ..Scripted::default()
    };
    let scope = DrainScope::Owned {
        namespaces: vec![namespace("app")],
    };
    let report = run_within(&mut client, scope, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(
        report.remaining,
        Remaining::Observed {
            services: vec![blog.clone(), QualifiedService::system_ingress()],
            unchosen: vec![blog],
        }
    );
    assert!(report.complete());
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json.get("complete"), Some(&json!(true)));
    assert_eq!(
        json.pointer("/remaining/unchosen"),
        Some(&json!(["blog/web"]))
    );
}
