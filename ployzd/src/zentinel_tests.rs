//! Zentinel renderer, support-file, and apply-lifecycle contract tests.

use super::{
    Error as RendererError, ZENTINEL_BOOTSTRAP_CERT_FILE, ZENTINEL_BOOTSTRAP_KEY_FILE,
    ZENTINEL_CHALLENGES_DIR, ZENTINEL_GID, ZENTINEL_VERIFY_DIR, render, set_group,
    write_initial_config, write_support_files,
};
use crate::{
    corrosion::CertificateChallenge,
    ingress::zentinel::apply::{
        ApplyIo, ApplyOutcome, Error as ApplyError, ValidationOutcome, active_digest, apply,
    },
    ingress::{certificate_file_stem, tests::renderer_projection},
};
use ployz_core::{
    ContainerId, INGRESS_VERIFY_PATH, IngressProxyFragment, MachineId, QualifiedService,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    sync::Mutex,
};

const SELECTED_IMAGE: &str = "example.invalid/zentinel@sha256:exact";

#[test]
fn caddy_fragment_is_a_typed_backend_mismatch() {
    let mut projection = renderer_projection();
    projection.service_fragments.insert(
        QualifiedService::parse("app/api").unwrap(),
        IngressProxyFragment::parse_caddy("respond ok").unwrap(),
    );

    assert!(matches!(
        render(&projection),
        Err(RendererError::BackendMismatch)
    ));
}

#[test]
fn shared_projection_matches_the_frozen_zentinel_contract() {
    let rendered = render(&renderer_projection()).unwrap();

    assert_eq!(
        rendered.kdl(),
        include_str!("zentinel_tests/renderer_projection.kdl")
    );
    assert_eq!(
        rendered.digest(),
        "64da812043281f6839f0fdbe8bb7ff63dff071847a87af60089bb010426ee231"
    );
}

#[test]
fn changed_projection_fully_regenerates_static_targets() {
    let mut projection = renderer_projection();
    let first = render(&projection).unwrap();
    let site = projection
        .sites
        .iter_mut()
        .find(|site| site.hostname.as_str() == "example.com")
        .unwrap();
    site.publication.as_mut().unwrap().http = Some(vec![crate::ingress::IngressEndpoint {
        address: ployz_core::ContainerAddress("10.210.3.2".parse().unwrap()),
        port: std::num::NonZeroU16::new(8080).unwrap(),
    }]);

    let second = render(&projection).unwrap();

    assert!(second.kdl().contains("target \"10.210.3.2:8080\""));
    assert!(!second.kdl().contains("target \"10.210.1.2:8080\""));
    assert!(!second.kdl().contains("target \"10.210.2.2:8080\""));
    assert_ne!(second.digest(), first.digest());
}

#[test]
fn verification_route_returns_the_local_machine_identity() {
    let root = test_root("verification");
    let config_file = root.join("zentinel.kdl");
    let projection = renderer_projection();

    write_support_files(&projection, &config_file).unwrap();
    let rendered = render(&projection).unwrap();

    assert!(rendered.kdl().contains("route \"ployz-verify\""));
    assert!(
        rendered
            .kdl()
            .contains(&format!("path {INGRESS_VERIFY_PATH:?}"))
    );
    assert_eq!(
        fs::read_to_string(
            root.join(ZENTINEL_VERIFY_DIR)
                .join(INGRESS_VERIFY_PATH.trim_start_matches('/'))
        )
        .unwrap(),
        projection.machine.id.to_string()
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn support_files_are_stable_valid_and_readable_by_the_container() {
    let root = std::env::temp_dir().join(format!(
        "ployz-zentinel-support-test-{}",
        MachineId::random()
    ));
    let config_file = root.join("zentinel.kdl");
    let projection = renderer_projection();

    write_support_files(&projection, &config_file).unwrap();

    let certificates = root.join("certs");
    let bootstrap_certificate = certificates.join(ZENTINEL_BOOTSTRAP_CERT_FILE);
    let bootstrap_key = certificates.join(ZENTINEL_BOOTSTRAP_KEY_FILE);
    let certificate = fs::read_to_string(&bootstrap_certificate).unwrap();
    let key = fs::read_to_string(&bootstrap_key).unwrap();
    assert!(
        crate::corrosion::CertificateMaterial::new(certificate.as_str(), key.as_str()).is_some()
    );
    assert_eq!(mode(&certificates), 0o750);
    assert_eq!(mode(&bootstrap_certificate), 0o644);
    assert_eq!(mode(&bootstrap_key), 0o640);

    let secure_site = projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "secure.example.com")
        .unwrap();
    let material = secure_site.material().unwrap();
    let stem = certificate_file_stem(&secure_site.hostname, material);
    assert_eq!(mode(&certificates.join(format!("{stem}.crt"))), 0o644);
    assert_eq!(mode(&certificates.join(format!("{stem}.key"))), 0o640);

    let challenge = root
        .join(ZENTINEL_CHALLENGES_DIR)
        .join("example.com/.well-known/acme-challenge/token");
    assert_eq!(fs::read_to_string(&challenge).unwrap(), "token.thumbprint");
    assert_eq!(mode(&challenge), 0o644);
    assert_eq!(mode(&root.join(ZENTINEL_CHALLENGES_DIR)), 0o750);
    assert_eq!(mode(&root.join("challenges/example.com")), 0o750);
    assert_eq!(
        mode(&root.join("challenges/example.com/.well-known")),
        0o750
    );
    assert_eq!(
        mode(&root.join("challenges/example.com/.well-known/acme-challenge")),
        0o750
    );

    if fs::metadata("/proc/self").unwrap().uid() == 0 {
        assert_eq!(fs::metadata(&bootstrap_key).unwrap().gid(), ZENTINEL_GID);
        assert_eq!(fs::metadata(&challenge).unwrap().gid(), ZENTINEL_GID);
    }

    write_support_files(&projection, &config_file).unwrap();
    assert_eq!(
        fs::read_to_string(&bootstrap_certificate).unwrap(),
        certificate
    );

    fs::remove_file(&bootstrap_key).unwrap();
    write_support_files(&projection, &config_file).unwrap();
    assert!(
        crate::corrosion::CertificateMaterial::new(
            fs::read_to_string(&bootstrap_certificate).unwrap(),
            fs::read_to_string(&bootstrap_key).unwrap()
        )
        .is_some()
    );

    fs::write(&bootstrap_key, "not a private key").unwrap();
    let error = write_support_files(&projection, &config_file).unwrap_err();
    assert!(matches!(error, RendererError::InvalidBootstrapPair));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn initial_config_reports_an_unwritable_parent() {
    let root = std::env::temp_dir().join(format!(
        "ployz-zentinel-initial-config-test-{}",
        MachineId::random()
    ));
    fs::write(&root, "not a directory").unwrap();
    let projection = renderer_projection();

    let error = write_initial_config(&projection.machine, &root.join("zentinel.kdl")).unwrap_err();

    assert!(matches!(error, RendererError::Filesystem(_)));
    fs::remove_file(root).unwrap();
}

#[test]
fn support_files_reject_a_challenge_path() {
    let root = std::env::temp_dir().join(format!(
        "ployz-zentinel-challenge-path-test-{}",
        MachineId::random()
    ));
    let mut projection = renderer_projection();
    projection
        .sites
        .iter_mut()
        .find(|site| site.hostname.as_str() == "example.com")
        .unwrap()
        .certificate
        .as_mut()
        .unwrap()
        .challenge = CertificateChallenge::new("../token", "response");

    let error = write_support_files(&projection, &root.join("zentinel.kdl")).unwrap_err();

    assert!(matches!(error, RendererError::InvalidChallengeToken));
    assert!(!root.exists());
}

#[test]
fn zentinel_group_assignment_rejects_a_missing_path() {
    let missing = test_root("missing-group-path");
    assert!(set_group(&missing).is_err());
}

#[tokio::test]
async fn rejected_candidate_leaves_live_configuration_and_proxy_untouched() {
    let root = test_root("rejected");
    let live = root.join("zentinel.kdl");
    fs::create_dir_all(&root).unwrap();
    fs::write(&live, "live configuration").unwrap();
    let io = FakeIo::rejecting("schema rejected");
    let container = container_id();

    let error = apply(
        &renderer_projection(),
        &live,
        SELECTED_IMAGE,
        &container,
        &io,
    )
    .await
    .unwrap_err();

    let expected_digest = render(&renderer_projection()).unwrap().digest().to_owned();
    assert!(matches!(
        error,
        ApplyError::ValidationRejected { ref digest, .. } if digest == &expected_digest
    ));
    assert_eq!(fs::read_to_string(&live).unwrap(), "live configuration");
    assert!(!root.join("certs").exists());
    assert!(io.signals.lock().unwrap().is_empty());
    let validations = io.validations.lock().unwrap();
    let (image, candidate) = validations.first().unwrap();
    assert_eq!(image, SELECTED_IMAGE);
    assert!(candidate.contains(&format!("ployz-admin-{expected_digest}")));
    assert_eq!(*io.admin_calls.lock().unwrap(), 0);

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn accepted_candidate_is_activated_signalled_and_confirmed() {
    let root = test_root("confirmed");
    let live = root.join("zentinel.kdl");
    let projection = renderer_projection();
    let digest = render(&projection).unwrap().digest().to_owned();
    let io = FakeIo::accepting([Some("0".repeat(64)), Some(digest.clone())]);
    let container = container_id();

    let outcome = apply(&projection, &live, SELECTED_IMAGE, &container, &io)
        .await
        .unwrap();

    assert_eq!(outcome, ApplyOutcome::Confirmed { digest });
    assert_eq!(
        fs::read_to_string(&live).unwrap(),
        render(&projection).unwrap().kdl()
    );
    assert_eq!(io.signals.lock().unwrap().as_slice(), &[container]);
    assert!(!live.with_extension("candidate.kdl").exists());
    assert!(root.join("certs").exists());

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn activation_failure_removes_the_unactivated_candidate() {
    let root = test_root("activation-error");
    let live = root.join("zentinel.kdl");
    fs::create_dir_all(&live).unwrap();
    let io = FakeIo::accepting([]);

    let error = apply(
        &renderer_projection(),
        &live,
        SELECTED_IMAGE,
        &container_id(),
        &io,
    )
    .await
    .unwrap_err();

    assert!(matches!(error, ApplyError::Filesystem(_)));
    assert!(!live.with_extension("candidate.kdl").exists());
    assert!(io.signals.lock().unwrap().is_empty());

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(start_paused = true)]
async fn confirmation_timeout_reports_the_last_observed_digest() {
    let root = test_root("timeout");
    let live = root.join("zentinel.kdl");
    let projection = renderer_projection();
    let digest = render(&projection).unwrap().digest().to_owned();
    let observed = "1".repeat(64);
    let io = FakeIo::accepting([Some(observed.clone())]).stall_after(1);
    let container = container_id();

    let outcome = apply(&projection, &live, SELECTED_IMAGE, &container, &io)
        .await
        .unwrap();

    assert_eq!(
        outcome,
        ApplyOutcome::ReloadUnconfirmed {
            digest,
            last_observed_digest: Some(observed),
        }
    );
    assert_eq!(io.signals.lock().unwrap().as_slice(), &[container]);

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn docker_execution_and_reload_errors_keep_their_typed_phase() {
    let projection = renderer_projection();
    let container = container_id();
    let validation_root = test_root("validation-error");
    let validation_live = validation_root.join("zentinel.kdl");
    let mut validation_io = FakeIo::accepting([]);
    validation_io.validation_error = true;

    let validation_error = apply(
        &projection,
        &validation_live,
        SELECTED_IMAGE,
        &container,
        &validation_io,
    )
    .await
    .unwrap_err();
    assert!(matches!(validation_error, ApplyError::Validation(_)));
    assert!(!validation_live.exists());
    assert!(validation_io.signals.lock().unwrap().is_empty());

    let reload_root = test_root("reload-error");
    let reload_live = reload_root.join("zentinel.kdl");
    let mut reload_io = FakeIo::accepting([]);
    reload_io.reload_error = true;
    let reload_error = apply(
        &projection,
        &reload_live,
        SELECTED_IMAGE,
        &container,
        &reload_io,
    )
    .await
    .unwrap_err();
    assert!(matches!(reload_error, ApplyError::Reload(_)));
    assert!(reload_live.exists());
    assert_eq!(*reload_io.admin_calls.lock().unwrap(), 0);

    fs::remove_dir_all(validation_root).unwrap();
    fs::remove_dir_all(reload_root).unwrap();
}

#[test]
fn active_config_digest_accepts_only_the_digest_listener() {
    let digest = "a".repeat(64);
    let response = serde_json::json!({
        "config": {
            "listeners": [
                { "id": "ployz-http" },
                { "id": format!("ployz-admin-{digest}") }
            ]
        }
    });
    assert_eq!(active_digest(&response.to_string()).unwrap(), Some(digest));

    let malformed = serde_json::json!({
        "config": { "listeners": [{ "id": "ployz-admin-not-a-digest" }] }
    });
    assert_eq!(active_digest(&malformed.to_string()).unwrap(), None);
    assert!(active_digest("{}").is_err());
}

struct FakeIo {
    validation: ValidationOutcome,
    validations: Mutex<Vec<(String, String)>>,
    signals: Mutex<Vec<ContainerId>>,
    digests: Vec<Option<String>>,
    admin_calls: Mutex<usize>,
    validation_error: bool,
    reload_error: bool,
    stall_after: Option<usize>,
}

impl FakeIo {
    fn accepting(digests: impl IntoIterator<Item = Option<String>>) -> Self {
        Self {
            validation: ValidationOutcome::Accepted,
            validations: Mutex::default(),
            signals: Mutex::default(),
            digests: digests.into_iter().collect(),
            admin_calls: Mutex::default(),
            validation_error: false,
            reload_error: false,
            stall_after: None,
        }
    }

    fn rejecting(reason: &str) -> Self {
        Self {
            validation: ValidationOutcome::Rejected(reason.to_owned()),
            validations: Mutex::default(),
            signals: Mutex::default(),
            digests: Vec::new(),
            admin_calls: Mutex::default(),
            validation_error: false,
            reload_error: false,
            stall_after: None,
        }
    }

    fn stall_after(mut self, calls: usize) -> Self {
        self.stall_after = Some(calls);
        self
    }
}

impl ApplyIo for FakeIo {
    async fn validate_candidate(
        &self,
        image: &str,
        candidate: &Path,
    ) -> Result<ValidationOutcome, ApplyError> {
        self.validations
            .lock()
            .unwrap()
            .push((image.to_owned(), fs::read_to_string(candidate).unwrap()));
        if self.validation_error {
            return Err(ApplyError::Validation(
                crate::docker::Error::InvalidContainerConfig("test validation failure".into()),
            ));
        }
        Ok(self.validation.clone())
    }

    async fn signal_reload(&self, container: &ContainerId) -> Result<(), ApplyError> {
        self.signals.lock().unwrap().push(*container);
        if self.reload_error {
            return Err(ApplyError::Reload(
                crate::docker::Error::InvalidContainerConfig("test signal failure".into()),
            ));
        }
        Ok(())
    }

    async fn active_digest(&self) -> Result<Option<String>, ApplyError> {
        let call = {
            let mut calls = self.admin_calls.lock().unwrap();
            let call = *calls;
            *calls += 1;
            call
        };
        if self.stall_after.is_some_and(|after| call >= after) {
            return std::future::pending().await;
        }
        let digest = self
            .digests
            .get(call)
            .or_else(|| self.digests.last())
            .cloned()
            .flatten();
        Ok(digest)
    }
}

fn test_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "ployz-zentinel-apply-{label}-{}",
        MachineId::random()
    ))
}

fn container_id() -> ContainerId {
    ContainerId::parse("f".repeat(64)).unwrap()
}

fn mode(path: &std::path::Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}
