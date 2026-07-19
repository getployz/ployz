use super::{DEPLOY_TERMINAL_BUDGET, reserved_deploy_request};
use crate::support::dind::CONNECT_TIMEOUT;
use crate::support::dind::assert::{
    managed_workload_containers, terminal_operation_events, wait_for_terminal_deploy_status,
};
use crate::support::dind::contracts::{
    CONTAINER_TYPE_LABEL, NAMESPACE_ID_LABEL, SERVICE_ID_LABEL, read_intent,
};
use crate::support::dind::formation::{CoreContext, connect_core_client};
use ployz_core::deploy::{
    ContainerCommand, ContainerRuntimeSpec, DeployRequest, DeployServiceSpec, EnvName, EnvValue,
    ImageReference, ReplicaCount, ServiceEnvironment,
};
use ployz_core::operation::{
    DeployCompletionOutcome, DeployOperationState, OperationEvent, OperationStatus,
};
use ployz_core::security::NatsPrincipal;
use ployz_sdk_types::{ServiceInspectRequest, ServiceMachineTestimony};
use ployz_test_support::ids::{namespace_id, service_id};
use std::collections::BTreeMap;

const SECRET_SENTINEL: &str = "ployz-e2e-secret-sentinel-665";

/// Environment values cross the operator API into only the selected runtime,
/// while durable and public evidence retains only their names.
pub(super) async fn scenario_deploy_environment_evidence_boundary(
    core: &CoreContext,
    workload_image: &ImageReference,
) {
    const ENV_NAME: &str = "PLOYZ_E2E_DEPLOY_SECRET";

    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.command = Some(
        ContainerCommand::try_new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "sleep 600".to_owned(),
        ])
        .expect("valid sleeper command"),
    );
    runtime.environment = ServiceEnvironment::from(BTreeMap::from([(
        EnvName::try_new(ENV_NAME).expect("valid environment name"),
        EnvValue::try_new(SECRET_SENTINEL).expect("valid environment value"),
    )]));
    let accepted = core
        .api
        .deploy_submit(
            &reserved_deploy_request(
                core,
                "idem_dind_deploy_env_evidence",
                DeployRequest {
                    namespace_id: namespace_id("deploy_env_evidence"),
                    origin: None,
                    volumes: BTreeMap::new(),
                    services: vec![DeployServiceSpec {
                        keep: None,
                        service_id: service_id("svc_deploy_env_evidence"),
                        image: workload_image.clone(),
                        image_source: ployz_core::deploy::ImageSource::Registry,
                        mode: ployz_core::deploy::ServiceMode::Replicated {
                            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                        },
                        runtime,
                        pre_start: None,
                        depends_on: Vec::new(),
                        routes: Vec::new(),
                    }],
                },
            )
            .await,
        )
        .await
        .expect("environment boundary deploy submits");
    let status =
        wait_for_terminal_deploy_status(core, &accepted.operation_id, DEPLOY_TERMINAL_BUDGET).await;
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
        "environment boundary deploy did not complete"
    );

    let machines = std::iter::once(core.cluster.core())
        .chain(core.cluster.edges())
        .collect::<Vec<_>>();
    let mut placements = Vec::new();
    let mut placement_counts = Vec::new();
    for machine in &machines {
        let containers = managed_workload_containers(core, machine)
            .await
            .into_iter()
            .filter(|container| {
                container.labels.get(NAMESPACE_ID_LABEL).map(String::as_str)
                    == Some("deploy_env_evidence")
                    && container.labels.get(SERVICE_ID_LABEL).map(String::as_str)
                        == Some("svc_deploy_env_evidence")
                    && container
                        .labels
                        .get(CONTAINER_TYPE_LABEL)
                        .map(String::as_str)
                        == Some("service")
            })
            .collect::<Vec<_>>();
        assert!(
            containers.len() <= 1,
            "one service replica created multiple containers on one machine"
        );
        placement_counts.push((machine.name.as_str(), containers.len()));
        if let [container] = containers.as_slice() {
            placements.push((*machine, container.clone()));
        }
    }
    let [(selected_machine, selected_container)] = placements.as_slice() else {
        panic!("one service replica must run on exactly one selected machine");
    };
    assert!(
        placement_counts
            .iter()
            .filter(|(machine_name, _)| *machine_name != selected_machine.name)
            .all(|(_, count)| *count == 0),
        "the non-selected machine ran a managed container for the service"
    );
    assert!(
        selected_container
            .env
            .iter()
            .any(|entry| entry == &format!("{ENV_NAME}={SECRET_SENTINEL}")),
        "selected container config did not retain the exact submitted environment value"
    );
    let process_environment = core
        .exec_on(
            selected_machine,
            &[
                "docker",
                "exec",
                &selected_container.id,
                "printenv",
                ENV_NAME,
            ],
        )
        .await;
    assert!(
        process_environment.success() && process_environment.stdout.trim() == SECRET_SENTINEL,
        "selected container process did not receive the exact submitted environment value"
    );

    let events = terminal_operation_events(core, &accepted.operation_id).await;
    assert_secret_absent(
        "terminal deploy evidence",
        &serde_json::to_string(&events).expect("terminal deploy evidence serializes"),
    );
    let Some(target) = events.iter().find_map(|event| {
        if let OperationEvent::DeploySubmitted { target, .. } = event {
            Some(target)
        } else {
            None
        }
    }) else {
        panic!("terminal replay omitted DeploySubmitted evidence");
    };
    let [environment] = target.environment_names() else {
        panic!("DeploySubmitted evidence must contain one service environment name set");
    };
    assert_eq!(
        environment.service_id(),
        &service_id("svc_deploy_env_evidence")
    );
    assert_eq!(
        environment
            .names()
            .iter()
            .map(EnvName::as_str)
            .collect::<Vec<_>>(),
        vec![ENV_NAME]
    );

    let controller = connect_core_client(
        core,
        NatsPrincipal::Controller,
        &core.material.controller_seed,
    )
    .await
    .expect("connect controller for intent evidence read");
    let intent = read_intent(&controller, CONNECT_TIMEOUT)
        .await
        .expect("read serving intent after environment deploy");
    assert!(
        intent.serving_target_entries.iter().any(|entry| {
            entry.namespace_id == namespace_id("deploy_env_evidence")
                && entry.service_id == service_id("svc_deploy_env_evidence")
        }),
        "serving intent contains the completed environment-boundary deploy"
    );
    assert_secret_absent(
        "serving intent",
        &serde_json::to_string(&intent).expect("serving intent serializes"),
    );
    let service_snapshot = core
        .api
        .service_inspect(&ServiceInspectRequest {
            namespace_id: namespace_id("deploy_env_evidence"),
            service_id: service_id("svc_deploy_env_evidence"),
        })
        .await
        .expect("inspect deployed environment-boundary service");
    assert!(
        service_snapshot.testimony.observed_container_count > 0,
        "service inspection observed no service containers"
    );
    assert!(
        service_snapshot.testimony.machines.iter().any(|machine| {
            let ServiceMachineTestimony::Answered { containers, .. } = machine else {
                return false;
            };
            containers.iter().any(|container| {
                container.observation.identity.namespace_id == namespace_id("deploy_env_evidence")
                    && container.observation.identity.service_id
                        == service_id("svc_deploy_env_evidence")
            })
        }),
        "service inspection omitted the concrete namespace/service observation"
    );
    assert_secret_absent(
        "populated service snapshot",
        &serde_json::to_string(&service_snapshot).expect("service snapshot serializes"),
    );
}

fn assert_secret_absent(label: &str, serialized: &str) {
    assert!(
        !serialized.contains(SECRET_SENTINEL),
        "{label} exposed a plaintext environment value"
    );
}
