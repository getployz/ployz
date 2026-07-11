use super::*;
use ployz::commands::parse_command;
use ployz::deploy_history::{ClusterFingerprint, DeployHistory};
use ployz::runtime::{PloyzctlRuntimeConfig, execute_command};

/// Rollback replays successful local history as a fresh deploy with the exact
/// pinned image identity, and a missing pinned registry artifact fails without
/// falling back to the mutable tag.
#[tokio::test]
async fn scenario_deploy_rollback_replays_pinned_history() {
    if !dind::e2e_enabled() {
        return;
    }
    let docker = dind::connect_docker().expect("connect to Docker daemon");
    let core = init_core_cluster(&docker, 0).await;
    with_evidence(&core.cluster, async {
        wait_for_machine_observations(&core, &machine_id("core_1")).await;

        let namespace = namespace_id("rollback");
        let service = service_id("svc_rollback");
        let v1 = "127.0.0.1:5001/rollback/web:v1";
        let v2 = "127.0.0.1:5001/rollback/web:v2";
        let setup = core
            .exec_sh(
                core.cluster.core(),
                &format!(
                    "set -eu
docker run -d --name ployz-rollback-registry -p 5001:5000 -e REGISTRY_STORAGE_DELETE_ENABLED=true {REGISTRY_IMAGE} >/dev/null
for attempt in $(seq 1 30); do
  if curl -fsS http://127.0.0.1:5001/v2/ >/dev/null; then break; fi
  sleep 1
done
mkdir -p /tmp/ployz-rollback-v1 /tmp/ployz-rollback-v2
printf '%s\n' 'FROM {WORKLOAD_IMAGE}' 'COPY index.html /usr/share/nginx/html/index.html' > /tmp/ployz-rollback-v1/Dockerfile
printf '%s\n' 'rollback-v1' > /tmp/ployz-rollback-v1/index.html
printf '%s\n' 'FROM {WORKLOAD_IMAGE}' 'COPY index.html /usr/share/nginx/html/index.html' > /tmp/ployz-rollback-v2/Dockerfile
printf '%s\n' 'rollback-v2' > /tmp/ployz-rollback-v2/index.html
docker build -q -t {v1} /tmp/ployz-rollback-v1 >/dev/null
docker build -q -t {v2} /tmp/ployz-rollback-v2 >/dev/null
docker push {v1} >/dev/null
docker push {v2} >/dev/null"
                ),
            )
            .await;
        assert!(setup.success(), "rollback registry setup failed: {setup:?}");

        let history = tempfile::tempdir().expect("create isolated deploy history");
        let config = PloyzctlRuntimeConfig {
            nats_url: Some(format!("tls://{}", core.cluster.core().published.nats)),
            nats_ca_file: Some(core.material.ca_file.clone()),
            nats_seed_file: Some(core.material.operator_seed_file.clone()),
            join_seed_file: Some(core.material.join_seed_file.clone()),
            ops_watch_timeout: Some(DEPLOY_TERMINAL_BUDGET),
            deploy_history_root: Some(history.path().to_path_buf()),
            ..PloyzctlRuntimeConfig::default()
        };
        let history_store = DeployHistory::new(
            history.path().to_path_buf(),
            ClusterFingerprint::from_connection(
                config.nats_url.as_deref().expect("NATS URL configured"),
                config.nats_ca_file.as_deref().expect("NATS CA configured"),
            )
            .expect("cluster fingerprint"),
            namespace.clone(),
        );

        for (image, origin) in [(v1, "dind-rollback-v1"), (v2, "dind-rollback-v2")] {
            let command = parse_command(
                [
                    "deploy",
                    "--namespace",
                    namespace.as_str(),
                    "--service",
                    service.as_str(),
                    "--image",
                    image,
                    "--from-registry",
                    "--origin",
                    origin,
                ]
                .into_iter()
                .map(str::to_owned),
            )
            .expect("registry deploy command parses");
            execute_command(command, &config)
                .await
                .unwrap_or_else(|error| panic!("registry deploy {origin} failed: {error}"));
        }

        let entries = history_store.load().expect("v1 and v2 history loads");
        let [v1_entry, v2_entry] = entries.as_slice() else {
            panic!("expected v1 and v2 history, got {entries:?}");
        };
        let [v1_service] = v1_entry.request.services.as_slice() else {
            panic!("v1 history must contain one service");
        };
        let [v2_service] = v2_entry.request.services.as_slice() else {
            panic!("v2 history must contain one service");
        };
        let v1_operation = v1_entry.operation_id.clone();
        let v2_operation = v2_entry.operation_id.clone();
        let v1_resolved = v1_service.image.clone();
        let v2_resolved = v2_service.image.clone();
        assert_ne!(v1_resolved, v2_resolved, "v1 and v2 must be distinct");

        let rollback = parse_command(
            [
                "deploy",
                "rollback",
                "--namespace",
                namespace.as_str(),
                "--to",
                v1_operation.as_str(),
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("rollback to v1 parses");
        execute_command(rollback, &config)
            .await
            .expect("rollback to v1 executes");

        let entries = history_store.load().expect("successful rollback history loads");
        let [_, _, rollback_entry] = entries.as_slice() else {
            panic!("successful rollback must append history: {entries:?}");
        };
        let rollback_operation = rollback_entry.operation_id.clone();
        let rollback_status = operation_status(&core, &rollback_operation).await;
        let OperationStatus::Deploy {
            state:
                DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::Completed,
                },
            ..
        } = &rollback_status
        else {
            panic!("rollback to v1 did not complete: {rollback_status:?}");
        };
        assert_eq!(
            rollback_entry
                .request
                .origin
                .as_ref()
                .map(|origin| origin.as_str()),
            Some("rollback")
        );
        assert!(
            rollback_entry
                .request
                .services
                .iter()
                .any(|service| service.image == v1_resolved)
        );
        let rollback_events = terminal_operation_events(&core, &rollback_operation).await;
        assert!(
            rollback_events.iter().any(|event| matches!(
                event,
                OperationEvent::DeploySubmitted { operation_id, target, .. }
                    if operation_id == &rollback_operation
                        && target.origin.as_ref().is_some_and(|origin| origin.as_str() == "rollback")
                        && target.services.iter().any(|service| service.image == v1_resolved)
            )),
            "rollback did not replay v1's pinned request: {rollback_events:?}"
        );
        assert!(
            !rollback_events
                .iter()
                .any(|event| matches!(event, OperationEvent::DeployImageResolved { .. })),
            "pinned rollback must not resolve a mutable tag: {rollback_events:?}"
        );
        let snapshot = core
            .api
            .service_inspect(&ServiceInspectRequest {
                namespace_id: namespace.clone(),
                service_id: service.clone(),
            })
            .await
            .expect("rolled-back service manifest reads");
        assert_eq!(snapshot.active.image, v1_resolved);
        let containers = namespace_managed_containers(&core, namespace.as_str()).await;
        let [container] = containers.as_slice() else {
            panic!("rollback must leave one v1 container: {containers:?}");
        };
        assert_eq!(
            container.labels.get(OPERATION_ID_LABEL).map(String::as_str),
            Some(rollback_operation.as_str())
        );
        let content = core
            .exec_on(
                core.cluster.core(),
                &[
                    "docker",
                    "exec",
                    &container.id,
                    "cat",
                    "/usr/share/nginx/html/index.html",
                ],
            )
            .await;
        assert!(
            content.success() && content.stdout.trim() == "rollback-v1",
            "rollback container has wrong content: {content:?}"
        );

        let v1_digest = v1_resolved
            .pinned_digest()
            .expect("v1 history image is pinned");
        let v2_digest = v2_resolved
            .pinned_digest()
            .expect("v2 history image is pinned");
        let remove_v2 = core
            .exec_sh(
                core.cluster.core(),
                &format!(
                    "set -eu
v2_image_id=$(docker image inspect --format '{{{{.Id}}}}' {v2_resolved})
docker tag {v1} {v2}
docker push {v2} >/dev/null
curl -fsS -X DELETE -H 'Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json' http://127.0.0.1:5001/v2/rollback/web/manifests/{v2_digest} >/dev/null
docker image rm -f \"$v2_image_id\" >/dev/null
if docker image inspect {v2_resolved} >/dev/null 2>&1; then exit 1; fi
if curl -fsSI -H 'Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json' http://127.0.0.1:5001/v2/rollback/web/manifests/{v2_digest} >/dev/null; then exit 1; fi
curl -fsSI -H 'Accept: application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json' http://127.0.0.1:5001/v2/rollback/web/manifests/v2 | tr -d '\r' | grep -i -F 'docker-content-digest: {v1_digest}' >/dev/null",
                    v2_resolved = v2_resolved.as_str(),
                ),
            )
            .await;
        assert!(
            remove_v2.success(),
            "v2 digest was not made exclusively unavailable: {remove_v2:?}"
        );

        let rollback = parse_command(
            [
                "deploy",
                "rollback",
                "--namespace",
                namespace.as_str(),
                "--to",
                v2_operation.as_str(),
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("rollback to missing v2 parses");
        execute_command(rollback, &config)
            .await
            .expect("failed rollback remains a completed CLI execution");

        let operations = core
            .api
            .ops_list(&OpsListRequest { active_only: false })
            .await
            .expect("list failed rollback")
            .operations;
        let failed_status = operations
            .iter()
            .find(|snapshot| {
                matches!(
                    &snapshot.status,
                    OperationStatus::Deploy {
                        id,
                        namespace_id,
                        origin: Some(origin),
                        ..
                    } if id != &rollback_operation
                        && id != &v1_operation
                        && id != &v2_operation
                        && namespace_id == &namespace
                        && origin.as_str() == "rollback"
                )
            })
            .map(|snapshot| snapshot.status.clone())
            .unwrap_or_else(|| panic!("failed rollback deploy is missing: {operations:?}"));
        let OperationStatus::Deploy {
            id: failed_operation,
            state:
                DeployOperationState::Failed {
                    failure:
                        DeployOperationFailure::ArtifactUnavailable {
                            service_id,
                            reason:
                                ArtifactUnavailableReason::ImagePullFailed {
                                    machine_id: failed_machine_id,
                                    ..
                                },
                            ..
                        },
                },
            ..
        } = &failed_status
        else {
            panic!("missing v2 digest did not produce typed image failure: {failed_status:?}");
        };
        assert_eq!(service_id, &service);
        assert_eq!(failed_machine_id, &machine_id("core_1"));
        let failed_events = terminal_operation_events(&core, failed_operation).await;
        assert!(
            failed_events.iter().any(|event| matches!(
                event,
                OperationEvent::DeploySubmitted { target, .. }
                    if target.services.iter().any(|service| service.image == v2_resolved)
            )),
            "failed rollback did not submit v2's pinned digest: {failed_events:?}"
        );
        assert!(
            !failed_events
                .iter()
                .any(|event| matches!(event, OperationEvent::DeployImageResolved { .. })),
            "failed rollback re-resolved the surviving v2 tag: {failed_events:?}"
        );
    })
    .await;
    finish(core).await;
}
