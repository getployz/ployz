//! The Drain's wire contract and scope, and how execution sequences what it observes.

use std::collections::VecDeque;

use ployz_core::{
    ContainerId, DeploymentLogId, Machine, MachineId, MachineName, Namespace, QualifiedService,
    RpcError, RpcErrorCode, ServingPointer, WireGuardPublicKey,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    DrainClient, DrainOutcome, DrainReport, DrainScope, DrainStop, Listed, Lost, MachineRef, Move,
    MoveFailure, Ready, Remaining, ServiceDrain, ServicesRole, StayReason, execute,
    publish_replacements_past_cancel,
};
use crate::deploy::{Converged, OldCopy, Replacement};
use crate::drain::retirement::Retirement;
use crate::serving_publish::Amendment;
use crate::serving_publish::tests::{FakeRow, container, pointer};

fn namespace(value: &str) -> Namespace {
    Namespace::parse(value).unwrap()
}

fn service(value: &str) -> QualifiedService {
    QualifiedService::parse(value).unwrap()
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
    let services: DrainScope =
        serde_json::from_value(json!({ "scope": "services", "services": ["shop/web"] })).unwrap();
    assert_eq!(
        services,
        DrainScope::Services {
            services: vec![service("shop/web")]
        }
    );
}

#[test]
fn an_empty_owned_scope_chooses_nothing_and_reserved_namespaces_never() {
    let nothing = DrainScope::Owned {
        namespaces: Vec::new(),
    };
    assert!(!nothing.includes(&service("shop/web")));
    let shop = DrainScope::Owned {
        namespaces: vec![namespace("shop")],
    };
    assert!(shop.includes(&service("shop/web")) && !shop.includes(&service("blog/web")));
    assert!(DrainScope::EveryNamespace.includes(&service("blog/web")));
    let system = QualifiedService::system_ingress();
    assert!(!DrainScope::EveryNamespace.includes(&system));
    let web = DrainScope::Services {
        services: vec![service("shop/web"), system.clone()],
    };
    assert!(web.includes(&service("shop/web")) && !web.includes(&service("shop/api")));
    assert!(
        !web.includes(&system),
        "a listed reserved Service is still never drained"
    );
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
    observations: VecDeque<Result<Vec<Listed>, Lost>>,
    convergences: VecDeque<Converged>,
    /// Cancelled once the first Service converges.
    cancel_after_converge: Option<CancellationToken>,
    retired: bool,
    pointer: Option<ServingPointer>,
    pointer_error: Option<RpcError>,
    kept: Vec<Option<Vec<ContainerId>>>,
    repointed: Vec<(QualifiedService, DeploymentLogId, Vec<Replacement>)>,
    repoint_error: Option<RpcError>,
}

impl DrainClient for Scripted {
    async fn still_on(&mut self, _id: &MachineId) -> Result<Vec<Listed>, Lost> {
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

    async fn pointer(
        &mut self,
        _service: &QualifiedService,
    ) -> Result<Option<ServingPointer>, RpcError> {
        self.pointer_error
            .clone()
            .map_or_else(|| Ok(self.pointer.clone()), Err)
    }

    async fn converge(
        &mut self,
        _service: &QualifiedService,
        old: OldCopy<'_>,
        _cancellation: &CancellationToken,
    ) -> Converged {
        self.kept.push(match old {
            OldCopy::Remove => None,
            OldCopy::Keep { pointer } => Some(pointer.retiring.clone()),
        });
        if let Some(cancellation) = &self.cancel_after_converge {
            cancellation.cancel();
        }
        self.convergences
            .pop_front()
            .expect("one scripted convergence per Service")
    }

    async fn repoint(
        &mut self,
        service: &QualifiedService,
        deploy: &DeploymentLogId,
        replaced: &[Replacement],
    ) -> Result<Amendment, RpcError> {
        self.repointed
            .push((service.clone(), deploy.clone(), replaced.to_vec()));
        self.repoint_error
            .clone()
            .map_or(Ok(Amendment::Published { retries: 0 }), Err)
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

fn one_move() -> Vec<Move> {
    vec![Move {
        from: MachineRef::from(&drained()),
        to: MachineRef {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("web-1").unwrap(),
        },
    }]
}

fn one_replaced() -> Vec<Replacement> {
    vec![Replacement {
        old: container('1'),
        new: container('9'),
        deploy: DeploymentLogId::parse("dep-a").unwrap(),
    }]
}

fn moved() -> DrainOutcome {
    DrainOutcome::Moved { moves: one_move() }
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

fn nothing_left() -> Result<Vec<Listed>, Lost> {
    Ok(Vec::new())
}

/// `service` still on the Server with `copies`, the old copy `'1'` unless given.
fn listed(service: QualifiedService, copies: &[char]) -> Listed {
    let copies = if copies.is_empty() { &['1'] } else { copies };
    Listed {
        service,
        copies: copies.iter().map(|id| container(*id)).collect(),
    }
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
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
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
            retiring: Vec::new(),
        }
    );
    assert!(!report.complete());
    assert_eq!(
        serde_json::to_value(&report).unwrap().get("complete"),
        Some(&json!(false))
    );
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
        replaced: Vec::new(),
        stop: DrainStop::EntryUnreachable {
            detail: "timed out".into(),
        },
    };
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [
            Converged::Moved {
                moves: one_move(),
                replaced: one_replaced(),
            },
            lost,
        ]
        .into(),
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
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
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
        observations: [
            Ok(vec![listed(qualified("probe"), &[])]),
            Ok(vec![listed(qualified("probe"), &[])]),
        ]
        .into(),
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
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
        observations: [Ok(vec![
            listed(blog.clone(), &[]),
            listed(QualifiedService::system_ingress(), &[]),
        ])]
        .into(),
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
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
            retiring: Vec::new(),
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

#[tokio::test]
async fn a_moved_service_is_repointed_and_one_with_nothing_to_move_is_not() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [
            Converged::Moved {
                moves: one_move(),
                replaced: one_replaced(),
            },
            Converged::NothingToMove,
        ]
        .into(),
        pointer: Some(pointer('a', &['1', '2'], &['3'])),
        ..Scripted::default()
    };
    let report = run(&mut client, &[], &["web", "api"], &CancellationToken::new()).await;
    assert_eq!(
        client.repointed,
        [(
            qualified("web"),
            DeploymentLogId::parse("dep-a").unwrap(),
            one_replaced()
        )]
    );
    assert_eq!(
        outcomes(&report),
        [
            entry("web", moved()),
            entry("api", DrainOutcome::NothingToMove)
        ]
    );
}

#[tokio::test]
async fn a_failed_repoint_reads_as_the_replacement_not_serving() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
        pointer: Some(pointer('a', &['1'], &[])),
        repoint_error: Some(RpcError {
            code: RpcErrorCode::Conflict,
            message: "lost".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
        ..Scripted::default()
    };
    let report = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    let outcomes = outcomes(&report);
    let [(_, DrainOutcome::Failed { moves, failure })] = outcomes.as_slice() else {
        panic!("{report:?}");
    };
    assert_eq!(*moves, one_move());
    assert!(
        matches!(failure, MoveFailure::NotServing { detail, replacement_removed: false, .. } if detail.contains("Serving Pointer")),
        "{failure:?}"
    );
    assert!(!report.complete());
}

#[tokio::test]
async fn a_failed_repoint_after_a_stop_reports_the_failure_and_the_stop() {
    let stop = DrainStop::EntryUnreachable {
        detail: "timed out".into(),
    };
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Stopped {
            moves: one_move(),
            replaced: one_replaced(),
            stop: stop.clone(),
        }]
        .into(),
        pointer: Some(pointer('a', &['1'], &[])),
        repoint_error: Some(RpcError {
            code: RpcErrorCode::Conflict,
            message: "lost".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
        ..Scripted::default()
    };
    let report = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    let outcomes = outcomes(&report);
    let [(_, DrainOutcome::Failed { moves, failure })] = outcomes.as_slice() else {
        panic!("{report:?}");
    };
    assert_eq!(*moves, one_move());
    assert!(
        matches!(failure, MoveFailure::NotServing { detail, .. } if detail.contains("Serving Pointer")),
        "{failure:?}"
    );
    assert_eq!(report.stopped, Some(stop));
}

#[tokio::test]
async fn a_pointed_service_keeps_its_old_copies_and_an_unpointed_one_removes_them() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
        pointer: Some(pointer('a', &['1'], &['3'])),
        ..Scripted::default()
    };
    run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(client.kept, [Some(vec![container('3')])]);

    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: Vec::new(),
        }]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(client.kept, [None]);
    assert!(
        client.repointed.is_empty(),
        "nothing to publish without a pointer"
    );
    assert_eq!(outcomes(&report), [entry("web", moved())]);
}

#[tokio::test]
async fn a_drain_publishes_its_replacements_by_id_and_retires_what_they_replaced() {
    let fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    let deploy = DeploymentLogId::parse("dep-a").unwrap();
    let amended = publish_replacements_past_cancel(
        &mut fake.clone(),
        &qualified("web"),
        &deploy,
        &one_replaced(),
    )
    .await
    .unwrap();
    assert_eq!(amended, Amendment::Published { retries: 0 });
    assert_eq!(
        *fake.row.lock().unwrap(),
        Some(pointer('a', &['2', '9'], &['1'])),
        "the replacement serves though it carries no deploy label"
    );
}

#[tokio::test]
async fn a_drain_publishes_nothing_once_a_newer_deploy_owns_the_pointer() {
    let fake = FakeRow::default();
    *fake.row.lock().unwrap() = Some(pointer('a', &['1', '2'], &[]));
    fake.interlopers
        .lock()
        .unwrap()
        .push(pointer('b', &['7'], &['1', '2']));
    let deploy = DeploymentLogId::parse("dep-a").unwrap();
    let amended = publish_replacements_past_cancel(
        &mut fake.clone(),
        &qualified("web"),
        &deploy,
        &one_replaced(),
    )
    .await
    .unwrap();
    assert_eq!(amended, Amendment::Outdated);
    assert_eq!(
        *fake.row.lock().unwrap(),
        Some(pointer('b', &['7'], &['1', '2']))
    );
}

#[tokio::test]
async fn a_service_whose_pointer_cannot_be_read_stays_and_nothing_moves() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        pointer_error: Some(RpcError {
            code: ployz_core::RpcErrorCode::Unavailable,
            message: "timed out".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
        ..Scripted::default()
    };
    let report = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry(
            "web",
            DrainOutcome::Stays {
                reason: StayReason::PointerUnread {
                    detail: "timed out".into()
                }
            }
        )]
    );
    assert!(client.kept.is_empty() && client.repointed.is_empty());
}

#[tokio::test]
async fn an_old_copy_a_first_pointer_named_before_removal_is_repointed_under_it() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: one_move(),
            replaced: one_replaced(),
        }]
        .into(),
        ..Scripted::default()
    };
    run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(client.kept, [None]);
    assert_eq!(
        client.repointed,
        [(
            qualified("web"),
            DeploymentLogId::parse("dep-a").unwrap(),
            one_replaced()
        )]
    );
}

#[tokio::test]
async fn a_pointed_move_is_complete_while_its_old_copy_retires() {
    let still_listed = || Ok(vec![listed(qualified("web"), &['1'])]);
    let mut client = Scripted {
        observations: [still_listed(), still_listed(), nothing_left()].into(),
        convergences: [
            Converged::Moved {
                moves: one_move(),
                replaced: one_replaced(),
            },
            Converged::NothingToMove,
            Converged::NothingToMove,
        ]
        .into(),
        pointer: Some(pointer('a', &['9'], &['1'])),
        ..Scripted::default()
    };
    let first = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(outcomes(&first), [entry("web", moved())]);
    assert!(
        first.complete(),
        "the old copy retires through its pointer: {first:?}"
    );
    assert!(matches!(
        &first.remaining,
        Remaining::Observed { retiring, .. } if *retiring == [qualified("web")]
    ));
    let retry = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&retry),
        [entry("web", DrainOutcome::NothingToMove)]
    );
    assert!(
        !retry.complete(),
        "a retry moved nothing and cannot vouch for the listed copy: {retry:?}"
    );
    let gone = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
    assert!(gone.complete(), "{gone:?}");
}

#[tokio::test]
async fn a_moved_service_with_a_copy_outside_the_retiring_set_is_not_complete() {
    for pointer in [pointer('b', &['5'], &['1']), pointer('a', &['9'], &['1'])] {
        let mut client = Scripted {
            observations: [Ok(vec![listed(qualified("web"), &['1', '5'])])].into(),
            convergences: [Converged::Moved {
                moves: one_move(),
                replaced: one_replaced(),
            }]
            .into(),
            pointer: Some(pointer),
            ..Scripted::default()
        };
        let report = run(&mut client, &[], &["web"], &CancellationToken::new()).await;
        assert_eq!(outcomes(&report), [entry("web", moved())]);
        assert_eq!(
            report.remaining,
            Remaining::Observed {
                services: vec![qualified("web")],
                unchosen: Vec::new(),
                retiring: Vec::new(),
            }
        );
        assert!(!report.complete(), "{report:?}");
    }
}
