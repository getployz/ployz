use std::path::Path;
use std::time::Duration;

use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, CertificateArtifactKind,
    CertificateArtifactPushOutcome, CertificateArtifactPushRequest,
    CertificateArtifactPushResponse, CertificateChallengeApplicationStatus,
    CertificateChallengeStatusRequest, CertificateChallengeStatusResponse, CustomCertBundle,
    GatewayCertificateRpcError, custom_bundle_digest,
};
use ployz_core::install::{AbsoluteInstallPath, InstallSha256Digest};
use ployz_core::machine_rpc::MachineRpcResponse;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::service_runtime::request_json;
use ployz_nats::services::EndpointExecution;
use ployz_test_support::ids::{cert_id, machine_id, operation_id, route_hostname};
use ployzd::roles::gateway::pingora::PingoraRouteRegistry;
use ployzd::roles::gateway::process::start_gateway_certificate_service;
use ployzd::roles::gateway::projection::GatewayProjection;
use ployzd::roles::gateway::source::{GatewayCertificateStore, GatewayCertificateStoreError};
use ployzd::service_catalog::{DaemonServiceCatalog, gateway_role_service};
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
                "machine.certificate.challenge.status",
                "plz.v1.rpc.machine.query.machine_7.certificate.challenge.status",
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

    assert_eq!(
        store.push_at(&request, NOW),
        Ok(CertificateArtifactPushOutcome::Stored)
    );
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

    assert_eq!(
        store.push_at(&request, NOW),
        Ok(CertificateArtifactPushOutcome::AlreadyPresent)
    );
}

#[test]
fn stored_artifact_is_adopted_from_active_metadata_after_restart() {
    let state = tempfile::tempdir().expect("state directory");
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    GatewayCertificateStore::new(state.path().to_path_buf())
        .push_at(&request, NOW)
        .expect("initial push");

    assert_eq!(
        GatewayCertificateStore::new(state.path().to_path_buf())
            .load_at(request.bundle.active_cert(), NOW),
        Ok(request.bundle)
    );
}

#[test]
fn unknown_artifact_kind_is_rejected_by_the_wire_contract() {
    let request = artifact_request("app.example.com", Path::new("/core/owned.bundle"));
    let mut encoded = serde_json::to_value(request).expect("serialize request");
    encoded["artifact_kind"] = serde_json::json!("unknown");

    assert!(serde_json::from_value::<CertificateArtifactPushRequest>(encoded).is_err());
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
                && ok.outcome == CertificateArtifactPushOutcome::Stored
    ));
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
    registry
        .replace_projection(&GatewayProjection {
            managed_cert_bundle: None,
            custom_cert_bundles: Vec::new(),
            challenges: vec![applied_challenge.clone()],
            routes: Vec::new(),
        })
        .expect("apply challenge projection");
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
        artifact_kind: CertificateArtifactKind::CustomTlsBundle,
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
