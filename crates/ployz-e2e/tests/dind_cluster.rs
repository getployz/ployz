//! Gated Docker-in-Docker harness tests.
//!
//! Run with: `PLOYZ_DIND_E2E=1 cargo test -p ployz-e2e --test dind_cluster
//! -- --test-threads=1`. Requires the machine image from
//! `scripts/build-dind-machine-image.sh` and Docker with `--privileged`
//! support. `PLOYZ_DIND_KEEP=1` keeps the cluster running for debugging;
//! `scripts/dind-clean.sh` sweeps leftovers.
//!
//! Scenarios form the core through the product quick-start command
//! `ployz machine init root@core` (driven over a docker-exec-backed
//! stand-in ssh), and scenarios 2–5 join the edge through the product
//! `ployz machine add root@edge`.
//!
//! Every scenario body runs inside [`support::dind::with_evidence`], so any
//! failed assertion captures whole-cluster evidence before the panic
//! propagates.

mod support;

use futures_util::StreamExt;
use ployz::compose::{ComposeInput, UnsupportedFieldMode, parse_deploy_file};
use ployz::image_push::{prepare_deploy_images, push_local_image};
use ployz_core::dataplane::{DEFAULT_WIREGUARD_LISTEN_PORT, DataplaneMember};
use ployz_core::deploy::{
    ContainerCommand, ContainerHealthcheck, ContainerHealthcheckTest, ContainerMountPath,
    ContainerResourceLimits, ContainerRestartPolicy, ContainerRuntimeSpec, DeployRequest,
    DeployRoute, DeployRouteTarget, DeployServiceSpec, EnvName, EnvValue, HealthcheckDurationNanos,
    HealthcheckRetries, HealthcheckShellCommand, ImageReference, LinuxCapability, MemoryBytes,
    NanoCpus, PidsLimit, PreStartHook, ReplicaCount, ServiceEnvironment, ServiceVolumeMount,
    VolumeName,
};
use ployz_core::ids::MachineId;
use ployz_core::machine::{
    ConnectivityProofUnreachablePeer, MachineAddFailure, MachineCredentialProvisioningStep,
};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    NamespaceRemoveOperationState, OperationEvent, OperationStatus, PreStartHookFailure,
    VolumeRemoveOperationState,
};
use ployz_core::ops::{MachineAddOperationState, ManagedLeaseOperationState};
use ployz_core::permissions::inbox_subscribe_scope;
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{MachineServiceEndpoint, OPERATOR_RUNTIME_SNAPSHOT, machine_service};
use ployz_e2e::bollard::body_full;
use ployz_e2e::bollard::query_parameters::{
    BuildImageOptionsBuilder, ListContainersOptionsBuilder, ListNetworksOptionsBuilder,
};
use ployz_e2e::dind::{
    self, DindCluster, DindClusterSpec, DindMachine, DindMachineRole, MACHINE_NATS_PORT,
    MachineSpec, exec_in_container, read_file_from_container,
};
use ployz_nats::connect::{NatsClientUrl, connect_with_timeout};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    DeploySubmitRequest, MachineJoinRedeemError, MachineJoinRedeemRequest, MachineListRequest,
    MachineSnapshot, MachineTestimony, NamespaceRemoveRequest, OpsListRequest, VolumeListRequest,
    VolumeRemoveRequest, VolumeStatus,
};
use ployz_test_support::ids::{
    idempotency_key, machine_id, namespace_id, operation_id, route_hostname, route_port, service_id,
};
use ployz_test_support::nats::SecuredTestNats;
use ployz_test_support::ops::wait_for_terminal_status;
use ployzd::adapters::docker::labels::{
    CONTAINER_TYPE_LABEL, NAMESPACE_ID_LABEL, NAMESPACE_REVISION_ENTRY_LABEL, OPERATION_ID_LABEL,
    SERVICE_ID_LABEL,
};
use ployzd::intent::service::NatsIntentReader;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::time::{Duration, Instant};
use support::dind::assert::{
    ManagedWorkloadContainer, all_managed_workload_containers, assert_deploy_event_sequence,
    assert_machine_add_event_sequence, assert_unit_active, connect_with_event_capture,
    decode_base64, gateway_http_get, gateway_https_get, managed_workload_containers,
    next_permission_violation, operation_events, operation_status, terminal_operation_events,
    wait_for_inspect, wait_for_machine_observations, wait_for_terminal_deploy_status,
};
use support::dind::formation::{
    CoreContext, add_and_join_edge, connect_core_client, finish, host_client_config,
    init_core_cluster, submit_machine_add,
};
use support::dind::join::{parse_install_line, run_edge_join, run_edge_join_outcome};
use support::dind::{AUTHORIZED_USERS_FILE, CONNECT_TIMEOUT, EDGE_NATS_CREDS_FILE, with_evidence};

/// The workload image `scripts/build-dind-machine-image.sh` bakes into the
/// machine image; the inner Docker daemons load it at boot.
const WORKLOAD_IMAGE: &str = "nginx:1.27-alpine";
/// Route hostname the smoke deploy registers on both gateways.
const ROUTE_HOSTNAME: &str = "smoke.local";
/// Port nginx listens on inside its workload container.
const WORKLOAD_ENDPOINT_PORT: u16 = 80;
/// Budget for the routed two-machine deploy to reach a terminal state
/// (includes the image pull check and WireGuard/eBPF preparation).
const DEPLOY_TERMINAL_BUDGET: Duration = Duration::from_secs(300);

/// Smoke test: one machine boots to systemd + inner-docker readiness with the
/// artifact mount in place, and teardown leaves nothing labeled behind.
#[tokio::test]
async fn boots_machine_image() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let spec = DindClusterSpec {
        artifact_dir: dind::artifact_dir(),
        machines: vec![MachineSpec {
            role: DindMachineRole::Core,
            image: dind::machine_image(),
        }],
    };
    let cluster = DindCluster::provision(&docker, spec)
        .await
        .expect("provision one-machine DinD cluster");

    // Provisioning already waited for readiness; assert it holds from the
    // outside through the same exec surface scenarios will use.
    with_evidence(&cluster, async {
        let system_state = exec_in_container(
            &docker,
            &cluster.core().container_id,
            &["systemctl", "is-system-running"],
        )
        .await;
        assert!(
            matches!(
                &system_state,
                Ok(outcome) if matches!(outcome.stdout.trim(), "running" | "degraded")
            ),
            "core systemd not ready: {system_state:?}"
        );

        let inner_docker =
            exec_in_container(&docker, &cluster.core().container_id, &["docker", "info"]).await;
        assert!(
            matches!(&inner_docker, Ok(outcome) if outcome.success()),
            "inner docker not ready: {inner_docker:?}"
        );

        let nested_workload = exec_in_container(
            &docker,
            &cluster.core().container_id,
            &["docker", "run", "--rm", WORKLOAD_IMAGE, "true"],
        )
        .await;
        assert!(
            matches!(&nested_workload, Ok(outcome) if outcome.success()),
            "inner docker cannot start a workload: {nested_workload:?}"
        );

        let artifacts = exec_in_container(
            &docker,
            &cluster.core().container_id,
            &["test", "-x", "/opt/ployz/artifacts/ployzd"],
        )
        .await;
        assert!(
            matches!(&artifacts, Ok(outcome) if outcome.success()),
            "artifact mount missing executable ployzd: {artifacts:?}"
        );
    })
    .await;

    if dind::keep_requested() {
        eprintln!(
            "PLOYZ_DIND_KEEP=1: keeping run {} (network {}, core container {})",
            cluster.run_id(),
            cluster.network_name(),
            cluster.core().container_id,
        );
        return;
    }

    let run_label = format!("{}={}", dind::RUN_LABEL, cluster.run_id());
    cluster.teardown().await.expect("teardown DinD cluster");

    let filters = HashMap::from([("label".to_owned(), vec![run_label])]);
    let leftover_containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::new()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await
        .expect("list containers after teardown");
    assert!(
        leftover_containers.is_empty(),
        "teardown left labeled containers behind: {leftover_containers:?}"
    );
    let leftover_networks = docker
        .list_networks(Some(
            ListNetworksOptionsBuilder::new().filters(&filters).build(),
        ))
        .await
        .expect("list networks after teardown");
    assert!(
        leftover_networks.is_empty(),
        "teardown left labeled networks behind: {leftover_networks:?}"
    );
}

/// Scenario 1 — machine init forms a TLS-authenticated core through product
/// commands only and activates the first machine.
#[tokio::test]
async fn scenario_init_and_activate_first_machine() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        let cluster = &core.cluster;
        let machine_unit = "ployzd-machine-core_1";
        let gateway_unit = "ployzd-gateway";
        for unit in ["nats-server", "ployzd-control", machine_unit, gateway_unit] {
            assert_unit_active(&core, cluster.core(), unit).await;
        }

        let machine_seed = read_file_from_container(
            &docker,
            &cluster.core().container_id,
            "/var/lib/ployz/nats/machine.seed",
        )
        .await
        .expect("machine.seed exists after activate");
        assert!(
            machine_seed.trim().starts_with("SU"),
            "machine.seed is an NKey user seed"
        );

        wait_for_machine_observations(&core, &machine_id("core_1")).await;

        let authorized =
            read_file_from_container(&docker, &cluster.core().container_id, AUTHORIZED_USERS_FILE)
                .await
                .expect("authorized-users.conf is readable");
        for principal in ["controller", "operator", "join", "machine_core_1"] {
            assert!(
                authorized.contains(&format!("# ployz-principal: {principal}")),
                "authorized-users.conf must contain {principal}: {authorized}"
            );
        }

        let deadline = Instant::now() + Duration::from_secs(60);
        let mut last_operations = Vec::new();
        while Instant::now() < deadline {
            last_operations = core
                .api
                .ops_list(&OpsListRequest { active_only: false })
                .await
                .expect("list operations")
                .operations;
            if last_operations.iter().any(|snapshot| {
                matches!(
                    &snapshot.status,
                    OperationStatus::ManagedLease {
                        state: ManagedLeaseOperationState::Completed,
                        ..
                    }
                )
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("managed lease acquisition did not complete: {last_operations:?}");
    })
    .await;

    finish(core).await;
}

#[tokio::test]
async fn scenario_auto_hostname_https_survives_core_stop() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        let first = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_auto_https_first"),
                target: auto_hostname_deploy_target(),
            })
            .await
            .expect("auto-hostname deploy submits");
        let status =
            wait_for_terminal_deploy_status(&core, &first.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(
            matches!(
                status,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed { .. },
                    ..
                }
            ),
            "auto-hostname deploy did not complete: {status:?}"
        );

        let client = connect_core_client(
            &core,
            NatsPrincipal::Controller,
            &core.material.controller_seed,
        )
        .await
        .expect("connect controller for intent read");
        let reader = NatsIntentReader::new(client);
        let intent = reader.intent().await.expect("read auto-hostname intent");
        let binding = intent
            .route_bindings
            .iter()
            .find(|binding| binding.namespace_id == namespace_id("auto_https"))
            .expect("auto hostname binding committed");
        let hostname = binding.target.hostname.as_str().to_owned();
        assert_eq!(
            hostname,
            "svc-auto.demo.up.ployz.app".replace(
                "demo",
                intent
                    .managed_lease
                    .as_ref()
                    .expect("managed lease")
                    .name
                    .as_str()
            )
        );
        let bundle = intent
            .managed_cert_bundle
            .expect("managed certificate bundle");

        let second = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_auto_https_second"),
                target: auto_hostname_deploy_target(),
            })
            .await
            .expect("auto-hostname redeploy submits");
        let second_status =
            wait_for_terminal_deploy_status(&core, &second.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(matches!(
            second_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed { .. },
                ..
            }
        ));
        let intent_after = reader.intent().await.expect("read redeployed intent");
        assert!(intent_after.route_bindings.iter().any(|binding| {
            binding.namespace_id == namespace_id("auto_https")
                && binding.target.hostname.as_str() == hostname
        }));

        let machine = core.cluster.core();
        let response = gateway_https_get(
            machine.published.gateway_tls,
            &hostname,
            &bundle.certificate_chain_pem,
        )
        .await
        .expect("managed HTTPS route answers");
        assert!(response.contains("Welcome to nginx"), "{response}");
        let redirect = gateway_http_get(machine.published.gateway, &hostname)
            .await
            .expect("managed HTTP route redirects");
        assert!(redirect.starts_with("HTTP/1.1 301"), "{redirect}");
        assert!(
            gateway_https_get(
                machine.published.gateway_tls,
                &format!("unrouted.{}", bundle.lease.hostname_suffix()),
                &bundle.certificate_chain_pem,
            )
            .await
            .is_err(),
            "unknown SNI must be refused"
        );

        let stopped = core
            .exec_on(machine, &["systemctl", "stop", "ployzd-control"])
            .await;
        assert!(stopped.success(), "stop core control: {stopped:?}");
        let response = gateway_https_get(
            machine.published.gateway_tls,
            &hostname,
            &bundle.certificate_chain_pem,
        )
        .await
        .expect("last-known-good HTTPS survives core stop");
        assert!(response.contains("Welcome to nginx"), "{response}");
        let started = core
            .exec_on(machine, &["systemctl", "start", "ployzd-control"])
            .await;
        assert!(started.success(), "restart core control: {started:?}");
    })
    .await;
    finish(core).await;
}

/// A failed deploy retains its stopped container for inspection until the
/// next explicit deploy to the namespace sweeps the prior attempt.
#[tokio::test]
async fn scenario_namespace_manifest_convergence_sweeps_failed_retry() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        wait_for_machine_observations(&core, &machine_id("core_1")).await;

        let failed = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_convergence_failed"),
                // The 0.4s outlives at least one 100ms health poll (so the
                // container is sampled running) yet exits inside the 1.5s
                // running-confirmation window, exercising the fast-exit race.
                target: convergence_deploy_target("sleep 0.4; exit 42"),
            })
            .await
            .expect("failing deploy submits");
        let failed_operation = failed.operation_id;
        let failed_status =
            wait_for_terminal_deploy_status(&core, &failed_operation, DEPLOY_TERMINAL_BUDGET).await;
        let OperationStatus::Deploy {
            state:
                DeployOperationState::Failed {
                    failure: failed_failure,
                },
            ..
        } = failed_status
        else {
            panic!("deploy must fail with readable evidence");
        };
        assert_eq!(
            failed_failure
                .retained_artifacts()
                .iter()
                .filter(|artifact| artifact.is_container())
                .count(),
            1
        );

        let retained = namespace_managed_containers(&core, "converge").await;
        assert!(
            retained.iter().any(|container| {
                container.labels.get(OPERATION_ID_LABEL).map(String::as_str)
                    == Some(failed_operation.as_str())
            }),
            "failed attempt container must be retained before retry: {retained:?}"
        );

        let retry = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_convergence_retry"),
                target: convergence_deploy_target("sleep 600"),
            })
            .await
            .expect("retry deploy submits");
        let retry_operation = retry.operation_id;
        let retry_status =
            wait_for_terminal_deploy_status(&core, &retry_operation, DEPLOY_TERMINAL_BUDGET).await;
        assert!(
            matches!(
                retry_status,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                    ..
                }
            ),
            "retry deploy did not complete: {retry_status:?}"
        );

        let current = namespace_managed_containers(&core, "converge").await;
        assert!(
            current.iter().all(|container| {
                container.labels.get(OPERATION_ID_LABEL).map(String::as_str)
                    != Some(failed_operation.as_str())
            }),
            "retry must sweep failed attempt containers: {current:?}"
        );
        assert!(
            current.iter().any(|container| {
                container.labels.get(OPERATION_ID_LABEL).map(String::as_str)
                    == Some(retry_operation.as_str())
            }),
            "retry's current attempt container must survive: {current:?}"
        );

        let reread = operation_status(&core, &failed_operation).await;
        let OperationStatus::Deploy {
            state:
                DeployOperationState::Failed {
                    failure: reread_failure,
                },
            ..
        } = reread
        else {
            panic!("failed deploy status must remain readable after sweep");
        };
        assert_eq!(reread_failure, failed_failure);
        let events = terminal_operation_events(&core, &failed_operation).await;
        assert!(
            events.iter().any(|event| matches!(
                event,
                OperationEvent::DeployFailed {
                    operation_id,
                    failure,
                } if operation_id == &failed_operation && failure == &failed_failure
            )),
            "failed deploy event must stay readable after sweep: {events:?}"
        );
    })
    .await;

    finish(core).await;
}

#[tokio::test]
async fn scenario_pre_start_hook_runs_before_service_and_failure_retains_evidence() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        wait_for_machine_observations(&core, &machine_id("core_1")).await;

        let success = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_pre_start_success"),
                target: pre_start_deploy_target("pre_start_success", "echo ok > /data/marker"),
            })
            .await
            .expect("pre-start deploy submits");
        let success_status =
            wait_for_terminal_deploy_status(&core, &success.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(
            matches!(
                success_status,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                    ..
                }
            ),
            "pre-start deploy did not complete: {success_status:?}"
        );
        let running = managed_workload_containers(&core, core.cluster.core())
            .await
            .into_iter()
            .filter(|container| {
                container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str)
                    == Some("pre_start_success")
                    && container
                        .labels
                        .get(CONTAINER_TYPE_LABEL)
                        .map(String::as_str)
                        == Some("service")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            running.len(),
            1,
            "service container must be running: {running:?}"
        );

        let failed = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_pre_start_failure"),
                target: pre_start_deploy_target("pre_start_failure", "exit 7"),
            })
            .await
            .expect("failing pre-start deploy submits");
        let failed_status =
            wait_for_terminal_deploy_status(&core, &failed.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        let OperationStatus::Deploy {
            state: DeployOperationState::Failed { failure },
            ..
        } = failed_status
        else {
            panic!("pre-start deploy must fail with readable evidence");
        };
        assert!(
            matches!(
                failure,
                DeployOperationFailure::PreStartHookFailed {
                    failure: PreStartHookFailure::Exited { exit_code: 7, .. },
                    ..
                }
            ),
            "unexpected pre-start failure: {failure:?}"
        );
        let failed_containers = namespace_managed_containers(&core, "pre_start_failure").await;
        assert!(
            failed_containers.iter().all(|container| {
                container
                    .labels
                    .get(CONTAINER_TYPE_LABEL)
                    .map(String::as_str)
                    != Some("service")
            }),
            "hook gate must prevent service container creation: {failed_containers:?}"
        );
        assert!(
            failed_containers.iter().any(|container| {
                container
                    .labels
                    .get(CONTAINER_TYPE_LABEL)
                    .map(String::as_str)
                    == Some("predeploy")
            }),
            "failed hook container must remain as evidence: {failed_containers:?}"
        );
    })
    .await;
    finish(core).await;
}

#[tokio::test]
async fn scenario_services_start_in_depends_on_order() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        wait_for_machine_observations(&core, &machine_id("core_1")).await;
        let accepted = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_depends_on_order"),
                target: depends_on_deploy_target(),
            })
            .await
            .expect("dependency deploy submits");
        let status =
            wait_for_terminal_deploy_status(&core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(
            matches!(
                status,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                    ..
                }
            ),
            "dependency deploy did not complete: {status:?}"
        );
        let running = managed_workload_containers(&core, core.cluster.core())
            .await
            .into_iter()
            .filter(|container| {
                container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str)
                    == Some("depends_on_order")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            running.len(),
            2,
            "both dependency services must run: {running:?}"
        );
        let started_at = running
            .iter()
            .map(|container| {
                (
                    container
                        .labels
                        .get(SERVICE_ID_LABEL)
                        .expect("managed service container has service id")
                        .as_str(),
                    container.started_at.as_str(),
                )
            })
            .collect::<HashMap<_, _>>();
        let Some(a_started_at) = started_at.get("a") else {
            panic!("dependency service is missing: {started_at:?}");
        };
        let Some(b_started_at) = started_at.get("b") else {
            panic!("dependent service is missing: {started_at:?}");
        };
        assert!(
            a_started_at < b_started_at,
            "dependency must start first: {started_at:?}"
        );

        let events = terminal_operation_events(&core, &accepted.operation_id).await;
        let Some(plan) = events.iter().find_map(|event| {
            if let OperationEvent::DeployPlanCreated { plan, .. } = event {
                Some(plan)
            } else {
                None
            }
        }) else {
            panic!("dependency deploy must record its plan: {events:?}");
        };
        assert_eq!(
            plan.services
                .iter()
                .map(|service| service.service_id.clone())
                .collect::<Vec<_>>(),
            vec![service_id("a"), service_id("b")]
        );
    })
    .await;
    finish(core).await;
}

/// Scenario 2 — machine add returns its operation id before the mint's
/// reload lands, and the printed join bundle material drives the real
/// `scripts/ployz.sh` join flow on an edge machine over direct TLS NATS.
#[tokio::test]
async fn scenario_machine_add_via_join_bundle() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    // Product machine init forms and activates the core; this scenario owns
    // the machine-add/join details through the explicit low-level path.
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let cluster = &core.cluster;

        let add_operation = operation_id("op_add_edge_2");
        let accepted = submit_machine_add(&core).await;
        assert_eq!(accepted.accepted.operation_id, add_operation);

        // The submit response carried the operation id before the mint's
        // reload landed: the immediately-replayed event page has no `reloaded`
        // event yet (minting is bounded operation work after acceptance).
        let early_events = operation_events(&core, &add_operation).await;
        assert!(
            !early_events.iter().any(|event| matches!(
                event,
                OperationEvent::MachineAddCredentialProvisioned {
                    step: MachineCredentialProvisioningStep::Reloaded,
                    ..
                }
            )),
            "reload must land after acceptance, not inside the submit handler: {early_events:?}"
        );

        // The install line is the product's own render of the join material;
        // the edge joins with exactly what it prints.
        let install = parse_install_line(&core, accepted.clone());
        assert_eq!(
            install.nats_url,
            format!("tls://{}:{MACHINE_NATS_PORT}", core.core_ip()),
            "join bundle must point at the core's direct TLS NATS endpoint"
        );
        let printed_ca = decode_base64(&install.nats_ca_b64);
        assert_eq!(
            printed_ca.trim(),
            core.material.ca_pem.trim(),
            "install line must carry the cluster CA"
        );

        let [edge] = cluster.edges() else {
            panic!("scenario requires exactly one edge machine");
        };
        run_edge_join(&core, edge, &install).await;

        // Join operation completed with the mint sequence ordered around
        // acceptance, and the machine is active.
        let status = operation_status(&core, &add_operation).await;
        let OperationStatus::MachineAdd { state, .. } = status else {
            panic!("machine add is not a machine add: {status:?}");
        };
        assert_eq!(
            state,
            MachineAddOperationState::Completed,
            "machine add not completed"
        );
        let events = terminal_operation_events(&core, &add_operation).await;
        assert_machine_add_event_sequence(&events, &machine_id("edge_2"));

        let mangle_forward = core.exec_sh(edge, "iptables -t mangle -S FORWARD").await;
        assert!(
            mangle_forward.success(),
            "read edge mangle/FORWARD rules failed: {mangle_forward:?}"
        );
        for direction in ["-i", "-o"] {
            assert!(
                mangle_forward.stdout.lines().any(|rule| {
                    rule.contains(&format!("{direction} ployz-wg0"))
                        && rule.contains("TCPMSS")
                        && rule.contains("--clamp-mss-to-pmtu")
                }),
                "edge overlay is missing the {direction} TCPMSS clamp rule: {}",
                mangle_forward.stdout
            );
        }

        // nats_connection readiness evidence: the edge's machine process connects
        // with its minted credential and publishes observations.
        wait_for_machine_observations(&core, &machine_id("edge_2")).await;

        // The edge holds its own minted seed — not the controller's.
        let edge_creds =
            read_file_from_container(&docker, &edge.container_id, EDGE_NATS_CREDS_FILE)
                .await
                .expect("edge nats.creds exists after join");
        assert!(
            edge_creds.trim().starts_with("SU"),
            "edge nats.creds is an NKey user seed"
        );
        assert_ne!(
            edge_creds.trim(),
            core.material.controller_seed.trim(),
            "edge credential must differ from the controller seed"
        );

        // Never-shrink: the edge key is appended alongside every prior user.
        let authorized =
            read_file_from_container(&docker, &cluster.core().container_id, AUTHORIZED_USERS_FILE)
                .await
                .expect("authorized-users.conf is readable");
        for principal in [
            "controller",
            "operator",
            "join",
            "machine_core_1",
            "machine_edge_2",
        ] {
            assert!(
                authorized.contains(&format!("# ployz-principal: {principal}")),
                "authorized-users.conf must keep {principal}: {authorized}"
            );
        }

        // No separate gateway credential exists: the edge gateway role env
        // points its seed file at the machine's Machine creds.
        let gateway_env =
            read_file_from_container(&docker, &edge.container_id, "/etc/ployz/ployzd-gateway.env")
                .await
                .expect("edge gateway env file exists");
        assert!(
            gateway_env.contains(&format!("PLOYZ_NATS_NKEY_SEED_FILE={EDGE_NATS_CREDS_FILE}")),
            "edge gateway must authenticate with the Machine creds: {gateway_env}"
        );

        // The join token is single-use: a completed machine-add scrubs its
        // token material, so re-redeeming is refused as an unknown token —
        // a typed refusal, not a fresh secret.
        let join_client = connect_core_client(&core, NatsPrincipal::Join, &core.material.join_seed)
            .await
            .expect("join principal connects");
        let redeem_again = OperationApiClient::new(join_client)
            .machine_join_redeem(&MachineJoinRedeemRequest {
                join_token: accepted.join_token.clone(),
            })
            .await;
        match redeem_again {
            Err(OperationApiClientError::Domain {
                error: MachineJoinRedeemError::UnknownJoinToken,
                ..
            }) => {}
            other => {
                panic!("token re-redeem must be refused as an unknown token: {other:?}");
            }
        }
    })
    .await;

    finish(core).await;
}

/// An edge whose WireGuard UDP cannot reach the core keeps direct TLS NATS
/// control-plane access but fails admission with typed overlay evidence.
#[tokio::test]
async fn scenario_machine_add_rejects_unreachable_overlay_peer() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let accepted = submit_machine_add(&core).await;
        let operation_id = accepted.accepted.operation_id.clone();
        let install = parse_install_line(&core, accepted);
        let [edge] = core.cluster.edges() else {
            panic!("scenario requires exactly one edge machine");
        };
        let drop_wireguard = core
            .exec_sh(
                edge,
                &format!(
                    "iptables -I OUTPUT 1 -p udp -d {} --dport {DEFAULT_WIREGUARD_LISTEN_PORT} -j DROP",
                    core.core_ip()
                ),
            )
            .await;
        assert!(
            drop_wireguard.success(),
            "install edge WireGuard DROP rule failed: {drop_wireguard:?}"
        );

        let join = run_edge_join_outcome(&core, edge, &install).await;
        assert!(
            !join.success(),
            "unreachable edge join unexpectedly succeeded: {join:?}"
        );

        // The proof waits the full handshake budget before the join reports its
        // failure, so poll the operation until it records the terminal
        // connectivity-proof failure rather than reading once.
        let deadline = Instant::now() + Duration::from_secs(60);
        let evidence = loop {
            let status = operation_status(&core, &operation_id).await;
            let OperationStatus::MachineAdd { state, .. } = status else {
                panic!("machine add is not a machine add: {status:?}");
            };
            if let MachineAddOperationState::Failed {
                failure: MachineAddFailure::ConnectivityProofFailed { evidence },
            } = &state
            {
                break evidence.clone();
            }
            assert!(
                Instant::now() < deadline,
                "unreachable machine add did not fail connectivity proof: {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let core_member = DataplaneMember::default_for_machine(machine_id("core_1"));
        assert_eq!(
            evidence.unreachable_peers(),
            [ConnectivityProofUnreachablePeer {
                machine_id: machine_id("core_1"),
                gateway: core_member.endpoint_subnet.bridge_gateway_ipv4(),
            }]
        );
    })
    .await;

    finish(core).await;
}

/// Scenarios 3–5 on one formed two-machine cluster (one cluster fits the
/// host at a time, scenario 4 needs scenario 3's serving deploy, and
/// scenario 5 needs the same healthy cluster as its blast-radius control):
///
/// - **Scenario 3 — cross-machine deploy:** the baked nginx image deploys
///   with a replica on each machine and a route, driven through the
///   host-side API client; the operation events, the inner Docker reality
///   on both machines, and HTTP through both published gateway ports all
///   agree.
/// - **Scenario 4 — daemon-restart invisibility:** restarting
///   `ployzd-control` (core) and the edge machine unit neither interrupts
///   gateway HTTP nor replaces workload containers, and the operations API
///   answers afterwards with unmutated machine state.
/// - **Scenario 5 — auth rejection:** unauthorized seeds, plaintext
///   clients, and over-reaching Machine/Join principals are refused by the
///   real cluster — which keeps serving afterwards.
#[tokio::test]
async fn scenario_deploy_restart_invisibility_and_auth_rejection() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    // The product quick-start path: machine init forms and activates the
    // core, machine add joins the edge.
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let cluster = &core.cluster;
        let [edge] = cluster.edges() else {
            panic!("scenario requires exactly one edge machine");
        };
        add_and_join_edge(&core, edge).await;
        wait_for_machine_observations(&core, &machine_id("core_1")).await;
        wait_for_machine_observations(&core, &machine_id("edge_2")).await;

        scenario_cross_machine_deploy(&core, edge).await;
        scenario_daemon_restart_invisibility(&core, edge).await;
        scenario_auth_rejection(&core, edge).await;
        scenario_runtime_fields_deploy(&core).await;
        scenario_named_volume_survives_redeploy(&core).await;
    })
    .await;

    finish(core).await;
}

/// A host-only image crosses the operator NATS connection once, then every
/// selected machine pulls its pinned manifest from the seed over the mesh.
#[tokio::test]
async fn scenario_direct_push_multi_machine_deploy() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 2).await;
    with_evidence(&core.cluster, async {
        for edge in core.cluster.edges() {
            add_and_join_edge(&core, edge).await;
        }
        for machine in [
            machine_id("core_1"),
            machine_id("edge_2"),
            machine_id("edge_3"),
        ] {
            wait_for_machine_observations(&core, &machine).await;
        }

        let tag = format!("ployz-e2e-push:{}", core.cluster.run_id().as_str());
        build_derived_image(
            &docker,
            &tag,
            WORKLOAD_IMAGE,
            "ployz-marker",
            core.cluster.run_id().as_str(),
        )
        .await;
        for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
            let absent = core
                .exec_on(machine, &["docker", "image", "inspect", &tag])
                .await;
            assert!(
                !absent.success(),
                "operator-only image unexpectedly exists on {}",
                machine.name
            );
        }

        let namespace = namespace_id("direct_push");
        let mut services = vec![DeployServiceSpec {
            service_id: service_id("svc_direct_push"),
            image: ImageReference::try_new(&tag).expect("valid pushed image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(2).expect("valid replica count"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }];
        let receipts = prepare_deploy_images(&core.api, &mut services, false)
            .await
            .expect("local image pushes before deploy submit");
        let [receipt] = receipts.as_slice() else {
            panic!("one local image must produce one push receipt: {receipts:?}");
        };
        assert_eq!(receipt.seed, machine_id("core_1"));

        let accepted = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_direct_push"),
                target: DeployRequest {
                    namespace_id: namespace,
                    services,
                },
            })
            .await
            .expect("direct-push deploy submits");
        let status =
            wait_for_terminal_deploy_status(&core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(
            matches!(
                &status,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                    ..
                }
            ),
            "direct-push deploy did not complete: {status:?}"
        );
        let events = terminal_operation_events(&core, &accepted.operation_id).await;
        assert!(events.iter().any(|event| matches!(
            event,
            OperationEvent::DeployImageAvailabilityVerified {
                seed,
                manifest_digest,
                ..
            } if seed == &receipt.seed && manifest_digest == &receipt.manifest_digest
        )));

        let mut running = 0_usize;
        let mut running_machines = BTreeSet::new();
        for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
            for container in managed_workload_containers(&core, machine)
                .await
                .into_iter()
                .filter(|container| {
                    container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str)
                        == Some("direct_push")
                })
            {
                let marker = core
                    .exec_on(
                        machine,
                        &[
                            "docker",
                            "exec",
                            &container.id,
                            "cat",
                            "/usr/share/nginx/html/ployz-marker",
                        ],
                    )
                    .await;
                assert!(
                    marker.success() && marker.stdout.trim() == core.cluster.run_id().as_str(),
                    "pushed marker differs on {}: {marker:?}",
                    machine.name
                );
                running += 1;
                running_machines.insert(machine.name.clone());
            }
        }
        assert_eq!(running, 2, "two pushed-image replicas must be running");
        assert_eq!(
            running_machines.len(),
            2,
            "pushed-image replicas must run on two distinct machines: {running_machines:?}"
        );
        assert!(
            running_machines
                .iter()
                .any(|machine| machine != receipt.seed.as_str()),
            "at least one pushed-image replica must run away from seed {}: {running_machines:?}",
            receipt.seed.as_str()
        );
    })
    .await;
    finish(core).await;
}

/// A second image derived from the first reuses every parent layer and sends
/// only its newly-added layer to the seed.
#[tokio::test]
async fn scenario_repush_transfers_only_new_layers() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        wait_for_machine_observations(&core, &machine_id("core_1")).await;
        let suffix = core.cluster.run_id().as_str();
        let tag_a = format!("ployz-e2e-repush-a:{suffix}");
        let tag_b = format!("ployz-e2e-repush-b:{suffix}");
        build_derived_image(&docker, &tag_a, WORKLOAD_IMAGE, "layer-a", suffix).await;
        build_derived_image(&docker, &tag_b, &tag_a, "layer-b", suffix).await;
        let first = push_local_image(
            &core.api.nats_client(),
            &docker,
            &machine_id("core_1"),
            ImageReference::try_new(&tag_a).expect("valid first image reference"),
        )
        .await
        .expect("first image pushes");
        let second = push_local_image(
            &core.api.nats_client(),
            &docker,
            &machine_id("core_1"),
            ImageReference::try_new(&tag_b).expect("valid second image reference"),
        )
        .await
        .expect("derived image pushes");

        let first_layers = first
            .uploaded
            .digests
            .iter()
            .chain(first.reused.digests.iter())
            .collect::<std::collections::BTreeSet<_>>();
        let reused = second
            .reused
            .digests
            .iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert!(
            first_layers.is_subset(&reused),
            "derived image must reuse every parent layer: first={first:?} second={second:?}"
        );
        assert_eq!(
            second.uploaded.count(),
            1,
            "derived image must upload exactly its new layer: {second:?}"
        );
    })
    .await;
    finish(core).await;
}

/// A workload resolves a sibling service by its plain service id when the
/// target container runs on the other machine.
#[tokio::test]
async fn scenario_internal_service_dns_reaches_cross_machine_sibling() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 1).await;
    with_evidence(&core.cluster, async {
        let [edge] = core.cluster.edges() else {
            panic!("scenario requires exactly one edge machine");
        };
        add_and_join_edge(&core, edge).await;
        wait_for_machine_observations(&core, &machine_id("core_1")).await;
        wait_for_machine_observations(&core, &machine_id("edge_2")).await;
        for machine in [core.cluster.core(), edge] {
            assert_unit_active(&core, machine, "ployzd-dns").await;
        }

        let accepted = core
            .api
            .deploy_submit(&DeploySubmitRequest {
                idempotency_key: idempotency_key("idem_dind_internal_dns"),
                target: internal_dns_deploy_target(),
            })
            .await
            .expect("internal DNS deploy submits");
        let status =
            wait_for_terminal_deploy_status(&core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET)
                .await;
        assert!(
            matches!(
                &status,
                OperationStatus::Deploy {
                    state: DeployOperationState::Completed {
                        outcome: DeployCompletionOutcome::Completed,
                    },
                    ..
                }
            ),
            "internal DNS deploy did not complete: {status:?}"
        );

        let mut servers = Vec::new();
        let mut clients = Vec::new();
        for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
            for container in managed_workload_containers(&core, machine).await {
                match container.labels.get(SERVICE_ID_LABEL).map(String::as_str) {
                    Some("server") => servers.push((machine.clone(), container)),
                    Some("client") => clients.push((machine.clone(), container)),
                    Some(_) | None => {}
                }
            }
        }
        let [(server_machine, server)] = servers.as_slice() else {
            panic!("expected one server container, got {servers:?}");
        };
        let Some((client_machine, client)) = clients
            .iter()
            .find(|(machine, _)| machine.name != server_machine.name)
        else {
            panic!("expected a client on the machine opposite {server_machine:?}: {clients:?}");
        };
        let Some(server_ip) = server.endpoint_ip else {
            panic!("server container has no endpoint IP: {server:?}");
        };
        let Some(client_ip) = client.endpoint_ip else {
            panic!("client container has no endpoint IP: {client:?}");
        };

        let deadline = Instant::now() + OVERLAY_FIRST_CONTACT_BUDGET;
        let last = loop {
            let outcome = exec_in_container(
                &docker,
                &client_machine.container_id,
                &[
                    "docker",
                    "exec",
                    &client.id,
                    "sh",
                    "-c",
                    "wget -T 2 -qO- http://server/ | grep -q 'Welcome to nginx'",
                ],
            )
            .await;
            if matches!(&outcome, Ok(outcome) if outcome.success()) {
                break outcome;
            }
            if Instant::now() >= deadline {
                break outcome;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        assert!(
            matches!(&last, Ok(outcome) if outcome.success()),
            "plain service DNS failed from {}:{client_ip} to {}:{server_ip}: {last:?}",
            client_machine.name,
            server_machine.name,
        );

        let stopped_control = exec_in_container(
            &docker,
            &core.cluster.core().container_id,
            &["systemctl", "stop", "ployzd-control"],
        )
        .await;
        assert!(
            matches!(&stopped_control, Ok(outcome) if outcome.success()),
            "stopping core control plane failed: {stopped_control:?}"
        );

        let deadline = Instant::now() + OVERLAY_FIRST_CONTACT_BUDGET;
        let last = loop {
            let outcome = exec_in_container(
                &docker,
                &client_machine.container_id,
                &[
                    "docker",
                    "exec",
                    &client.id,
                    "sh",
                    "-c",
                    "wget -T 2 -qO- http://server/ | grep -q 'Welcome to nginx'",
                ],
            )
            .await;
            if matches!(&outcome, Ok(outcome) if outcome.success()) {
                break outcome;
            }
            if Instant::now() >= deadline {
                break outcome;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        assert!(
            matches!(&last, Ok(outcome) if outcome.success()),
            "plain service DNS lost last-known-good facts with core control stopped: {last:?}"
        );

        let stopped_server = exec_in_container(
            &docker,
            &server_machine.container_id,
            &["docker", "stop", &server.id],
        )
        .await;
        assert!(
            matches!(&stopped_server, Ok(outcome) if outcome.success()),
            "stopping server workload failed: {stopped_server:?}"
        );

        let deadline = Instant::now() + OVERLAY_FIRST_CONTACT_BUDGET;
        let last = loop {
            let outcome = exec_in_container(
                &docker,
                &client_machine.container_id,
                &[
                    "docker",
                    "exec",
                    &client.id,
                    "sh",
                    "-c",
                    "wget -T 2 -qO- http://server/ | grep -q 'Welcome to nginx'",
                ],
            )
            .await;
            // A connect failure to a still-resolving address is not enough:
            // the record must disappear, which musl reports as "bad address".
            if matches!(&outcome, Ok(outcome) if outcome.stderr.contains("bad address")) {
                break outcome;
            }
            if Instant::now() >= deadline {
                break outcome;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        assert!(
            matches!(&last, Ok(outcome) if outcome.stderr.contains("bad address")),
            "plain service DNS kept resolving the stopped workload before deadline: {last:?}"
        );
    })
    .await;

    finish(core).await;
}

// ---------------------------------------------------------------------------
// Scenario 3 — cross-machine deploy
// ---------------------------------------------------------------------------

/// Deploys the baked workload image with one replica per machine and a
/// route, through the host-side operator API client, and asserts the
/// committed deploy event vocabulary, Docker reality, and HTTP service.
async fn scenario_cross_machine_deploy(core: &CoreContext, edge: &DindMachine) {
    let cluster = &core.cluster;
    let accepted = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_deploy"),
            target: smoke_deploy_target(),
        })
        .await
        .expect("deploy submits");
    let deploy_operation = accepted.operation_id;

    let status =
        wait_for_terminal_deploy_status(core, &deploy_operation, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            &status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "deploy did not complete: {status:?}"
    );
    let events = terminal_operation_events(core, &deploy_operation).await;
    assert_deploy_event_sequence(&events, &deploy_operation);

    // Docker is execution reality: each machine runs exactly one managed
    // workload container carrying the product's exact label values, and the
    // two workloads reach each other over the overlay before and after the
    // control daemon restarts.
    let core_container =
        assert_smoke_workload_container(core, cluster.core(), &deploy_operation).await;
    let edge_container = assert_smoke_workload_container(core, edge, &deploy_operation).await;
    assert_workloads_reach_each_other_by_overlay_ip(
        core,
        cluster.core(),
        &core_container,
        edge,
        &edge_container,
        "before control shutdown",
    )
    .await;

    let stop_control = core
        .exec_on(cluster.core(), &["systemctl", "stop", "ployzd-control"])
        .await;
    assert!(
        stop_control.success(),
        "stopping control daemon failed: {stop_control:?}"
    );
    assert_workloads_reach_each_other_by_overlay_ip(
        core,
        cluster.core(),
        &core_container,
        edge,
        &edge_container,
        "after control shutdown",
    )
    .await;
    let start_control = core
        .exec_on(cluster.core(), &["systemctl", "start", "ployzd-control"])
        .await;
    assert!(
        start_control.success(),
        "starting control daemon failed after overlay assertion: {start_control:?}"
    );
    assert_unit_active(core, cluster.core(), "ployzd-control").await;

    // The route serves through BOTH gateways via the published ports.
    for machine in [cluster.core(), edge] {
        wait_for_gateway_serving(machine).await;
    }
}

async fn assert_smoke_workload_container(
    core: &CoreContext,
    machine: &DindMachine,
    deploy_operation: &ployz_core::ids::OperationId,
) -> ManagedWorkloadContainer {
    // The revision entry is the content-derived id of the deployed service spec.
    let smoke_target = smoke_deploy_target();
    let [smoke_service] = smoke_target.services.as_slice() else {
        panic!("smoke deploy target carries one service");
    };
    let revision_entry = smoke_service
        .namespace_revision_entry_id(&smoke_target.namespace_id)
        .as_str()
        .to_owned();
    let containers = managed_workload_containers(core, machine).await;
    let [container] = containers.as_slice() else {
        panic!(
            "expected one managed container on {}, got {containers:?}",
            machine.name
        );
    };
    for (key, value) in [
        (SERVICE_ID_LABEL, "svc_smoke".to_owned()),
        (NAMESPACE_REVISION_ENTRY_LABEL, revision_entry.clone()),
        (OPERATION_ID_LABEL, deploy_operation.as_str().to_owned()),
        (CONTAINER_TYPE_LABEL, "service".to_owned()),
    ] {
        assert_eq!(
            container.labels.get(key),
            Some(&value),
            "managed container on {} must carry {key}={value}: {container:?}",
            machine.name
        );
    }
    container.clone()
}

async fn assert_workloads_reach_each_other_by_overlay_ip(
    core: &CoreContext,
    core_machine: &DindMachine,
    core_container: &ManagedWorkloadContainer,
    edge: &DindMachine,
    edge_container: &ManagedWorkloadContainer,
    phase: &str,
) {
    assert_overlay_http(core, core_machine, core_container, edge_container, phase).await;
    assert_overlay_http(core, edge, edge_container, core_container, phase).await;
}

/// Budget for one workload to first reach the other over the overlay: the
/// WireGuard tunnel and routes converge shortly after the deploy completes,
/// so first contact is eventually consistent. Repeat contact (after the
/// control daemon stops) reuses the same helper and settles immediately.
const OVERLAY_FIRST_CONTACT_BUDGET: Duration = Duration::from_secs(60);

async fn assert_overlay_http(
    core: &CoreContext,
    source_machine: &DindMachine,
    source: &ManagedWorkloadContainer,
    target: &ManagedWorkloadContainer,
    phase: &str,
) {
    let Some(source_ip) = source.endpoint_ip else {
        panic!(
            "running workload {} on {} has no endpoint IP",
            source.id, source_machine.name
        );
    };
    let Some(target_ip) = target.endpoint_ip else {
        panic!("overlay target workload {} has no endpoint IP", target.id);
    };
    let script = format!("wget -T 2 -qO- http://{target_ip}/ | grep -q 'Welcome to nginx'");
    let deadline = Instant::now() + OVERLAY_FIRST_CONTACT_BUDGET;
    let last = loop {
        let outcome = core
            .exec_on(
                source_machine,
                &["docker", "exec", &source.id, "sh", "-c", &script],
            )
            .await;
        if outcome.success() {
            return;
        }
        if Instant::now() >= deadline {
            break outcome;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let diagnostics = core
        .exec_sh(
            source_machine,
            "wg show; ip -br addr; ip route; iptables -S FORWARD | head -30",
        )
        .await;
    panic!(
        "workload overlay HTTP failed {phase} from {}:{source_ip} to {target_ip}: {last:?}\n\
         dataplane on {}: {diagnostics:?}",
        source_machine.name, source_machine.name
    );
}

/// Deploys command and environment through the real machine Docker path and
/// checks the created container's Docker config.
async fn scenario_runtime_fields_deploy(core: &CoreContext) {
    let accepted = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_runtime_fields"),
            target: runtime_fields_deploy_target(),
        })
        .await
        .expect("runtime fields deploy submits");
    let deploy_operation = accepted.operation_id;

    let status =
        wait_for_terminal_deploy_status(core, &deploy_operation, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            &status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "runtime fields deploy did not complete: {status:?}"
    );

    let mut runtime_containers = Vec::new();
    for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
        runtime_containers.extend(
            managed_workload_containers(core, machine)
                .await
                .into_iter()
                .filter(|container| {
                    container.labels.get(SERVICE_ID_LABEL).map(String::as_str)
                        == Some("svc_runtime")
                }),
        );
    }
    let [container] = runtime_containers.as_slice() else {
        panic!("expected one runtime container, got {runtime_containers:?}");
    };
    assert!(
        container
            .env
            .iter()
            .any(|value| value == "PLOYZ_E2E_RUNTIME=present"),
        "runtime container env missing PLOYZ_E2E_RUNTIME=present: {container:?}"
    );
    assert_eq!(
        container.cmd,
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "env; sleep 600".to_owned()
        ]
    );
    assert_eq!(
        container.healthcheck.get("Test"),
        Some(&serde_json::json!([
            "CMD-SHELL",
            "test \"$PLOYZ_E2E_RUNTIME\" = present"
        ]))
    );
    assert_eq!(
        container.restart_policy.get("Name"),
        Some(&serde_json::json!("unless-stopped"))
    );
    assert_eq!(container.cap_add, vec!["NET_ADMIN".to_owned()]);
    assert_eq!(container.cap_drop, vec!["MKNOD".to_owned()]);
    assert_eq!(container.nano_cpus, 500_000_000);
    assert_eq!(container.memory, 64_000_000);
    assert_eq!(container.pids_limit, 64);

    scenario_failing_healthcheck_deploy(core).await;
}

async fn scenario_failing_healthcheck_deploy(core: &CoreContext) {
    let accepted = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_failing_healthcheck"),
            target: failing_healthcheck_deploy_target(),
        })
        .await
        .expect("failing healthcheck deploy submits");
    let deploy_operation = accepted.operation_id;

    let status =
        wait_for_terminal_deploy_status(core, &deploy_operation, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            &status,
            OperationStatus::Deploy {
                state: DeployOperationState::Failed { .. },
                ..
            }
        ),
        "failing healthcheck deploy did not fail: {status:?}"
    );

    let filter = format!("label={SERVICE_ID_LABEL}=svc_bad_health");
    let mut retained = Vec::new();
    for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
        let listed = core
            .exec_on(
                machine,
                &[
                    "docker",
                    "ps",
                    "--all",
                    "--quiet",
                    "--no-trunc",
                    "--filter",
                    &filter,
                ],
            )
            .await;
        assert!(
            listed.success(),
            "inner docker ps -a on {} failed: {listed:?}",
            machine.name
        );
        retained.extend(
            listed
                .stdout
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
        );
    }
    assert!(
        !retained.is_empty(),
        "failing healthcheck deploy must retain its failed container"
    );
}

/// Exercises the complete explicit named-volume lifecycle.
async fn scenario_named_volume_survives_redeploy(core: &CoreContext) {
    let first = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_volume_first"),
            target: volume_deploy_target(
                "printf first > /data/marker; while true; do sleep 600; done",
            ),
        })
        .await
        .expect("first volume deploy submits");
    let first_status =
        wait_for_terminal_deploy_status(core, &first.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            &first_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "first volume deploy did not complete: {first_status:?}"
    );

    let second = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_volume_second"),
            target: volume_deploy_target(
                "cat /data/marker > /tmp/restored-marker || true; while true; do sleep 600; done",
            ),
        })
        .await
        .expect("second volume deploy submits");
    let second_status =
        wait_for_terminal_deploy_status(core, &second.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            &second_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
                ..
            }
        ),
        "second volume deploy did not complete: {second_status:?}"
    );

    let mut volume_containers = Vec::new();
    for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
        volume_containers.extend(
            managed_workload_containers(core, machine)
                .await
                .into_iter()
                .filter(|container| {
                    container.labels.get(SERVICE_ID_LABEL).map(String::as_str) == Some("svc_volume")
                })
                .map(|container| (machine.clone(), container)),
        );
    }
    let [(machine, container)] = volume_containers.as_slice() else {
        panic!("expected one volume container, got {volume_containers:?}");
    };
    let restored = core
        .exec_on(
            machine,
            &[
                "docker",
                "exec",
                &container.id,
                "cat",
                "/tmp/restored-marker",
            ],
        )
        .await;
    assert!(
        restored.success() && restored.stdout.trim() == "first",
        "redeployed container did not restore volume marker: {restored:?}"
    );

    let drop_mount = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_volume_drop_mount"),
            target: volume_deploy_target_without_mount(),
        })
        .await
        .expect("drop-volume deploy submits");
    let drop_mount_status =
        wait_for_terminal_deploy_status(core, &drop_mount.operation_id, DEPLOY_TERMINAL_BUDGET)
            .await;
    assert!(
        matches!(
            drop_mount_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed { .. },
                ..
            }
        ),
        "drop-volume deploy did not complete: {drop_mount_status:?}"
    );
    assert_volume_status(core, VolumeStatus::Orphaned).await;

    let docker_volume_name = "ployz-n6-volume-v4-data";
    let preserved = core
        .exec_on(
            machine,
            &[
                "docker",
                "run",
                "--rm",
                "-v",
                &format!("{docker_volume_name}:/data"),
                WORKLOAD_IMAGE,
                "cat",
                "/data/marker",
            ],
        )
        .await;
    assert!(
        preserved.success() && preserved.stdout.trim() == "first",
        "orphaned volume did not preserve data: {preserved:?}"
    );

    let reattach = core
        .api
        .deploy_submit(&DeploySubmitRequest {
            idempotency_key: idempotency_key("idem_dind_volume_reattach"),
            target: volume_deploy_target(
                "cat /data/marker > /tmp/reattached-marker; while true; do sleep 600; done",
            ),
        })
        .await
        .expect("volume reattach deploy submits");
    let reattach_status =
        wait_for_terminal_deploy_status(core, &reattach.operation_id, DEPLOY_TERMINAL_BUDGET).await;
    assert!(
        matches!(
            reattach_status,
            OperationStatus::Deploy {
                state: DeployOperationState::Completed { .. },
                ..
            }
        ),
        "volume reattach deploy did not complete: {reattach_status:?}"
    );
    assert_volume_status(core, VolumeStatus::InUse).await;

    let namespace_remove = core
        .api
        .namespace_remove(&NamespaceRemoveRequest {
            operation_id: operation_id("op_dind_volume_namespace_remove"),
            namespace_id: namespace_id("volume"),
        })
        .await
        .expect("namespace remove submits");
    let namespace_remove_status = wait_for_terminal_status(
        &core.api,
        &namespace_remove.operation_id,
        DEPLOY_TERMINAL_BUDGET,
    )
    .await;
    assert!(
        matches!(
            namespace_remove_status,
            OperationStatus::NamespaceRemove {
                state: NamespaceRemoveOperationState::Completed,
                ..
            }
        ),
        "namespace remove did not complete: {namespace_remove_status:?}"
    );
    assert_volume_status(core, VolumeStatus::Orphaned).await;

    let volume_remove = core
        .api
        .volume_remove(&VolumeRemoveRequest {
            operation_id: operation_id("op_dind_volume_remove"),
            namespace_id: namespace_id("volume"),
            volume_name: VolumeName::try_new("data").expect("valid volume name"),
        })
        .await
        .expect("volume remove submits");
    let volume_remove_status = wait_for_terminal_status(
        &core.api,
        &volume_remove.operation_id,
        DEPLOY_TERMINAL_BUDGET,
    )
    .await;
    assert!(
        matches!(
            volume_remove_status,
            OperationStatus::VolumeRemove {
                state: VolumeRemoveOperationState::Completed,
                ..
            }
        ),
        "volume remove did not complete: {volume_remove_status:?}"
    );
    let volumes = core
        .api
        .volume_list(&VolumeListRequest {})
        .await
        .expect("volume list after remove");
    assert!(volumes.volumes.is_empty(), "removed pin remains listed");
    let inspect_removed = core
        .exec_on(
            machine,
            &["docker", "volume", "inspect", docker_volume_name],
        )
        .await;
    assert!(
        !inspect_removed.success(),
        "explicitly removed Docker volume still exists: {inspect_removed:?}"
    );
}

async fn assert_volume_status(core: &CoreContext, expected: VolumeStatus) {
    let listed = core
        .api
        .volume_list(&VolumeListRequest {})
        .await
        .expect("volume list succeeds");
    let [volume] = listed.volumes.as_slice() else {
        panic!("expected one listed volume, got {:?}", listed.volumes);
    };
    assert_eq!(volume.namespace_id.as_str(), "volume");
    assert_eq!(volume.volume_name.as_str(), "data");
    assert_eq!(volume.status, expected);
}

async fn build_derived_image(
    docker: &ployz_e2e::bollard::Docker,
    tag: &str,
    base: &str,
    marker_name: &str,
    marker_contents: &str,
) {
    let dockerfile =
        format!("FROM {base}\nCOPY {marker_name} /usr/share/nginx/html/{marker_name}\n");
    let mut context = tar::Builder::new(Vec::new());
    append_build_file(&mut context, "Dockerfile", dockerfile.as_bytes());
    append_build_file(&mut context, marker_name, marker_contents.as_bytes());
    let context = context.into_inner().expect("finish image build context");
    let options = BuildImageOptionsBuilder::default()
        .dockerfile("Dockerfile")
        .t(tag)
        .rm(true)
        .build();
    let mut build = docker.build_image(options, None, Some(body_full(context.into())));
    while let Some(message) = build.next().await {
        let message = message.expect("host Docker image build request succeeds");
        if let Some(error) = message.error_detail {
            panic!(
                "host Docker image build failed: {}",
                error.message.as_deref().unwrap_or("unknown build error")
            );
        }
    }
    docker
        .inspect_image(tag)
        .await
        .unwrap_or_else(|error| panic!("built host image {tag} is missing: {error}"));
}

fn append_build_file(context: &mut tar::Builder<Vec<u8>>, path: &str, contents: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header
        .set_path(path)
        .expect("valid image build context path");
    header.set_size(u64::try_from(contents.len()).expect("build context file size fits u64"));
    header.set_mode(0o644);
    header.set_cksum();
    context
        .append(&header, contents)
        .expect("append image build context file");
}

/// The smoke service: the baked workload image, one replica per machine,
/// routed on every gateway's listen port.
fn smoke_deploy_target() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("smoke"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_smoke"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(2).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: vec![DeployRoute {
                target: DeployRouteTarget::Hostname {
                    hostname: route_hostname(ROUTE_HOSTNAME),
                    port: route_port(dind::MACHINE_GATEWAY_PORT),
                },
                endpoint_port: route_port(WORKLOAD_ENDPOINT_PORT),
            }],
        }],
    }
}

fn auto_hostname_deploy_target() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("auto_https"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_auto"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: vec![DeployRoute {
                target: DeployRouteTarget::AutoHostname {
                    port: route_port(dind::MACHINE_GATEWAY_TLS_PORT),
                },
                endpoint_port: route_port(WORKLOAD_ENDPOINT_PORT),
            }],
        }],
    }
}

fn internal_dns_deploy_target() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("internal_dns"),
        services: vec![
            DeployServiceSpec {
                service_id: service_id("server"),
                image: ImageReference::try_new(WORKLOAD_IMAGE)
                    .expect("valid workload image reference"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            },
            DeployServiceSpec {
                service_id: service_id("client"),
                image: ImageReference::try_new(WORKLOAD_IMAGE)
                    .expect("valid workload image reference"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                replicas: ReplicaCount::try_new(2).expect("valid replica count"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            },
        ],
    }
}

fn runtime_fields_deploy_target() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("runtime"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_runtime"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: runtime_fields_spec(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn volume_deploy_target(command: &str) -> DeployRequest {
    let source = format!(
        r#"
name: volume
services:
  svc_volume:
    image: {WORKLOAD_IMAGE}
    command: ["sh", "-c", {command:?}]
    volumes: [data:/data]
volumes:
  data: {{}}
"#
    );
    let (parsed, warnings) = parse_deploy_file(ComposeInput {
        source: &source,
        base_dir: Path::new("."),
        interpolation_env: BTreeMap::new(),
        namespace_override: None,
        mode: UnsupportedFieldMode::Strict,
    })
    .expect("volume compose parses");
    assert!(warnings.is_empty(), "volume compose emitted warnings");
    DeployRequest {
        namespace_id: parsed.namespace_id,
        services: parsed.services,
    }
}

fn volume_deploy_target_without_mount() -> DeployRequest {
    let mut target = volume_deploy_target("while true; do sleep 600; done");
    let [service] = target.services.as_mut_slice() else {
        panic!("volume target has one service");
    };
    service.runtime.volume_mounts.clear();
    target
}

fn failing_healthcheck_deploy_target() -> DeployRequest {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(
        ContainerCommand::try_new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "sleep 600".to_owned(),
        ])
        .expect("valid runtime command"),
    );
    runtime.healthcheck = Some(ContainerHealthcheck {
        test: ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("exit 1").expect("valid healthcheck"),
        ),
        interval: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
        timeout: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
        retries: Some(HealthcheckRetries::try_new(1).expect("retries")),
        start_period: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
    });
    DeployRequest {
        namespace_id: namespace_id("bad_health"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_bad_health"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime,
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn pre_start_deploy_target(namespace: &str, hook_command: &str) -> DeployRequest {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(
        ContainerCommand::try_new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "test -f /data/marker && sleep 600".to_owned(),
        ])
        .expect("valid service command"),
    );
    runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: VolumeName::try_new("data").expect("valid volume name"),
        target: ContainerMountPath::try_new("/data").expect("valid mount path"),
    }];
    DeployRequest {
        namespace_id: namespace_id(namespace),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_hooked"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime,
            pre_start: Some(PreStartHook {
                command: ContainerCommand::try_new(vec![
                    "sh".to_owned(),
                    "-c".to_owned(),
                    hook_command.to_owned(),
                ])
                .expect("valid hook command"),
            }),
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn depends_on_deploy_target() -> DeployRequest {
    let sleeper = || {
        let mut runtime = ContainerRuntimeSpec::image_defaults();
        runtime.command = Some(
            ContainerCommand::try_new(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "sleep 600".to_owned(),
            ])
            .expect("valid sleeper command"),
        );
        runtime
    };
    DeployRequest {
        namespace_id: namespace_id("depends_on_order"),
        services: vec![
            DeployServiceSpec {
                service_id: service_id("b"),
                image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: sleeper(),
                pre_start: None,
                depends_on: vec![service_id("a")],
                routes: Vec::new(),
            },
            DeployServiceSpec {
                service_id: service_id("a"),
                image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: sleeper(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            },
        ],
    }
}

fn runtime_fields_spec() -> ContainerRuntimeSpec {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(
        ContainerCommand::try_new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "env; sleep 600".to_owned(),
        ])
        .expect("valid runtime command"),
    );
    runtime.environment = ServiceEnvironment::from(BTreeMap::from([(
        EnvName::try_new("PLOYZ_E2E_RUNTIME").expect("valid env name"),
        EnvValue::try_new("present").expect("valid env value"),
    )]));
    runtime.healthcheck = Some(ContainerHealthcheck {
        test: ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("test \"$PLOYZ_E2E_RUNTIME\" = present")
                .expect("valid healthcheck"),
        ),
        interval: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
        timeout: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
        retries: Some(HealthcheckRetries::try_new(3).expect("retries")),
        start_period: Some(HealthcheckDurationNanos::try_new(1_000_000_000).expect("duration")),
    });
    runtime.restart_policy = ContainerRestartPolicy::UnlessStopped;
    runtime.cap_add = vec![LinuxCapability::try_new("NET_ADMIN").expect("valid capability")];
    runtime.cap_drop = vec![LinuxCapability::try_new("MKNOD").expect("valid capability")];
    runtime.resources = ContainerResourceLimits {
        nano_cpus: Some(NanoCpus::try_new(500_000_000).expect("valid nano cpus")),
        memory_bytes: Some(MemoryBytes::try_new(64_000_000).expect("valid memory")),
        pids: Some(PidsLimit::try_new(64).expect("valid pids")),
    };
    runtime
}

fn convergence_deploy_target(command: &str) -> DeployRequest {
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(
        ContainerCommand::try_new(vec!["sh".to_owned(), "-c".to_owned(), command.to_owned()])
            .expect("valid runtime command"),
    );

    DeployRequest {
        namespace_id: namespace_id("converge"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_converge"),
            image: ImageReference::try_new(WORKLOAD_IMAGE).expect("valid workload image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime,
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

async fn namespace_managed_containers(
    core: &CoreContext,
    namespace: &str,
) -> Vec<support::dind::assert::ManagedWorkloadContainer> {
    let mut containers = Vec::new();
    for machine in std::iter::once(core.cluster.core()).chain(core.cluster.edges()) {
        containers.extend(
            all_managed_workload_containers(core, machine)
                .await
                .into_iter()
                .filter(|container| {
                    container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str) == Some(namespace)
                }),
        );
    }
    containers
}

/// The route host header: hostname plus the gateway's in-machine listen
/// port (the route target port), not the published host port.
fn route_host_header() -> String {
    format!("{ROUTE_HOSTNAME}:{}", dind::MACHINE_GATEWAY_PORT)
}

/// Asserts the smoke route answers through one machine's published gateway
/// port with the workload's response body.
async fn assert_gateway_serves(machine: &DindMachine) {
    match gateway_http_get(machine.published.gateway, &route_host_header()).await {
        Ok(response) if response.contains("Welcome to nginx") => {}
        Ok(response) => panic!(
            "gateway on {} answered without the workload body: {response}",
            machine.name
        ),
        Err(error) => panic!("gateway on {} did not answer: {error}", machine.name),
    }
}

/// Waits for the smoke route's first answer through one machine's published
/// gateway port: the gateway converges on route intent from NATS after the
/// deploy completes, so first-serve is eventually consistent. Once serving,
/// assertions use the single-shot [`assert_gateway_serves`].
async fn wait_for_gateway_serving(machine: &DindMachine) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::from("<no attempt>");
    while Instant::now() < deadline {
        match gateway_http_get(machine.published.gateway, &route_host_header()).await {
            Ok(response) if response.contains("Welcome to nginx") => return,
            Ok(response) => last = format!("answered without the workload body: {response}"),
            Err(error) => last = format!("did not answer: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("gateway on {} never served the route: {last}", machine.name);
}

// ---------------------------------------------------------------------------
// Scenario 4 — daemon-restart invisibility
// ---------------------------------------------------------------------------

/// Restarts the control daemon on the core and the machine daemon on the edge
/// while the deploy is serving: gateway HTTP must keep answering throughout,
/// workload containers must be adopted (same IDs), and the operations API
/// must answer afterwards with unmutated machine state.
async fn scenario_daemon_restart_invisibility(core: &CoreContext, edge: &DindMachine) {
    let core_machine = core.cluster.core();

    let core_snapshot_before = wait_for_settled_snapshot(core, &machine_id("core_1")).await;
    let edge_snapshot_before = wait_for_settled_snapshot(core, &machine_id("edge_2")).await;
    let core_containers_before = workload_container_ids(core, core_machine).await;
    let edge_containers_before = workload_container_ids(core, edge).await;

    // Restart the daemons concurrently with the HTTP polling below.
    let docker = core.docker.clone();
    let core_container = core_machine.container_id.clone();
    let edge_container = edge.container_id.clone();
    let restart = tokio::spawn(async move {
        let control = exec_in_container(
            &docker,
            &core_container,
            &["systemctl", "restart", "ployzd-control"],
        )
        .await;
        let machine = exec_in_container(
            &docker,
            &edge_container,
            &["systemctl", "restart", "ployzd-machine-edge_2"],
        )
        .await;
        (control, machine)
    });

    // Gateway HTTP keeps answering during the whole restart window.
    let mut polls: u32 = 0;
    loop {
        let finished = restart.is_finished();
        for machine in [core_machine, edge] {
            assert_gateway_serves(machine).await;
        }
        polls += 1;
        if finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let restarted = restart.await.expect("restart task does not panic");
    match restarted {
        (Ok(control), Ok(machine)) if control.success() && machine.success() => {}
        other => panic!("daemon restarts failed: {other:?}"),
    }
    assert!(
        polls >= 2,
        "gateways must be polled during the restart window, not only after it"
    );
    assert_unit_active(core, core_machine, "ployzd-control").await;
    assert_unit_active(core, edge, "ployzd-machine-edge_2").await;

    // Adopt-not-recreate: the inner Docker container IDs are unchanged.
    let core_containers_after = workload_container_ids(core, core_machine).await;
    let edge_containers_after = workload_container_ids(core, edge).await;
    assert_eq!(
        core_containers_before, core_containers_after,
        "core workload containers must survive the control restart"
    );
    assert_eq!(
        edge_containers_before, edge_containers_after,
        "edge workload containers must survive the machine restart"
    );

    // The operations API answers after reconnect with unmutated machine
    // state (gateway status is the restarted processes' own live
    // observation, so the comparison covers identity, public ip, and the
    // observed container set).
    wait_for_matching_snapshot(core, &machine_id("core_1"), &core_snapshot_before).await;
    wait_for_matching_snapshot(core, &machine_id("edge_2"), &edge_snapshot_before).await;
}

/// Sorted running managed-container IDs inside one machine.
async fn workload_container_ids(core: &CoreContext, machine: &DindMachine) -> Vec<String> {
    let mut ids = managed_workload_containers(core, machine)
        .await
        .into_iter()
        .map(|container| container.id)
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

/// Polls until the machine snapshot reflects the completed deploy (one
/// observed workload container plus the standing observations), so the
/// post-restart comparison starts from settled truth.
async fn wait_for_settled_snapshot(core: &CoreContext, machine: &MachineId) -> MachineSnapshot {
    wait_for_inspect(
        core,
        machine,
        Duration::from_secs(60),
        "never settled after the deploy",
        answered_with_gateway_and_containers(1),
    )
    .await
}

/// Polls until the machine snapshot matches the pre-restart truth again:
/// same active state, same endpoints, same observed container count.
async fn wait_for_matching_snapshot(
    core: &CoreContext,
    machine: &MachineId,
    before: &MachineSnapshot,
) {
    wait_for_inspect(
        core,
        machine,
        Duration::from_secs(120),
        &format!("state never matched the pre-restart truth (expected {before:?})"),
        |snapshot| {
            let MachineSnapshot { active, testimony } = snapshot;
            *active == before.active && matching_answered_testimony(testimony, &before.testimony)
        },
    )
    .await;
}

fn answered_with_gateway_and_containers(count: usize) -> impl Fn(&MachineSnapshot) -> bool {
    move |snapshot| match &snapshot.testimony {
        MachineTestimony::Answered {
            endpoints,
            gateway,
            observed_container_count,
            ..
        } => endpoints.is_some() && gateway.is_some() && *observed_container_count == count,
        MachineTestimony::NoAnswer => false,
    }
}

fn matching_answered_testimony(current: &MachineTestimony, before: &MachineTestimony) -> bool {
    match (current, before) {
        (
            MachineTestimony::Answered {
                endpoints,
                gateway,
                observed_container_count,
                ..
            },
            MachineTestimony::Answered {
                endpoints: before_endpoints,
                observed_container_count: before_container_count,
                ..
            },
        ) => {
            endpoints == before_endpoints
                && gateway.is_some()
                && observed_container_count == before_container_count
        }
        (MachineTestimony::NoAnswer, MachineTestimony::Answered { .. })
        | (MachineTestimony::Answered { .. }, MachineTestimony::NoAnswer)
        | (MachineTestimony::NoAnswer, MachineTestimony::NoAnswer) => false,
    }
}

// ---------------------------------------------------------------------------
// Scenario 5 — auth rejection
// ---------------------------------------------------------------------------

/// The four committed rejections against the real cluster, plus the proof
/// that none of them hurt the data plane.
async fn scenario_auth_rejection(core: &CoreContext, edge: &DindMachine) {
    let cluster = &core.cluster;

    // (a) A fresh random NKey seed with the correct cluster CA is refused.
    let unauthorized_seed =
        SecuredTestNats::fresh_unauthorized_seed().expect("fresh unauthorized seed");
    let rejected =
        connect_core_client(core, NatsPrincipal::Operator, unauthorized_seed.secret()).await;
    assert!(
        rejected.is_err(),
        "a seed outside the authorized user set connected"
    );

    // (b) A plaintext client cannot complete a handshake with the TLS port.
    let plaintext_url = NatsClientUrl::try_new(format!("nats://{}", cluster.core().published.nats))
        .expect("valid plaintext url");
    let plaintext = connect_with_timeout(&plaintext_url, CONNECT_TIMEOUT).await;
    assert!(
        plaintext.is_err(),
        "a plaintext client reached the TLS-only port"
    );

    // (c) The edge machine's minted seed is fenced to its own scope: publishing
    // into the core machine's service scope and API command scope both
    // draw server-side permission violations.
    let edge_seed =
        read_file_from_container(&core.docker, &edge.container_id, EDGE_NATS_CREDS_FILE)
            .await
            .expect("edge nats.creds is readable");
    let edge_machine_config = host_client_config(
        cluster,
        &core.material,
        NatsPrincipal::Machine {
            machine_id: machine_id("edge_2"),
        },
        edge_seed.trim(),
    );
    let (edge_client, mut edge_events) = connect_with_event_capture(&edge_machine_config).await;
    for subject in [
        machine_service(&machine_id("core_1"), MachineServiceEndpoint::Inspect),
        OPERATOR_RUNTIME_SNAPSHOT.to_owned(),
    ] {
        edge_client
            .publish(subject.clone(), "evidence".into())
            .await
            .unwrap_or_else(|error| {
                panic!("publish to {subject} was not accepted client-side: {error}")
            });
        edge_client.flush().await.expect("edge client flush");
        match next_permission_violation(&mut edge_events).await {
            Some(violation) if violation.contains("Publish") => {}
            other => panic!("expected a publish violation for {subject}, got {other:?}"),
        }
    }

    // (d) The cluster's Join seed cannot sniff inboxes: the shared legacy
    // inbox scope and the core machine's own prefix are both refused.
    let join_config = host_client_config(
        cluster,
        &core.material,
        NatsPrincipal::Join,
        &core.material.join_seed,
    );
    let (join_client, mut join_events) = connect_with_event_capture(&join_config).await;
    let core_machine_inbox = inbox_subscribe_scope(&NatsPrincipal::Machine {
        machine_id: machine_id("core_1"),
    });
    for scope in ["_INBOX.>", core_machine_inbox.as_str()] {
        join_client
            .subscribe(scope.to_owned())
            .await
            .unwrap_or_else(|error| {
                panic!("subscribe {scope} was not accepted client-side: {error}")
            });
        join_client.flush().await.expect("join client flush");
        match next_permission_violation(&mut join_events).await {
            Some(violation) if violation.contains("Subscription") => {}
            other => panic!("expected a subscription violation for {scope}, got {other:?}"),
        }
    }

    // The cluster shrugged all of it off: both gateways still serve and the
    // control API still answers.
    for machine in [cluster.core(), edge] {
        assert_gateway_serves(machine).await;
    }
    core.api
        .machine_list(&MachineListRequest {})
        .await
        .expect("control API healthy after rejected clients");
}

// ---------------------------------------------------------------------------
// Scenario 1 detail assertions
// ---------------------------------------------------------------------------
