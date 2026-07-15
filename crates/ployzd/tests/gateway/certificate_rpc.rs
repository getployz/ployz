use std::path::Path;
use std::time::Duration;

use ployz_core::certificate::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
    CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    CertificateChallengeApplicationStatus, CertificateChallengeStatusRequest,
    CertificateChallengeStatusResponse, CustomCertBundle, GatewayCertificateRpcError,
    custom_bundle_digest,
};
use ployz_core::ids::RouteBindingId;
use ployz_core::ingress::{CertificateOwner, RouteBindingOrigin};
use ployz_core::install::{AbsoluteInstallPath, InstallSha256Digest};
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::machine::{GatewayServingStatus, GatewayStatusObservation};
use ployz_core::operation::RouteTarget;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::{NatsServiceResponse, request_json, start_nats_service};
use ployz_nats::services::EndpointExecution;
use ployz_test_support::ids::{cert_id, machine_id, operation_id, route_hostname};
use ployzd::operations::deploy::MachineRuntimeUnavailableReason;
use ployzd::roles::gateway::client::{
    GatewayStatusGetOk, GatewayStatusGetResponse, NatsGatewayStatusReader,
};
use ployzd::roles::gateway::pingora::PingoraRouteRegistry;
use ployzd::roles::gateway::process::start_gateway_certificate_service;
use ployzd::roles::gateway::projection::{
    GatewayCertificateBundle, GatewayProjectedRoute, GatewayProjection,
};
use ployzd::roles::gateway::source::{GatewayCertificateStore, GatewayCertificateStoreError};
use ployzd::service_catalog::{DaemonServiceCatalog, gateway_role_service, machine_endpoint_spec};
use rcgen::{CertificateParams, KeyPair};
use time::OffsetDateTime;

const NOT_BEFORE: u64 = 1_577_836_800;
const NOT_AFTER: u64 = 2_208_988_800;
const NOW: u64 = 1_800_000_000;

#[test]
fn gateway_certificate_catalog_pins_names_subjects_and_execution() {
    let machine_id = machine_id("machine_7");
    let service = gateway_role_service(&machine_id);

    assert_eq!(service.name, "plz-gateway-machine");
    assert_eq!(service.id, "plz-gateway-machine.machine_7");
    assert_eq!(service.metadata.get("machine_id"), Some("machine_7"));
    assert_eq!(
        service
            .endpoints
            .iter()
            .map(|endpoint| (endpoint.name, endpoint.subject.as_str(), endpoint.execution,))
            .collect::<Vec<_>>(),
        vec![
            (
                "machine.certificate.artifact.push",
                "plz.v1.rpc.machine.command.machine_7.certificate.artifact.push",
                EndpointExecution::MachineRpc,
            ),
            (
                "machine.certificate.artifact.remove",
                "plz.v1.rpc.machine.command.machine_7.certificate.artifact.remove",
                EndpointExecution::MachineRpc,
            ),
            (
                "machine.certificate.challenge.apply",
                "plz.v1.rpc.machine.command.machine_7.certificate.challenge.apply",
                EndpointExecution::MachineRpc,
            ),
            (
                "machine.certificate.challenge.remove",
                "plz.v1.rpc.machine.command.machine_7.certificate.challenge.remove",
                EndpointExecution::MachineRpc,
            ),
            (
                "machine.certificate.challenge.status",
                "plz.v1.rpc.machine.query.machine_7.certificate.challenge.status",
                EndpointExecution::MachineRpc,
            ),
            (
                "machine.gateway.status.get",
                "plz.v1.rpc.machine.query.machine_7.gateway.status.get",
                EndpointExecution::MachineRpc,
            ),
        ]
    );
    assert_eq!(
        DaemonServiceCatalog::for_gateway(&machine_id).services(),
        &[service]
    );
}

#[test]
fn artifact_push_stores_material_at_gateway_derived_path() {
    let state = tempfile::tempdir().expect("state directory");
    let untrusted = tempfile::tempdir().expect("untrusted core directory");
    let core_path = untrusted.path().join("core-owned.bundle");
    let request = artifact_request("app.example.com", &core_path);
    let store = GatewayCertificateStore::new(state.path().to_path_buf());

    assert_eq!(store.push_at(&request, NOW), Ok(()));
    let local_path = store
        .artifact_path(request.bundle.active_cert())
        .expect("local artifact path");
    let expected_filename = format!(
        "cert_app_example_com-{}.bundle",
        request.expected_digest.as_str()
    );
    assert!(local_path.starts_with(state.path()));
    assert_eq!(
        local_path.file_name().and_then(|name| name.to_str()),
        Some(expected_filename.as_str())
    );
    assert!(!core_path.exists());
    assert_eq!(
        std::fs::read(local_path).expect("stored artifact"),
        request.bundle.material_bytes()
    );
}

#[cfg(unix)]
#[test]
fn artifact_push_restricts_material_to_owner_read_write() {
    use std::os::unix::fs::PermissionsExt;

    let state = tempfile::tempdir().expect("state directory");
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    let store = GatewayCertificateStore::new(state.path().to_path_buf());
    store.push_at(&request, NOW).expect("store artifact");
    let path = store
        .artifact_path(request.bundle.active_cert())
        .expect("artifact path");

    assert_eq!(
        std::fs::metadata(path)
            .expect("artifact metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn repeated_artifact_push_is_idempotent() {
    let state = tempfile::tempdir().expect("state directory");
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    let store = GatewayCertificateStore::new(state.path().to_path_buf());
    store.push_at(&request, NOW).expect("initial push");

    assert_eq!(store.push_at(&request, NOW), Ok(()));
}

#[test]
fn stored_artifact_is_adopted_from_active_metadata_after_restart() {
    let state = tempfile::tempdir().expect("state directory");
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    GatewayCertificateStore::new(state.path().to_path_buf())
        .push_at(&request, NOW)
        .expect("initial push");
    let active = request.bundle.active_cert().clone();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = GatewayCertificateStore::new(state.path().to_path_buf())
            .artifact_path(&active)
            .expect("artifact path");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("make artifact permissive");
        std::fs::set_permissions(
            path.parent().expect("artifact parent"),
            std::fs::Permissions::from_mode(0o755),
        )
        .expect("make artifact directory permissive");
    }

    assert_eq!(
        GatewayCertificateStore::new(state.path().to_path_buf()).load(request.bundle.active_cert()),
        Ok(request.bundle)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let path = GatewayCertificateStore::new(state.path().to_path_buf())
            .artifact_path(&active)
            .expect("artifact path");
        assert_eq!(
            (
                std::fs::metadata(path.parent().expect("artifact parent"))
                    .expect("directory metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                std::fs::metadata(path)
                    .expect("artifact metadata")
                    .permissions()
                    .mode()
                    & 0o777,
            ),
            (0o700, 0o600)
        );
    }
}

#[test]
fn wrong_artifact_size_does_not_replace_valid_material() {
    let state = tempfile::tempdir().expect("state directory");
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    let store = GatewayCertificateStore::new(state.path().to_path_buf());
    store.push_at(&request, NOW).expect("initial push");
    let path = store
        .artifact_path(request.bundle.active_cert())
        .expect("artifact path");
    let original = std::fs::read(&path).expect("original artifact");
    let invalid = CertificateArtifactPushRequest {
        expected_size: request.expected_size + 1,
        ..request
    };

    assert!(matches!(
        store.push_at(&invalid, NOW),
        Err(GatewayCertificateStoreError::SizeMismatch { .. })
    ));
    assert_eq!(std::fs::read(path).expect("retained artifact"), original);
}

#[test]
fn wrong_artifact_digest_is_rejected() {
    let state = tempfile::tempdir().expect("state directory");
    let mut request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    let wrong = if request.expected_digest.as_str().starts_with('a') {
        "b".repeat(64)
    } else {
        "a".repeat(64)
    };
    request.expected_digest = InstallSha256Digest::try_new(wrong).expect("digest");
    let store = GatewayCertificateStore::new(state.path().to_path_buf());

    assert!(matches!(
        store.push_at(&request, NOW),
        Err(GatewayCertificateStoreError::DigestMismatch { .. })
    ));
}

#[test]
fn certificate_for_different_san_is_rejected() {
    let state = tempfile::tempdir().expect("state directory");
    let (certificate, private_key) =
        certificate_material("other.example.com", NOT_BEFORE, NOT_AFTER);
    let request = request_from_material(
        "app.example.com",
        Path::new("/core/owned.bundle"),
        certificate,
        private_key,
        NOT_BEFORE,
        NOT_AFTER,
    );
    let store = GatewayCertificateStore::new(state.path().to_path_buf());

    assert!(matches!(
        store.push_at(&request, NOW),
        Err(GatewayCertificateStoreError::InvalidMaterial { message })
            if message.contains("does not cover requested hostname")
    ));
}

#[test]
fn certificate_with_different_private_key_is_rejected() {
    let state = tempfile::tempdir().expect("state directory");
    let (certificate, _) = certificate_material("app.example.com", NOT_BEFORE, NOT_AFTER);
    let (_, private_key) = certificate_material("app.example.com", NOT_BEFORE, NOT_AFTER);
    let request = request_from_material(
        "app.example.com",
        Path::new("/core/owned.bundle"),
        certificate,
        private_key,
        NOT_BEFORE,
        NOT_AFTER,
    );
    let store = GatewayCertificateStore::new(state.path().to_path_buf());

    assert!(matches!(
        store.push_at(&request, NOW),
        Err(GatewayCertificateStoreError::InvalidMaterial { message })
            if message.contains("does not match its private key")
    ));
}

#[test]
fn certificate_not_yet_valid_is_rejected() {
    let state = tempfile::tempdir().expect("state directory");
    let not_before = NOW + 100;
    let not_after = NOW + 1_000;
    let (certificate, private_key) = certificate_material("app.example.com", not_before, not_after);
    let request = request_from_material(
        "app.example.com",
        Path::new("/core/owned.bundle"),
        certificate,
        private_key,
        not_before,
        not_after,
    );
    let store = GatewayCertificateStore::new(state.path().to_path_buf());

    assert!(matches!(
        store.push_at(&request, NOW),
        Err(GatewayCertificateStoreError::NotUsable { .. })
    ));
}

#[test]
fn expired_certificate_push_is_rejected() {
    let state = tempfile::tempdir().expect("state directory");
    let not_after = NOW - 100;
    let (certificate, private_key) = certificate_material("app.example.com", NOT_BEFORE, not_after);
    let request = request_from_material(
        "app.example.com",
        Path::new("/core/owned.bundle"),
        certificate,
        private_key,
        NOT_BEFORE,
        not_after,
    );

    assert!(matches!(
        GatewayCertificateStore::new(state.path().to_path_buf()).push_at(&request, NOW),
        Err(GatewayCertificateStoreError::NotUsable { .. })
    ));
}

#[test]
fn expired_stored_certificate_keeps_serving_with_its_renewal_challenge() {
    let state = tempfile::tempdir().expect("state directory");
    let not_after = NOW - 100;
    let (certificate, private_key) = certificate_material("app.example.com", NOT_BEFORE, not_after);
    let request = request_from_material(
        "app.example.com",
        Path::new("/core/owned.bundle"),
        certificate,
        private_key,
        NOT_BEFORE,
        not_after,
    );
    let store = GatewayCertificateStore::new(state.path().to_path_buf());
    store
        .push_at(&request, NOT_BEFORE + 1)
        .expect("certificate is initially activated while valid");
    let bundle = store
        .load(request.bundle.active_cert())
        .expect("lapsed serving material remains loadable");
    let challenge = challenge("app.example.com", "renewal-token");
    let registry = PingoraRouteRegistry::new();
    registry
        .replace_projection(&GatewayProjection {
            certificate_bundles: vec![GatewayCertificateBundle {
                owner: route_owner(),
                bundle,
            }],
            challenges: vec![challenge.clone()],
            routes: vec![GatewayProjectedRoute {
                id: route_binding_id(),
                origin: RouteBindingOrigin::Declared,
                target: RouteTarget::new(route_hostname("app.example.com")),
                upstreams: Vec::new(),
                unroutable_containers: Vec::new(),
            }],
        })
        .expect("lapsed TLS and renewal challenge apply together");
    registry.apply_challenge(&challenge);

    assert_eq!(
        (
            registry.is_https_hostname("app.example.com"),
            registry.challenge_application_status(&challenge),
        ),
        (true, CertificateChallengeApplicationStatus::Applied)
    );
}

#[tokio::test]
async fn artifact_push_endpoint_returns_typed_machine_ack() {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_7")]).await;
    let state = tempfile::tempdir().expect("state directory");
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    let expected_digest = request.expected_digest.clone();
    let service = start_gateway_certificate_service(
        nats.machine_client(&machine_id("machine_7")).await,
        machine_id("machine_7"),
        GatewayCertificateStore::new(state.path().to_path_buf()),
        PingoraRouteRegistry::new(),
    )
    .await
    .expect("gateway certificate service");

    let response: CertificateArtifactPushResponse = request_json(
        &nats.controller,
        machine_service(
            &machine_id("machine_7"),
            MachineServiceEndpoint::CertificateArtifactPush,
        ),
        &request,
        Duration::from_secs(1),
    )
    .await
    .expect("artifact push response");

    assert!(matches!(
        response,
        MachineRpcResponse::Ok(ok)
            if ok.machine_id == machine_id("machine_7")
                && ok.cert_id == cert_id("cert_app_example_com")
                && ok.digest == expected_digest
    ));
    service.shutdown().await.expect("service shutdown");
}

#[tokio::test]
async fn gateway_status_endpoint_reads_fresh_process_state() {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_7")]).await;
    let state = tempfile::tempdir().expect("state directory");
    let service = start_gateway_certificate_service(
        nats.machine_client(&machine_id("machine_7")).await,
        machine_id("machine_7"),
        GatewayCertificateStore::new(state.path().to_path_buf()),
        PingoraRouteRegistry::new(),
    )
    .await
    .expect("gateway service");
    let reader = NatsGatewayStatusReader::new(nats.controller.clone())
        .with_request_timeout(Duration::from_secs(1));

    assert_eq!(
        reader
            .read(&machine_id("machine_7"))
            .await
            .expect("gateway status"),
        GatewayStatusObservation {
            machine_id: machine_id("machine_7"),
            listen_addr: "0.0.0.0:0".parse().expect("listen address"),
            serving: GatewayServingStatus::Unavailable,
            route_count: 0,
        }
    );
    service.shutdown().await.expect("service shutdown");
}

#[tokio::test]
async fn gateway_status_client_rejects_wrong_machine_response() {
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        machine_id("machine_a"),
        machine_id("machine_b"),
    ])
    .await;
    let requested = machine_id("machine_a");
    let mut service = start_nats_service(
        nats.machine_client(&requested).await,
        &gateway_role_service(&requested),
    )
    .await
    .expect("gateway service");
    service
        .bind_endpoint(
            &machine_endpoint_spec(&requested, MachineServiceEndpoint::GatewayStatusGet),
            |_request| async {
                NatsServiceResponse::json_ok(&GatewayStatusGetResponse::Ok(GatewayStatusGetOk {
                    observation: GatewayStatusObservation {
                        machine_id: machine_id("machine_b"),
                        listen_addr: "192.0.2.8:80".parse().expect("listen address"),
                        serving: GatewayServingStatus::Current,
                        route_count: 1,
                    },
                }))
            },
        )
        .await
        .expect("bind status endpoint");
    let reader = NatsGatewayStatusReader::new(nats.controller.clone())
        .with_request_timeout(Duration::from_secs(1));

    let error = reader
        .read(&requested)
        .await
        .expect_err("wrong machine response");

    assert_eq!(error.machine_id, requested);
    assert_eq!(
        error.reason,
        MachineRuntimeUnavailableReason::WrongResponder {
            actual_machine_id: machine_id("machine_b"),
        }
    );
    service.shutdown().await.expect("service shutdown");
}

#[tokio::test]
async fn artifact_push_endpoint_returns_typed_invalid_request() {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_7")]).await;
    let state = tempfile::tempdir().expect("state directory");
    let mut request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    request.expected_size += 1;
    let service = start_gateway_certificate_service(
        nats.machine_client(&machine_id("machine_7")).await,
        machine_id("machine_7"),
        GatewayCertificateStore::new(state.path().to_path_buf()),
        PingoraRouteRegistry::new(),
    )
    .await
    .expect("gateway certificate service");

    let response: CertificateArtifactPushResponse = request_json(
        &nats.controller,
        machine_service(
            &machine_id("machine_7"),
            MachineServiceEndpoint::CertificateArtifactPush,
        ),
        &request,
        Duration::from_secs(1),
    )
    .await
    .expect("artifact push response");

    assert!(matches!(
        response,
        MachineRpcResponse::DomainError {
            machine_id: response_machine_id,
            error: GatewayCertificateRpcError::InvalidRequest { .. },
        } if response_machine_id == machine_id("machine_7")
    ));
    service.shutdown().await.expect("service shutdown");
}

#[tokio::test]
async fn challenge_status_endpoint_reports_only_applied_registry_snapshot() {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_7")]).await;
    let state = tempfile::tempdir().expect("state directory");
    let registry = PingoraRouteRegistry::new();
    let applied_challenge = challenge("app.example.com", "token");
    let service = start_gateway_certificate_service(
        nats.machine_client(&machine_id("machine_7")).await,
        machine_id("machine_7"),
        GatewayCertificateStore::new(state.path().to_path_buf()),
        registry.clone(),
    )
    .await
    .expect("gateway certificate service");

    assert_eq!(
        challenge_status(&nats.controller, applied_challenge.clone()).await,
        CertificateChallengeApplicationStatus::NotApplied
    );
    registry.apply_challenge(&applied_challenge);
    assert_eq!(
        challenge_status(&nats.controller, applied_challenge).await,
        CertificateChallengeApplicationStatus::Applied
    );
    assert_eq!(
        challenge_status(&nats.controller, challenge("app.example.com", "stale")).await,
        CertificateChallengeApplicationStatus::NotApplied
    );
    let different_value = AcmeHttp01Challenge::try_new(
        route_hostname("app.example.com"),
        AcmeChallengeToken::try_new("token").expect("token"),
        AcmeChallengeValue::try_new("token.different-thumbprint").expect("value"),
        AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
    )
    .expect("challenge");
    assert_eq!(
        challenge_status(&nats.controller, different_value).await,
        CertificateChallengeApplicationStatus::NotApplied
    );
    service.shutdown().await.expect("service shutdown");
}

async fn challenge_status(
    client: &async_nats::Client,
    challenge: AcmeHttp01Challenge,
) -> CertificateChallengeApplicationStatus {
    let response: CertificateChallengeStatusResponse = request_json(
        client,
        machine_service(
            &machine_id("machine_7"),
            MachineServiceEndpoint::CertificateChallengeStatus,
        ),
        &CertificateChallengeStatusRequest { challenge },
        Duration::from_secs(1),
    )
    .await
    .expect("challenge status response");
    let MachineRpcResponse::Ok(ok) = response else {
        panic!("challenge status returns testimony")
    };
    ok.application
}

fn artifact_request(hostname: &str, core_path: &Path) -> CertificateArtifactPushRequest {
    let (certificate, private_key) = certificate_material(hostname, NOT_BEFORE, NOT_AFTER);
    request_from_material(
        hostname,
        core_path,
        certificate,
        private_key,
        NOT_BEFORE,
        NOT_AFTER,
    )
}

fn route_binding_id() -> RouteBindingId {
    RouteBindingId::try_new("route_app").expect("valid route binding id")
}

fn route_owner() -> CertificateOwner {
    CertificateOwner::RouteBinding {
        route_binding_id: route_binding_id(),
    }
}

fn request_from_material(
    hostname: &str,
    core_path: &Path,
    certificate_chain_pem: String,
    private_key_pem: String,
    not_before: u64,
    not_after: u64,
) -> CertificateArtifactPushRequest {
    let digest = custom_bundle_digest(&certificate_chain_pem, &private_key_pem).expect("digest");
    let path = AbsoluteInstallPath::try_new(
        core_path
            .to_str()
            .expect("fixture core path is valid UTF-8"),
    )
    .expect("absolute core path");
    let bundle = CustomCertBundle::try_new(
        ActiveCertState {
            cert_id: cert_id("cert_app_example_com"),
            hostname: route_hostname(hostname),
            bundle_ref: CertBundleRef::for_bundle(&digest, &path).expect("bundle reference"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(not_before).expect("not before"),
                CertValidAt::try_new(not_after).expect("not after"),
            )
            .expect("validity"),
        },
        certificate_chain_pem,
        private_key_pem,
    )
    .expect("validated bundle");
    CertificateArtifactPushRequest {
        operation_id: operation_id("op_cert"),
        expected_digest: digest,
        expected_size: u64::try_from(bundle.material_bytes().len()).expect("bundle size"),
        bundle,
    }
}

fn certificate_material(hostname: &str, not_before: u64, not_after: u64) -> (String, String) {
    let mut params = CertificateParams::new(vec![hostname.to_owned()]).expect("certificate params");
    params.not_before =
        OffsetDateTime::from_unix_timestamp(i64::try_from(not_before).expect("not before fits"))
            .expect("not before timestamp");
    params.not_after =
        OffsetDateTime::from_unix_timestamp(i64::try_from(not_after).expect("not after fits"))
            .expect("not after timestamp");
    let key = KeyPair::generate().expect("private key");
    let certificate = params.self_signed(&key).expect("certificate");
    (certificate.pem(), key.serialize_pem())
}

fn challenge(hostname: &str, token: &str) -> AcmeHttp01Challenge {
    AcmeHttp01Challenge::try_new(
        route_hostname(hostname),
        AcmeChallengeToken::try_new(token).expect("token"),
        AcmeChallengeValue::try_new(format!("{token}.account-thumbprint")).expect("value"),
        AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
    )
    .expect("challenge")
}
