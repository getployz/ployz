//! The Drain's wire contract and scope, and how execution sequences what it observes.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;

use ployz_core::{
    CopyRotation, DeploymentLogId, Machine, MachineId, MachineName, Namespace, QualifiedService,
    RpcError, WireGuardPublicKey,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{
    DrainClient, DrainOutcome, DrainReport, DrainScope, DrainStop, Lost, MachineRef, Move,
    MoveFailure, Ready, Remaining, ServiceDrain, ServicesRole, StayReason, execute,
};
use crate::deploy::cleanup::Copy;
use crate::drain::placement::{Converged, LastIn, Moving, OldCopyEnd, Unconfirmed};
use crate::global_fixtures::container_id;

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
    assert!(DrainScope::EveryNamespace.includes(&service("ployz/api")));
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
    let entry = ServiceDrain::of(
        QualifiedService::parse("shop/web").unwrap(),
        DrainOutcome::Failed {
            moves: vec![Move {
                from: server('b', "web-2"),
                to: server('a', "web-1"),
                old: OldCopyEnd::Stopped,
            }],
            failure: MoveFailure::Refused {
                from: None,
                reason: StayReason::MidRollout,
            },
        },
    );
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        json!({
            "service": "shop/web",
            "result": "failed",
            "moves": [{
                "from": { "id": "b".repeat(32), "name": "web-2" },
                "to": { "id": "a".repeat(32), "name": "web-1" },
                "old": { "kind": "stopped" },
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
    observations: VecDeque<Result<Vec<QualifiedService>, Lost>>,
    convergences: VecDeque<Converged>,
    /// Cancelled once the first Service converges.
    cancel_after_converge: Option<CancellationToken>,
    copies: VecDeque<Result<Vec<Copy>, RpcError>>,
    ends: VecDeque<OldCopyEnd>,
    let_go: Vec<Copy>,
    /// How long an old copy takes to retire; `None` retires it at once.
    retire_delay: Option<fn() -> tokio::time::Sleep>,
    last_in: Vec<QualifiedService>,
    calls: Arc<Mutex<Vec<String>>>,
}

impl Scripted {
    fn retiring(&self, service: &QualifiedService, moves: Vec<Move>) -> Vec<Moving> {
        self.calls.lock().unwrap().push(format!("in {service}"));
        moves
            .into_iter()
            .map(|Move { from, to, old }| {
                let calls = Arc::clone(&self.calls);
                let retire_delay = self.retire_delay;
                let refused = self.last_in.contains(service);
                let service = service.clone();
                let old = async move {
                    if let Some(delay) = retire_delay {
                        delay().await;
                    }
                    if refused {
                        return Err(LastIn);
                    }
                    calls.lock().unwrap().push(format!("gone {service}"));
                    Ok(old)
                };
                Moving {
                    from,
                    to,
                    old: old.boxed(),
                }
            })
            .collect()
    }
}

impl DrainClient for Scripted {
    async fn still_on(&mut self, _id: &MachineId) -> Result<Vec<QualifiedService>, Lost> {
        self.observations
            .pop_front()
            .expect("one scripted observation per look")
    }

    async fn converge(
        &mut self,
        service: &QualifiedService,
        _cancellation: &CancellationToken,
    ) -> Converged<Moving> {
        if let Some(cancellation) = &self.cancel_after_converge {
            cancellation.cancel();
        }
        match self
            .convergences
            .pop_front()
            .expect("one scripted convergence per Service")
        {
            Converged::Moved { moves } => Converged::Moved {
                moves: self.retiring(service, moves),
            },
            Converged::Failed { moves, failure } => Converged::Failed {
                moves: self.retiring(service, moves),
                failure,
            },
            Converged::Stopped { moves, stop } => Converged::Stopped {
                moves: self.retiring(service, moves),
                stop,
            },
            Converged::NothingToMove => Converged::NothingToMove,
            Converged::Stays { reason } => Converged::Stays { reason },
        }
    }

    async fn copies_on(
        &mut self,
        _server: &MachineId,
        _service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError> {
        self.copies.pop_front().unwrap_or_else(|| Ok(Vec::new()))
    }

    async fn let_go(&mut self, copy: &Copy, _cancellation: &CancellationToken) -> OldCopyEnd {
        self.let_go.push(copy.clone());
        self.ends
            .pop_front()
            .expect("one scripted end per Out copy")
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
        join_approval: None,
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
        old: OldCopyEnd::Stopped,
    }]
}

fn moved() -> DrainOutcome {
    DrainOutcome::Moved { moves: one_move() }
}

async fn run(
    client: &mut Scripted,
    replicated: &[&str],
    cancellation: &CancellationToken,
) -> DrainReport {
    run_within(client, DrainScope::EveryNamespace, replicated, cancellation).await
}

async fn run_within(
    client: &mut Scripted,
    scope: DrainScope,
    replicated: &[&str],
    cancellation: &CancellationToken,
) -> DrainReport {
    let ready = Ready {
        server: drained(),
        scope,
        services_role: ServicesRole::TurnedOff,
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

fn left_on_server(rotation: CopyRotation) -> Copy {
    Copy {
        machine_id: drained().id,
        service: qualified("web"),
        container_id: container_id('c'),
        rotation,
    }
}

fn running(unconfirmed: Unconfirmed) -> OldCopyEnd {
    OldCopyEnd::Running { unconfirmed }
}

#[tokio::test]
async fn a_copy_held_by_the_user_stays_and_leaves_the_drain_incomplete() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::NothingToMove].into(),
        copies: [Ok(vec![left_on_server(CopyRotation::Held(
            DeploymentLogId::parse("you").unwrap(),
        ))])]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(outcomes(&report), [entry("web", DrainOutcome::Held)]);
    assert!(
        client.let_go.is_empty(),
        "a Held copy is neither moved nor let go"
    );
    assert!(!report.complete());
}

#[tokio::test]
async fn an_out_copy_left_on_the_server_is_let_go_and_never_moved() {
    let out = left_on_server(CopyRotation::Out);
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::NothingToMove].into(),
        copies: [Ok(vec![out.clone()])].into(),
        ends: [OldCopyEnd::Stopped].into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry(
            "web",
            DrainOutcome::OldCopies {
                ends: vec![OldCopyEnd::Stopped],
            },
        )]
    );
    assert_eq!(client.let_go, [out]);
    assert!(report.complete());
}

#[tokio::test]
async fn a_move_beside_an_out_copy_reports_the_move_then_the_old_copy() {
    let proxies = Unconfirmed::Proxies {
        servers: vec![MachineRef {
            id: MachineId::parse("c".repeat(32)).unwrap(),
            name: MachineName::parse("web-3").unwrap(),
        }],
    };
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved { moves: one_move() }].into(),
        copies: [Ok(vec![left_on_server(CopyRotation::Out)])].into(),
        ends: [running(proxies.clone())].into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [
            entry("web", moved()),
            entry(
                "web",
                DrainOutcome::OldCopies {
                    ends: vec![running(proxies)],
                },
            ),
        ]
    );
    assert!(
        !report.complete(),
        "an old copy left running keeps the drain open"
    );
}

/// A move off another draining Server, as a concurrent standalone Drain's convergence
/// of the same Service makes.
fn a_move_off_web_3() -> Move {
    Move {
        from: MachineRef {
            id: MachineId::parse("c".repeat(32)).unwrap(),
            name: MachineName::parse("web-3").unwrap(),
        },
        to: MachineRef {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("web-1").unwrap(),
        },
        old: OldCopyEnd::Stopped,
    }
}

#[tokio::test]
async fn a_drain_reports_only_the_moves_off_its_own_server() {
    let both = [one_move(), vec![a_move_off_web_3()]].concat();
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [
            Converged::Moved { moves: both },
            Converged::Moved {
                moves: vec![a_move_off_web_3()],
            },
        ]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web", "api"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [
            entry("web", moved()),
            entry("api", DrainOutcome::NothingToMove)
        ]
    );
    assert!(report.complete());
}

#[tokio::test]
async fn another_server_s_old_copy_left_running_is_still_reported() {
    let proxies = Unconfirmed::Proxies {
        servers: Vec::new(),
    };
    let stuck = Move {
        old: running(proxies),
        ..a_move_off_web_3()
    };
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: vec![stuck.clone()],
        }]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry("web", DrainOutcome::Moved { moves: vec![stuck] })]
    );
    assert!(!report.complete());
}

fn web_3_failed() -> MoveFailure {
    let Move { from, to, .. } = a_move_off_web_3();
    MoveFailure::ReadImage {
        from,
        to,
        detail: "web-3 did not answer".into(),
    }
}

#[tokio::test]
async fn another_server_s_failed_move_is_reported_without_failing_this_drain() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Failed {
            moves: one_move(),
            failure: web_3_failed(),
        }]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry(
            "web",
            DrainOutcome::Failed {
                moves: one_move(),
                failure: web_3_failed(),
            }
        )]
    );
    assert!(report.complete());
}

#[tokio::test]
async fn another_server_s_failed_move_fails_this_drain_when_something_was_left() {
    let Move { from, to, .. } = a_move_off_web_3();
    let left = [
        // A copy here stayed.
        (web_3_failed(), Ok(vec![qualified("web")])),
        // This Drain's own cancellation, or a replacement it started, are its to report.
        (
            MoveFailure::Cancelled {
                from: from.clone(),
                to: to.clone(),
                replacement_removed: true,
            },
            nothing_left(),
        ),
        (
            MoveFailure::NotServing {
                from,
                to,
                detail: "never became healthy".into(),
                replacement_removed: false,
                needs_upgrade: false,
            },
            nothing_left(),
        ),
    ];
    let stuck = Move {
        old: running(Unconfirmed::Proxies {
            servers: Vec::new(),
        }),
        ..a_move_off_web_3()
    };
    let left = left
        .into_iter()
        .map(|(failure, remaining)| (failure, one_move(), remaining))
        // Another Server's old copy this convergence left running.
        .chain([(
            web_3_failed(),
            [one_move(), vec![stuck]].concat(),
            nothing_left(),
        )]);
    for (failure, moves, remaining) in left {
        let mut client = Scripted {
            observations: [remaining].into(),
            convergences: [Converged::Failed {
                moves,
                failure: failure.clone(),
            }]
            .into(),
            ..Scripted::default()
        };
        let report = run(&mut client, &["web"], &CancellationToken::new()).await;
        assert!(!report.complete(), "{failure:?}");
    }
}

#[tokio::test]
async fn only_another_server_s_moves_beside_an_out_copy_report_just_the_old_copy() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: vec![a_move_off_web_3()],
        }]
        .into(),
        copies: [Ok(vec![left_on_server(CopyRotation::Out)])].into(),
        ends: [OldCopyEnd::Stopped].into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry(
            "web",
            DrainOutcome::OldCopies {
                ends: vec![OldCopyEnd::Stopped],
            },
        )]
    );
    assert!(
        client
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call == "gone app/web"),
        "the other Server's old copy was still waited for"
    );
}

#[tokio::test]
async fn a_move_whose_old_copy_is_still_in_leaves_the_drain_incomplete() {
    let moves = vec![Move {
        old: OldCopyEnd::In {
            unconfirmed: Unconfirmed::Failed {
                detail: "the Server did not answer".into(),
            },
        },
        ..one_move().remove(0)
    }];
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved {
            moves: moves.clone(),
        }]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry("web", DrainOutcome::Moved { moves })]
    );
    assert!(!report.complete());
}

#[tokio::test]
async fn the_last_copy_in_rotation_stays_and_leaves_the_drain_incomplete() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Stays {
            reason: StayReason::LastIn,
        }]
        .into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry(
            "web",
            DrainOutcome::Stays {
                reason: StayReason::LastIn,
            },
        )]
    );
    assert!(!report.complete());
}

#[tokio::test]
async fn cancelled_after_the_move_leaves_the_out_copy_running_without_asking() {
    let cancellation = CancellationToken::new();
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved { moves: one_move() }].into(),
        copies: [Ok(vec![left_on_server(CopyRotation::Out)])].into(),
        cancel_after_converge: Some(cancellation.clone()),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &cancellation).await;
    assert_eq!(
        outcomes(&report),
        [
            entry("web", moved()),
            entry(
                "web",
                DrainOutcome::OldCopies {
                    ends: vec![running(Unconfirmed::Cancelled)],
                },
            ),
        ]
    );
    assert!(client.let_go.is_empty(), "a cancelled drain sends no Gone");
    assert!(!report.complete());
}

#[tokio::test]
async fn a_failed_copy_listing_leaves_the_leftovers_to_remaining() {
    let mut client = Scripted {
        observations: [Ok(vec![qualified("web")])].into(),
        convergences: [Converged::NothingToMove].into(),
        copies: [Err(RpcError::unavailable("the Server did not answer"))].into(),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [entry("web", DrainOutcome::NothingToMove)]
    );
    assert!(client.let_go.is_empty());
    assert_eq!(
        report.remaining,
        Remaining::Observed {
            services: vec![qualified("web")],
            unchosen: Vec::new(),
        }
    );
}

#[tokio::test]
async fn cancelled_between_services_keeps_the_moves_made() {
    let cancellation = CancellationToken::new();
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [Converged::Moved { moves: one_move() }].into(),
        cancel_after_converge: Some(cancellation.clone()),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web", "api"], &cancellation).await;
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
    assert_eq!(
        serde_json::to_value(&report).unwrap().get("complete"),
        Some(&json!(false))
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
        convergences: [Converged::Moved { moves: one_move() }, lost].into(),
        ..Scripted::default()
    };
    let report = run(
        &mut client,
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
async fn a_full_drain_is_complete_and_names_the_user_services_its_scope_left_alone() {
    let blog = QualifiedService::parse("blog/web").unwrap();
    let mut client = Scripted {
        observations: [Ok(vec![blog.clone(), QualifiedService::system_ingress()])].into(),
        convergences: [Converged::Moved { moves: one_move() }].into(),
        ..Scripted::default()
    };
    let scope = DrainScope::Owned {
        namespaces: vec![namespace("app")],
    };
    let report = run_within(&mut client, scope, &["web"], &CancellationToken::new()).await;
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

#[tokio::test(start_paused = true)]
async fn old_copy_retires_overlap_the_next_move() {
    let mut client = Scripted {
        observations: [nothing_left()].into(),
        convergences: [
            Converged::Moved { moves: one_move() },
            Converged::Moved { moves: one_move() },
        ]
        .into(),
        retire_delay: Some(|| tokio::time::sleep(Duration::from_secs(5))),
        ..Scripted::default()
    };
    let report = run(&mut client, &["web", "api"], &CancellationToken::new()).await;
    let calls = client.calls.lock().unwrap().clone();
    let at = |call: &str| calls.iter().position(|seen| seen == call).unwrap();
    assert!(
        at("in app/api") < at("gone app/web"),
        "the next move starts before the old copy is Gone: {calls:?}"
    );
    assert_eq!(
        outcomes(&report),
        [entry("web", moved()), entry("api", moved())]
    );
    assert!(report.complete());
}

#[tokio::test(start_paused = true)]
async fn an_old_copy_refused_out_keeps_its_move_and_stays() {
    let mut client = Scripted {
        observations: [Ok(vec![qualified("web")])].into(),
        convergences: [
            Converged::Moved { moves: one_move() },
            Converged::Moved { moves: one_move() },
        ]
        .into(),
        retire_delay: Some(|| tokio::time::sleep(Duration::from_secs(5))),
        last_in: vec![qualified("web")],
        ..Scripted::default()
    };
    let report = run(&mut client, &["web", "api"], &CancellationToken::new()).await;
    assert_eq!(
        outcomes(&report),
        [
            entry(
                "web",
                DrainOutcome::Failed {
                    moves: one_move()
                        .into_iter()
                        .map(|step| Move {
                            old: OldCopyEnd::In {
                                unconfirmed: Unconfirmed::Failed {
                                    detail: format!(
                                        "{} still counted it as the last copy serving",
                                        step.from.name
                                    ),
                                },
                            },
                            ..step
                        })
                        .collect(),
                    failure: MoveFailure::Refused {
                        from: None,
                        reason: StayReason::LastIn,
                    },
                }
            ),
            entry("api", moved()),
        ]
    );
    assert!(!report.complete());
}

/// One Service's rows for every convergence beside every kind of leftover, and whether an
/// old copy was refused out as the last one serving.
#[tokio::test(start_paused = true)]
async fn a_service_s_rows_follow_from_its_convergence_its_leftovers_and_a_refused_old_copy() {
    let held = || left_on_server(CopyRotation::Held(DeploymentLogId::parse("you").unwrap()));
    let out = || left_on_server(CopyRotation::Out);
    let refused_move = || {
        one_move()
            .into_iter()
            .map(|step| Move {
                old: OldCopyEnd::In {
                    unconfirmed: Unconfirmed::Failed {
                        detail: format!(
                            "{} still counted it as the last copy serving",
                            step.from.name
                        ),
                    },
                },
                ..step
            })
            .collect::<Vec<_>>()
    };
    let last_in = || MoveFailure::Refused {
        from: None,
        reason: StayReason::LastIn,
    };
    let failed = |moves, failure| DrainOutcome::Failed { moves, failure };
    let old_copies = || DrainOutcome::OldCopies {
        ends: vec![OldCopyEnd::Stopped],
    };
    let stays = || DrainOutcome::Stays {
        reason: StayReason::LastIn,
    };
    let other = || Converged::Moved {
        moves: vec![a_move_off_web_3()],
    };
    let own = || Converged::Moved { moves: one_move() };
    let both = || Converged::Moved {
        moves: [one_move(), vec![a_move_off_web_3()]].concat(),
    };
    let own_failed = || Converged::Failed {
        moves: one_move(),
        failure: web_3_failed(),
    };
    let stopped = || Converged::Stopped {
        moves: one_move(),
        stop: DrainStop::Cancelled,
    };
    // (case, convergence, copies left on the Server, old copy refused out, rows, stop)
    type Case = (
        &'static str,
        Converged,
        Vec<Copy>,
        bool,
        Vec<DrainOutcome>,
        Option<DrainStop>,
    );
    let table: Vec<Case> = vec![
        (
            "nothing to move",
            Converged::NothingToMove,
            vec![],
            false,
            vec![DrainOutcome::NothingToMove],
            None,
        ),
        (
            "nothing to move beside a held copy",
            Converged::NothingToMove,
            vec![held()],
            false,
            vec![DrainOutcome::Held],
            None,
        ),
        (
            "nothing to move beside an out copy",
            Converged::NothingToMove,
            vec![out()],
            false,
            vec![old_copies()],
            None,
        ),
        (
            "nothing to move beside both",
            Converged::NothingToMove,
            vec![held(), out()],
            false,
            vec![DrainOutcome::Held, old_copies()],
            None,
        ),
        (
            "stays",
            Converged::Stays {
                reason: StayReason::LastIn,
            },
            vec![],
            false,
            vec![stays()],
            None,
        ),
        (
            "stays beside both",
            Converged::Stays {
                reason: StayReason::LastIn,
            },
            vec![held(), out()],
            false,
            vec![stays(), DrainOutcome::Held, old_copies()],
            None,
        ),
        ("moved", own(), vec![], false, vec![moved()], None),
        (
            "moved beside a held copy",
            own(),
            vec![held()],
            false,
            vec![moved(), DrainOutcome::Held],
            None,
        ),
        (
            "moved beside an out copy",
            both(),
            vec![out()],
            false,
            vec![moved(), old_copies()],
            None,
        ),
        (
            "only another Server's move",
            other(),
            vec![],
            false,
            vec![DrainOutcome::NothingToMove],
            None,
        ),
        (
            "only another Server's move beside a held copy",
            other(),
            vec![held()],
            false,
            vec![DrainOutcome::Held],
            None,
        ),
        (
            "only another Server's move beside an out copy",
            other(),
            vec![out()],
            false,
            vec![old_copies()],
            None,
        ),
        (
            "moved but refused out",
            own(),
            vec![],
            true,
            vec![failed(refused_move(), last_in())],
            None,
        ),
        (
            "moved but refused out beside an out copy",
            own(),
            vec![out()],
            true,
            vec![failed(refused_move(), last_in()), old_copies()],
            None,
        ),
        (
            "failed",
            own_failed(),
            vec![],
            false,
            vec![failed(one_move(), web_3_failed())],
            None,
        ),
        (
            "failed beside a held copy",
            own_failed(),
            vec![held()],
            false,
            vec![failed(one_move(), web_3_failed()), DrainOutcome::Held],
            None,
        ),
        (
            "failed and refused out",
            own_failed(),
            vec![],
            true,
            vec![
                failed(refused_move(), web_3_failed()),
                failed(Vec::new(), last_in()),
            ],
            None,
        ),
        (
            "failed and refused out beside an out copy",
            own_failed(),
            vec![out()],
            true,
            vec![
                failed(refused_move(), web_3_failed()),
                failed(Vec::new(), last_in()),
                old_copies(),
            ],
            None,
        ),
        (
            "interrupted",
            stopped(),
            vec![],
            false,
            vec![DrainOutcome::Interrupted { moves: one_move() }],
            Some(DrainStop::Cancelled),
        ),
        (
            "interrupted beside leftovers reports none of them",
            stopped(),
            vec![held(), out()],
            false,
            vec![DrainOutcome::Interrupted { moves: one_move() }],
            Some(DrainStop::Cancelled),
        ),
        (
            "interrupted and refused out",
            stopped(),
            vec![],
            true,
            vec![
                DrainOutcome::Interrupted {
                    moves: refused_move(),
                },
                failed(Vec::new(), last_in()),
            ],
            Some(DrainStop::Cancelled),
        ),
    ];
    for (case, convergence, copies, refused, rows, stop) in table {
        let ends = copies
            .iter()
            .filter(|copy| copy.rotation == CopyRotation::Out)
            .map(|_| OldCopyEnd::Stopped)
            .collect();
        let mut client = Scripted {
            observations: [nothing_left()].into(),
            convergences: [convergence].into(),
            copies: [Ok(copies)].into(),
            ends,
            retire_delay: Some(|| tokio::time::sleep(Duration::from_secs(1))),
            last_in: if refused {
                vec![qualified("web")]
            } else {
                vec![]
            },
            ..Scripted::default()
        };
        let report = run(&mut client, &["web"], &CancellationToken::new()).await;
        assert_eq!(
            outcomes(&report),
            rows.into_iter()
                .map(|row| entry("web", row))
                .collect::<Vec<_>>(),
            "{case}"
        );
        assert_eq!(report.stopped, stop, "{case}");
    }
}

/// What the CLI reads off each outcome: whether an old copy is still on the Server, and
/// whether the Drain went wrong on the Service.
#[test]
fn an_outcome_says_whether_it_left_an_old_copy_or_failed() {
    let left = |old| {
        vec![Move {
            old,
            ..one_move().remove(0)
        }]
    };
    let running = || OldCopyEnd::Running {
        unconfirmed: Unconfirmed::Cancelled,
    };
    let in_rotation = || OldCopyEnd::In {
        unconfirmed: Unconfirmed::Cancelled,
    };
    let failure = || MoveFailure::Refused {
        from: None,
        reason: StayReason::LastIn,
    };
    let table = [
        ("moved", moved(), (false, false)),
        (
            "moved, old copy running",
            DrainOutcome::Moved {
                moves: left(running()),
            },
            (true, false),
        ),
        (
            "moved, old copy in",
            DrainOutcome::Moved {
                moves: left(in_rotation()),
            },
            (true, false),
        ),
        (
            "nothing to move",
            DrainOutcome::NothingToMove,
            (false, false),
        ),
        (
            "failed after a stopped move",
            DrainOutcome::Failed {
                moves: one_move(),
                failure: failure(),
            },
            (false, true),
        ),
        (
            "failed, old copy running",
            DrainOutcome::Failed {
                moves: left(running()),
                failure: failure(),
            },
            (false, true),
        ),
        (
            "stays",
            DrainOutcome::Stays {
                reason: StayReason::LastIn,
            },
            (false, false),
        ),
        ("held", DrainOutcome::Held, (false, false)),
        (
            "old copies stopped",
            DrainOutcome::OldCopies {
                ends: vec![OldCopyEnd::Stopped],
            },
            (false, false),
        ),
        (
            "old copies, one running",
            DrainOutcome::OldCopies {
                ends: vec![OldCopyEnd::Stopped, running()],
            },
            (true, false),
        ),
        ("retired", DrainOutcome::Retired, (false, false)),
        (
            "not retired",
            DrainOutcome::NotRetired {
                error: "refused".into(),
            },
            (false, true),
        ),
        (
            "interrupted",
            DrainOutcome::Interrupted {
                moves: left(running()),
            },
            (false, true),
        ),
        ("not attempted", DrainOutcome::NotAttempted, (false, false)),
    ];
    for (label, outcome, (left_old_copy, failed)) in table {
        assert_eq!(outcome.left_old_copy(), left_old_copy, "{label}");
        assert_eq!(outcome.failed(), failed, "{label}");
    }
}

/// What Server `web-2` answers at one look of a Drain.
enum Look {
    Lists(Vec<ployz_core::ContainerObservation>),
    /// Its Container listing fails, as a Server that doesn't answer.
    Fails,
    /// The Machine list leaves it out, so nothing asks it.
    Unlisted,
}

/// Copies of `app/metrics` that `web-2` may list.
struct Metrics {
    /// A Global copy, In, as Compose made one before 0.1.2-beta.42.
    global: ployz_core::ContainerObservation,
    /// A Global copy rotated Out.
    out_global: ployz_core::ContainerObservation,
    /// A replicated copy.
    replicated: ployz_core::ContainerObservation,
}

/// The Drain of Server `web-2` within `scope` against a Cluster that answers reads and the
/// services role, and every other call it sent, turning that role off included. Its peer
/// `web-1` runs a copy of `app/metrics`, a user Global left from Compose before
/// 0.1.2-beta.42. `look` answers for `web-2` by how many Machine listings came before it.
async fn drain_reading_only(
    accepts_services: bool,
    scope: DrainScope,
    look: fn(usize, &Metrics) -> Look,
) -> (Result<DrainReport, super::DrainError>, Vec<String>) {
    use crate::global_fixtures::{global_service, machine, qualified, running_on};
    use ployz_core::{
        ContainerList, MachineList, MachineObservation, MachineTarget, MachineUpdated,
        MembershipObservation, OpaquePayload, Placement, RpcRequestBody, RpcResponse,
    };
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use tonic::{Request, Response};

    let drained = Machine {
        accepts_services,
        ..machine('1', "web-2")
    };
    let peer = machine('2', "web-1");
    let on = |server: &Machine, hex| {
        global_service(
            qualified("app", "metrics"),
            'a',
            Placement::default(),
            running_on(server, hex),
        )
        .containers
        .remove(0)
        .into_observation()
    };
    let mut out_global = on(&drained, 'c');
    out_global
        .try_update(|parts| parts.rotation = Some(CopyRotation::Out))
        .unwrap();
    let mut replicated = on(&drained, 'd');
    replicated
        .try_update(|parts| {
            parts.resolved_spec.mode = ployz_core::ServiceMode::Replicated {
                replicas: NonZeroU32::new(2).unwrap(),
            };
        })
        .unwrap();
    let metrics = Arc::new(Metrics {
        global: on(&drained, 'a'),
        out_global,
        replicated,
    });
    let elsewhere = on(&peer, 'b');
    let (drained_id, peer_id) = (drained.id, peer.id);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let listings = Arc::new(AtomicUsize::new(0));
    let turned_off = Arc::new(AtomicBool::new(false));
    let machines = [drained.clone(), peer]
        .map(|machine| MachineObservation::new(machine, MembershipObservation::Up));
    let (mut client, server) = crate::connect::test_support::rpc_client({
        let asked = asked.clone();
        move |rpc: Request<OpaquePayload>| {
            let (asked, listings, machines) = (asked.clone(), listings.clone(), machines.clone());
            let (metrics, elsewhere) = (metrics.clone(), elsewhere.clone());
            let turned_off = turned_off.clone();
            let selected = rpc
                .metadata()
                .get(ployz_core::ONE_TARGET_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            async move {
                #[expect(
                    clippy::wildcard_enum_match_arm,
                    reason = "fixture answers only the reads a Drain makes"
                )]
                let response = match rpc.into_inner().decode_request().unwrap().body {
                    RpcRequestBody::UpdateMachine(update)
                        if update.update.accepts_services == Some(false) =>
                    {
                        turned_off.store(true, Ordering::SeqCst);
                        asked.lock().unwrap().push("services off".to_owned());
                        let mut machine = machines[0].machine.clone();
                        machine.accepts_services = false;
                        RpcResponse::from(MachineUpdated { machine })
                    }
                    RpcRequestBody::ListMachines(_) => {
                        let before = listings.fetch_add(1, Ordering::SeqCst) + 1;
                        let unlisted = matches!(look(before, &metrics), Look::Unlisted);
                        let off = turned_off.load(Ordering::SeqCst);
                        RpcResponse::from(MachineList {
                            enrollment: None,
                            machines: machines
                                .into_iter()
                                .filter(|seen| !(unlisted && seen.machine.id == drained_id))
                                .map(|mut seen| {
                                    if off && seen.machine.id == drained_id {
                                        seen.machine.accepts_services = false;
                                    }
                                    seen
                                })
                                .collect(),
                        })
                    }
                    RpcRequestBody::ListContainers(_)
                        if selected.as_deref() == Some(peer_id.as_str()) =>
                    {
                        RpcResponse::from(ContainerList {
                            containers: vec![elsewhere],
                        })
                    }
                    RpcRequestBody::ListContainers(_)
                        if selected.as_deref() == Some(drained_id.as_str()) =>
                    {
                        match look(listings.load(Ordering::SeqCst), &metrics) {
                            Look::Lists(containers) => {
                                RpcResponse::from(ContainerList { containers })
                            }
                            Look::Fails | Look::Unlisted => RpcResponse::from(RpcError::new(
                                ployz_core::RpcErrorCode::Internal,
                                "the Server did not list its Containers",
                            )),
                        }
                    }
                    other => {
                        asked.lock().unwrap().push(format!("{other:?}"));
                        panic!("the Drain asked for more than a read: {other:?}")
                    }
                };
                Ok(Response::new(response.encode().unwrap()))
            }
        }
    })
    .await;

    let drained = client
        .drain(
            &MachineTarget::from(&drained.id),
            &scope,
            crate::deploy::Approvals::default(),
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await;
    server.abort();
    let asked = asked.lock().unwrap().clone();
    (drained, asked)
}

/// The refusal a user Global gives a Drain: it names the Service and says to give it a
/// replica count. Retiring it could take the last copy that serves, since a copy stays In
/// while it crash-loops.
fn assert_refused_for_metrics(drained: Result<DrainReport, super::DrainError>) {
    let Err(super::DrainError::Refused(refused)) = drained else {
        panic!("expected a refusal, got {drained:?}");
    };
    assert_eq!(refused.code, ployz_core::RpcErrorCode::Conflict);
    assert!(
        refused.message.contains("`app/metrics`"),
        "{}",
        refused.message
    );
    assert!(
        refused.message.contains("replica count"),
        "{}",
        refused.message
    );
}

/// A user Global refuses the Drain before its services role changes.
#[tokio::test]
async fn a_user_global_refuses_the_drain_before_the_role_changes() {
    let (drained, asked) = drain_reading_only(true, DrainScope::EveryNamespace, |_, metrics| {
        Look::Lists(vec![metrics.global.clone()])
    })
    .await;

    assert_refused_for_metrics(drained);
    assert!(asked.is_empty(), "{asked:?}");
}

/// A Drain that can't list the Server's Containers can't tell whether it runs a user
/// Global, so it changes nothing, whether or not its role was already off. A later listing
/// that finds one must not reach a retirement.
#[tokio::test]
async fn a_drain_that_cannot_list_the_server_changes_nothing() {
    for accepts_services in [true, false] {
        let (drained, asked) = drain_reading_only(
            accepts_services,
            DrainScope::EveryNamespace,
            |before, metrics| {
                if before > 1 {
                    Look::Lists(vec![metrics.global.clone()])
                } else {
                    Look::Fails
                }
            },
        )
        .await;

        assert!(
            matches!(drained, Err(super::DrainError::Unobservable { .. })),
            "accepts_services {accepts_services}: {drained:?}"
        );
        assert!(
            asked.is_empty(),
            "accepts_services {accepts_services}: {asked:?}"
        );
    }
}

/// A user Global first seen after the services role is off still refuses the Drain: no
/// observation of one reaches a retirement.
#[tokio::test]
async fn a_user_global_seen_after_the_role_is_off_still_refuses() {
    let (drained, asked) =
        drain_reading_only(false, DrainScope::EveryNamespace, |before, metrics| {
            Look::Lists(if before > 1 {
                vec![metrics.global.clone()]
            } else {
                Vec::new()
            })
        })
        .await;

    assert_refused_for_metrics(drained);
    assert!(asked.is_empty(), "{asked:?}");
}

/// A Service with a Global copy anywhere refuses the Drain, even when the Server also runs
/// a replicated copy of it: the Drain would let go of the Global copy left Out there, and a
/// copy In elsewhere may crash-loop.
#[tokio::test]
async fn a_service_with_any_global_copy_refuses_the_drain() {
    let (drained, asked) = drain_reading_only(true, DrainScope::EveryNamespace, |_, metrics| {
        Look::Lists(vec![metrics.replicated.clone(), metrics.out_global.clone()])
    })
    .await;

    assert_refused_for_metrics(drained);
    assert!(asked.is_empty(), "{asked:?}");
}

/// A Server the Machine list leaves out at a later look was never asked what it runs by
/// then, so the Drain can't read it as empty.
#[tokio::test]
async fn a_server_left_out_of_the_machine_list_is_not_read_as_empty() {
    let (drained, asked) = drain_reading_only(false, DrainScope::EveryNamespace, |before, _| {
        if before > 1 {
            Look::Unlisted
        } else {
            Look::Lists(Vec::new())
        }
    })
    .await;

    assert!(
        matches!(drained, Err(super::DrainError::Unobservable { .. })),
        "{drained:?}"
    );
    assert!(asked.is_empty(), "{asked:?}");
}

/// A user Global the Drain's scope leaves out still refuses it, as Cloud's saved services
/// would leave out one that appeared after Cloud read its frame. Its copy would otherwise
/// sit on a Server whose services are off.
#[tokio::test]
async fn a_user_global_outside_the_scope_still_refuses() {
    let scope = DrainScope::Services {
        services: Vec::new(),
    };
    let (drained, asked) = drain_reading_only(false, scope, |_, metrics| {
        Look::Lists(vec![metrics.global.clone()])
    })
    .await;

    assert_refused_for_metrics(drained);
    assert!(asked.is_empty(), "{asked:?}");
}

/// A Drain whose Server answered before it turned services off, then went unlisted, says
/// it left services off: Cloud and the CLI record what changed, not a plain refusal.
#[tokio::test]
async fn a_server_unlisted_after_its_services_went_off_says_they_are_off() {
    let (drained, asked) = drain_reading_only(true, DrainScope::EveryNamespace, |before, _| {
        if before > 1 {
            Look::Fails
        } else {
            Look::Lists(Vec::new())
        }
    })
    .await;

    let Err(error @ super::DrainError::Unobservable { .. }) = drained else {
        panic!("expected Unobservable, got {drained:?}");
    };
    assert_eq!(
        error.to_string(),
        "Cannot observe Services on Server web-2 after turning its services off: \
         the Server did not list its Containers"
    );
    assert_eq!(asked, ["services off"]);
}
