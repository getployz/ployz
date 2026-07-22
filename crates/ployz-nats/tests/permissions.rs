use ployz_core::ids::{BuildExecutorId, BuildPoolId, OperationId};
use ployz_core::security::NatsPrincipal;
use ployz_nats::permissions::{NatsPermissionProfile, inbox_prefix, inbox_subscribe_scope};
use ployz_nats::services::{
    API_SERVICE_NAME, BUILD_EXECUTOR_SERVICE_NAME, DNS_SERVICE_NAME, GATEWAY_MACHINE_SERVICE_NAME,
    INGRESS_ENDPOINT_SERVICE_NAME, INTENT_SERVICE_NAME, MACHINE_SERVICE_NAME,
    RUNTIME_PROJECTION_SERVICE_NAME,
};
use ployz_nats::subjects::{
    BuildExecutorServiceEndpoint, CoreQueryEndpoint, INGRESS_ENDPOINT_CHANGED, INTENT_CHANGED,
    MachineServiceEndpoint, OperationApiCaller, OperationApiEndpoint,
    PENDING_MACHINE_JOINS_CHANGED, RUNTIME_SNAPSHOT_SEED, RUNTIME_SNAPSHOT_STREAM,
    build_executor_log, build_executor_log_publish_scope, build_executor_service,
    build_executor_service_scope, gateway_status, machine_build_log, machine_container_facts,
    machine_facts, machine_service, machine_service_scope,
};
use ployz_test_support::ids::machine_id;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Publish,
    Subscribe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatrixPrincipal {
    Controller,
    Operator,
    CloudOperator,
    MachineA,
    MachineB,
    ExecutorA,
    Join,
}

impl MatrixPrincipal {
    const ALL: &[Self] = &[
        Self::Controller,
        Self::Operator,
        Self::CloudOperator,
        Self::MachineA,
        Self::MachineB,
        Self::ExecutorA,
        Self::Join,
    ];

    fn principal(self) -> NatsPrincipal {
        match self {
            Self::Controller => NatsPrincipal::Controller,
            Self::Operator | Self::CloudOperator => NatsPrincipal::Operator,
            Self::MachineA => NatsPrincipal::Machine {
                machine_id: machine_id("machine-a"),
            },
            Self::MachineB => NatsPrincipal::Machine {
                machine_id: machine_id("machine-b"),
            },
            Self::ExecutorA => executor("pool-a", "executor-a"),
            Self::Join => NatsPrincipal::Join,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AuthorityCell {
    principal: MatrixPrincipal,
    direction: Direction,
}

impl Direction {
    const BOTH: &[Self] = &[Self::Publish, Self::Subscribe];
}

struct MatrixCase {
    label: String,
    subject: String,
    allowed: Vec<AuthorityCell>,
}

fn cell(principal: MatrixPrincipal, direction: Direction) -> AuthorityCell {
    AuthorityCell {
        principal,
        direction,
    }
}

#[test]
fn exhaustive_subject_family_principal_direction_matrix_is_intentional() {
    let mut cases = Vec::new();

    for endpoint in OperationApiEndpoint::ALL.iter().copied() {
        let mut allowed = vec![cell(MatrixPrincipal::Controller, Direction::Subscribe)];
        if endpoint == OperationApiEndpoint::InitFirstMachineActivate {
            allowed.push(cell(MatrixPrincipal::Controller, Direction::Publish));
        }
        match endpoint.caller() {
            OperationApiCaller::Operator => {
                allowed.push(cell(MatrixPrincipal::Operator, Direction::Publish));
                allowed.push(cell(MatrixPrincipal::CloudOperator, Direction::Publish));
            }
            OperationApiCaller::Join => {
                allowed.push(cell(MatrixPrincipal::Join, Direction::Publish));
            }
        }
        cases.push(MatrixCase {
            label: format!("operation {:?}", endpoint),
            subject: endpoint.subject().to_owned(),
            allowed,
        });
    }

    for endpoint in CoreQueryEndpoint::ALL.iter().copied() {
        cases.push(MatrixCase {
            label: format!("core query {:?}", endpoint),
            subject: endpoint.subject().to_owned(),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::Operator, Direction::Publish),
                cell(MatrixPrincipal::CloudOperator, Direction::Publish),
                cell(MatrixPrincipal::MachineA, Direction::Publish),
                cell(MatrixPrincipal::MachineB, Direction::Publish),
            ],
        });
    }
    cases.push(MatrixCase {
        label: "runtime snapshot seed".to_owned(),
        subject: RUNTIME_SNAPSHOT_SEED.to_owned(),
        allowed: vec![
            cell(MatrixPrincipal::Controller, Direction::Subscribe),
            cell(MatrixPrincipal::Operator, Direction::Publish),
            cell(MatrixPrincipal::CloudOperator, Direction::Publish),
        ],
    });

    for endpoint in MachineServiceEndpoint::ALL.iter().copied() {
        for (machine, owner) in [
            (machine_id("machine-a"), MatrixPrincipal::MachineA),
            (machine_id("machine-b"), MatrixPrincipal::MachineB),
        ] {
            let mut allowed = vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(owner, Direction::Subscribe),
            ];
            if MachineServiceEndpoint::IMAGE_TRANSFER.contains(&endpoint) {
                allowed.extend([
                    cell(MatrixPrincipal::Operator, Direction::Publish),
                    cell(MatrixPrincipal::CloudOperator, Direction::Publish),
                    cell(MatrixPrincipal::ExecutorA, Direction::Publish),
                ]);
            }
            cases.push(MatrixCase {
                label: format!("machine endpoint {:?} {}", endpoint, machine.as_str()),
                subject: machine_service(&machine, endpoint),
                allowed,
            });
        }
    }

    let pool = BuildPoolId::try_new("pool-a").expect("pool");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor");
    for endpoint in BuildExecutorServiceEndpoint::ALL.iter().copied() {
        for (case_pool, case_executor, own) in [
            ("pool-a", "executor-a", true),
            ("pool-b", "executor-a", false),
            ("pool-a", "executor-b", false),
        ] {
            let mut allowed = vec![cell(MatrixPrincipal::Controller, Direction::Publish)];
            if own {
                allowed.push(cell(MatrixPrincipal::ExecutorA, Direction::Subscribe));
            }
            cases.push(MatrixCase {
                label: format!(
                    "build executor endpoint {:?} {case_pool}/{case_executor}",
                    endpoint
                ),
                subject: build_executor_service(
                    &BuildPoolId::try_new(case_pool).expect("pool"),
                    &BuildExecutorId::try_new(case_executor).expect("executor"),
                    endpoint,
                ),
                allowed,
            });
        }
    }

    let operation_id = OperationId::try_new("op-a").expect("operation");
    let fixed = [
        MatrixCase {
            label: "operation progress".to_owned(),
            subject: "plz.v1.progress.cluster.operation.op-a.completed".to_owned(),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::Operator, Direction::Subscribe),
                cell(MatrixPrincipal::CloudOperator, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "runtime snapshot stream".to_owned(),
            subject: RUNTIME_SNAPSHOT_STREAM.to_owned(),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::Operator, Direction::Subscribe),
                cell(MatrixPrincipal::CloudOperator, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "intent invalidation".to_owned(),
            subject: INTENT_CHANGED.to_owned(),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "ingress invalidation".to_owned(),
            subject: INGRESS_ENDPOINT_CHANGED.to_owned(),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::Operator, Direction::Subscribe),
                cell(MatrixPrincipal::CloudOperator, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "pending join invalidation".to_owned(),
            subject: PENDING_MACHINE_JOINS_CHANGED.to_owned(),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "machine facts".to_owned(),
            subject: machine_facts(&machine_id("machine-a")),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Publish),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "machine container facts".to_owned(),
            subject: machine_container_facts(&machine_id("machine-a")),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Publish),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "gateway testimony".to_owned(),
            subject: gateway_status(&machine_id("machine-a")),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Publish),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
            ],
        },
        MatrixCase {
            label: "machine build log".to_owned(),
            subject: machine_build_log(&machine_id("machine-a"), &operation_id),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Publish),
            ],
        },
        MatrixCase {
            label: "external build log".to_owned(),
            subject: build_executor_log(&pool, &executor_id, &operation_id),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::ExecutorA, Direction::Publish),
            ],
        },
    ];
    cases.extend(fixed);
    for (label, subject) in [
        (
            "cross-pool external build log",
            "plz.v1.signal.build_executor.pool-b.executor-a.build.operation.op-a.log",
        ),
        (
            "cross-executor external build log",
            "plz.v1.signal.build_executor.pool-a.executor-b.build.operation.op-a.log",
        ),
    ] {
        cases.push(MatrixCase {
            label: label.to_owned(),
            subject: subject.to_owned(),
            allowed: vec![cell(MatrixPrincipal::Controller, Direction::Subscribe)],
        });
    }

    for (label, subject) in [
        (
            "future operator query",
            "plz.v1.rpc.operator.query.future.read",
        ),
        ("future core query", "plz.v1.rpc.core.query.future.get"),
        (
            "future machine command",
            "plz.v1.rpc.machine.command.machine-a.future.run",
        ),
        (
            "future build command",
            "plz.v1.rpc.build_executor.command.pool-a.executor-a.future.run",
        ),
        (
            "fourth image transfer",
            "plz.v1.rpc.machine.command.machine-a.image.layer.push",
        ),
        ("unrelated discovery", "$SRV.PING.unrelated-service.runtime"),
    ] {
        cases.push(MatrixCase {
            label: label.to_owned(),
            subject: subject.to_owned(),
            allowed: Vec::new(),
        });
    }

    let discovery_roles = [
        (API_SERVICE_NAME, MatrixPrincipal::Controller),
        (INTENT_SERVICE_NAME, MatrixPrincipal::Controller),
        (INGRESS_ENDPOINT_SERVICE_NAME, MatrixPrincipal::Controller),
        (RUNTIME_PROJECTION_SERVICE_NAME, MatrixPrincipal::Controller),
        (MACHINE_SERVICE_NAME, MatrixPrincipal::MachineA),
        (GATEWAY_MACHINE_SERVICE_NAME, MatrixPrincipal::MachineA),
        (DNS_SERVICE_NAME, MatrixPrincipal::MachineA),
        (BUILD_EXECUTOR_SERVICE_NAME, MatrixPrincipal::ExecutorA),
    ];
    for verb in ["PING", "INFO", "STATS"] {
        cases.push(MatrixCase {
            label: format!("discovery all {verb}"),
            subject: format!("$SRV.{verb}"),
            allowed: vec![
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
                cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                cell(MatrixPrincipal::MachineB, Direction::Subscribe),
                cell(MatrixPrincipal::ExecutorA, Direction::Subscribe),
            ],
        });
        for (service, owner) in discovery_roles.iter().copied() {
            let owners = match owner {
                MatrixPrincipal::MachineA | MatrixPrincipal::MachineB => vec![
                    cell(MatrixPrincipal::MachineA, Direction::Subscribe),
                    cell(MatrixPrincipal::MachineB, Direction::Subscribe),
                ],
                MatrixPrincipal::Controller
                | MatrixPrincipal::Operator
                | MatrixPrincipal::CloudOperator
                | MatrixPrincipal::ExecutorA
                | MatrixPrincipal::Join => vec![cell(owner, Direction::Subscribe)],
            };
            for suffix in [service.to_owned(), format!("{service}.runtime")] {
                cases.push(MatrixCase {
                    label: format!("discovery {verb} {suffix}"),
                    subject: format!("$SRV.{verb}.{suffix}"),
                    allowed: owners.clone(),
                });
            }
        }
    }

    for owner in MatrixPrincipal::ALL.iter().copied() {
        let subject = format!("{}.reply", inbox_prefix(&owner.principal()));
        let allowed = match owner {
            MatrixPrincipal::Controller => vec![
                cell(MatrixPrincipal::Controller, Direction::Publish),
                cell(MatrixPrincipal::Controller, Direction::Subscribe),
            ],
            MatrixPrincipal::Operator | MatrixPrincipal::CloudOperator => vec![
                cell(MatrixPrincipal::Operator, Direction::Subscribe),
                cell(MatrixPrincipal::CloudOperator, Direction::Subscribe),
            ],
            MatrixPrincipal::MachineA
            | MatrixPrincipal::MachineB
            | MatrixPrincipal::ExecutorA
            | MatrixPrincipal::Join => vec![cell(owner, Direction::Subscribe)],
        };
        cases.push(MatrixCase {
            label: format!("inbox {owner:?}"),
            subject,
            allowed,
        });
    }

    for case in cases {
        for principal in MatrixPrincipal::ALL.iter().copied() {
            for direction in Direction::BOTH.iter().copied() {
                let expected = case.allowed.contains(&cell(principal, direction));
                assert_eq!(
                    permits(principal.principal(), direction, &case.subject),
                    expected,
                    "{}: {principal:?} {direction:?} on {}",
                    case.label,
                    case.subject
                );
            }
        }
    }
}

fn permits(principal: NatsPrincipal, direction: Direction, subject: &str) -> bool {
    let profile = NatsPermissionProfile::render(principal);
    let permissions = match direction {
        Direction::Publish => &profile.publish,
        Direction::Subscribe => &profile.subscribe,
    };
    permissions
        .allowed_subjects()
        .iter()
        .any(|pattern| nats_subject_matches(pattern, subject))
        && permissions
            .denied_subjects()
            .iter()
            .all(|pattern| !nats_subject_matches(pattern, subject))
}

fn executor(pool: &str, executor: &str) -> NatsPrincipal {
    NatsPrincipal::BuildExecutor {
        pool_id: BuildPoolId::try_new(pool).expect("pool"),
        executor_id: BuildExecutorId::try_new(executor).expect("executor"),
    }
}

#[test]
fn non_system_rpc_image_and_discovery_grants_never_end_in_terminal_wildcards() {
    let principals = [
        NatsPrincipal::Controller,
        NatsPrincipal::Operator,
        NatsPrincipal::Join,
        NatsPrincipal::Machine {
            machine_id: machine_id("machine-a"),
        },
        executor("pool-a", "executor-a"),
    ];
    for principal in principals {
        let profile = NatsPermissionProfile::render(principal.clone());
        for grant in profile
            .publish
            .allowed_subjects()
            .iter()
            .chain(profile.subscribe.allowed_subjects())
        {
            if grant.ends_with('>') {
                assert!(
                    !grant.starts_with("plz.v1.rpc.")
                        && !grant.starts_with("$SRV.")
                        && !grant.contains(".image."),
                    "{principal:?} retains open authority {grant}"
                );
            }
        }
    }
}

#[test]
fn inventory_scopes_and_fixed_scopes_match_their_intended_families() {
    for endpoint in MachineServiceEndpoint::ALL.iter().copied() {
        assert!(nats_subject_matches(
            &machine_service_scope(endpoint),
            &machine_service(&machine_id("machine-a"), endpoint)
        ));
    }
    for endpoint in BuildExecutorServiceEndpoint::ALL.iter().copied() {
        assert!(nats_subject_matches(
            &build_executor_service_scope(endpoint),
            &build_executor_service(
                &BuildPoolId::try_new("pool-a").expect("pool"),
                &BuildExecutorId::try_new("executor-a").expect("executor"),
                endpoint,
            )
        ));
    }
    assert_eq!(
        build_executor_log_publish_scope(
            &BuildPoolId::try_new("pool-a").expect("pool"),
            &BuildExecutorId::try_new("executor-a").expect("executor")
        ),
        "plz.v1.signal.build_executor.pool-a.executor-a.build.operation.*.log"
    );
    assert_eq!(
        inbox_subscribe_scope(&NatsPrincipal::Controller),
        "_INBOX_ctl.>"
    );
}

#[test]
fn machine_build_status_is_a_narrow_query_permission() {
    let machine = machine_id("machine-a");
    let subject = machine_service(&machine, MachineServiceEndpoint::BuildStatus);

    assert_eq!(subject, "plz.v1.rpc.machine.query.machine-a.build.status");
    assert!(permits(
        NatsPrincipal::Controller,
        Direction::Publish,
        &subject
    ));
    assert!(permits(
        NatsPrincipal::Machine {
            machine_id: machine
        },
        Direction::Subscribe,
        &subject
    ));
    assert!(!permits(
        NatsPrincipal::Operator,
        Direction::Publish,
        &subject
    ));
}

fn nats_subject_matches(pattern: &str, subject: &str) -> bool {
    let mut pattern = pattern.split('.');
    let mut subject = subject.split('.');
    loop {
        match (pattern.next(), subject.next()) {
            (Some(">"), _) => return true,
            (Some("*"), Some(_)) => {}
            (Some(expected), Some(actual)) if expected == actual => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}
