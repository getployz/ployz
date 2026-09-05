//! Envoy renderer and apply-lifecycle contract tests.

use super::{
    Error as RendererError,
    apply::{ApplyIo, ApplyOutcome, Error as ApplyError, ValidationOutcome, apply},
    render, write_initial_config,
};
use crate::corrosion::CertificateChallenge;
use crate::ingress::{certificate_file_stem, tests::renderer_projection};
use ployz_core::{
    ContainerAddress, ENVOY_RUNTIME_GID, IngressProxyFragment, MachineId, QualifiedService,
};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    sync::Mutex,
};

const SELECTED_IMAGE: &str = "example.invalid/envoy@sha256:exact";

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
fn shared_projection_matches_the_frozen_envoy_contract() {
    let rendered = render(&renderer_projection()).unwrap();

    assert_eq!(
        rendered.lds(),
        include_str!("envoy_tests/renderer_projection.lds.yaml")
    );
    assert_eq!(
        rendered.rds(),
        include_str!("envoy_tests/renderer_projection.rds.yaml")
    );
    assert_eq!(
        rendered.cds(),
        include_str!("envoy_tests/renderer_projection.cds.yaml")
    );
    assert_eq!(
        rendered.sds(),
        include_str!("envoy_tests/renderer_projection.sds.yaml")
    );
    assert_eq!(
        rendered.digest(),
        "e3314422cbb7e109161ad18864b183fa14805f688832d117b9ea81d74fe624c8"
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
        address: ContainerAddress("10.210.3.2".parse().unwrap()),
        port: std::num::NonZeroU16::new(8080).unwrap(),
    }]);

    let second = render(&projection).unwrap();

    assert!(second.cds().contains("address: 10.210.3.2"));
    assert!(!second.cds().contains("address: 10.210.1.2"));
    assert!(!second.cds().contains("address: 10.210.2.2"));
    assert_ne!(second.digest(), first.digest());
}

#[test]
fn http_and_https_routes_carry_timeouts_secrets_and_challenge_direct_responses() {
    let rendered = render(&renderer_projection()).unwrap();

    assert!(rendered.rds().contains("cluster: ployz-http-example.com"));
    assert!(
        rendered
            .rds()
            .contains("cluster: ployz-https-secure.example.com")
    );
    assert!(rendered.rds().contains("timeout: 0s"));
    assert!(rendered.cds().contains("connect_timeout: 5s"));
    assert!(rendered.cds().contains("ployz-https-secure.example.com"));
    assert!(rendered.cds().contains("address: 10.210.1.3"));
    assert!(rendered.cds().contains("port_value: 8443"));
    assert!(rendered.lds().contains("port_value: 8443"));
    assert!(rendered.lds().contains("server_names:"));
    assert!(rendered.lds().contains("secure.example.com"));
    assert!(rendered.sds().contains(
        "filename: /config/certs/secure.example.com-b1f749c1ccf5ec27eadc3e24da1e2da92c902bd706dff8d418cdefd439405aa0.crt"
    ));
    assert!(rendered.sds().contains(
        "filename: /config/certs/secure.example.com-b1f749c1ccf5ec27eadc3e24da1e2da92c902bd706dff8d418cdefd439405aa0.key"
    ));
    assert!(rendered.rds().contains("acme-challenge"));
    assert!(rendered.rds().contains("token.thumbprint"));
    assert!(rendered.cds().contains("address: 127.0.0.1"));
    assert!(rendered.cds().contains("port_value: 1"));
    assert!(!rendered.lds().contains("admin:"));
    assert!(
        rendered
            .lds()
            .contains(&format!("version_info: \"{}\"", rendered.digest()))
    );
    let proxy_routes = rendered.rds().matches("cluster: ployz-http-").count()
        + rendered.rds().matches("cluster: ployz-https-").count();
    assert_eq!(rendered.rds().matches("timeout: 0s").count(), proxy_routes);
}

#[test]
fn https_without_material_is_not_rendered() {
    let mut projection = renderer_projection();
    projection
        .sites
        .iter_mut()
        .find(|site| site.hostname.as_str() == "secure.example.com")
        .unwrap()
        .certificate
        .as_mut()
        .unwrap()
        .material = None;
    let rendered = render(&projection).unwrap();

    assert!(!rendered.lds().contains("port_value: 8443"));
    assert!(!rendered.lds().contains("ployz-https"));
    assert!(!rendered.rds().contains("ployz-https-"));
    assert!(!rendered.cds().contains("ployz-https-"));
    assert!(!rendered.sds().contains("secure.example.com"));
    assert!(rendered.sds().contains("resources: []"));
}

#[test]
fn http01_challenge_is_served_without_https_material_or_http_publication() {
    let mut projection = renderer_projection();
    let site = projection
        .sites
        .iter_mut()
        .find(|site| site.hostname.as_str() == "secure.example.com")
        .unwrap();
    site.certificate.as_mut().unwrap().challenge =
        CertificateChallenge::new("issuance", "issuance.thumbprint");
    site.certificate.as_mut().unwrap().material = None;
    let rendered = render(&projection).unwrap();

    assert!(
        rendered
            .rds()
            .contains("/.well-known/acme-challenge/issuance")
    );
    assert!(rendered.rds().contains("issuance.thumbprint"));
    assert!(rendered.rds().contains("ployz-http-secure.example.com"));
    assert!(
        !rendered
            .rds()
            .contains("cluster: ployz-http-secure.example.com")
    );
    assert!(!rendered.lds().contains("port_value: 8443"));
    assert!(!rendered.cds().contains("ployz-https-"));
    assert!(!rendered.sds().contains("secure.example.com"));
}

#[test]
fn write_initial_config_is_idempotent_and_installs_file_watched_xds() {
    let root = test_root("bootstrap");
    let config_file = root.join("ingress/envoy/bootstrap.yaml");
    let machine = renderer_projection().machine;
    write_initial_config(&machine, &config_file).unwrap();
    let bootstrap = fs::read_to_string(&config_file).unwrap();
    assert_eq!(bootstrap, super::BOOTSTRAP);
    assert!(!bootstrap.contains("admin:"));
    assert!(bootstrap.contains("path_config_source:"));
    let rds = fs::read_to_string(config_file.parent().unwrap().join("rds.yaml")).unwrap();
    assert!(rds.contains("/.ployz-verify"));
    assert!(rds.contains(&machine.id.to_string()));
    let cds = fs::read_to_string(config_file.parent().unwrap().join("cds.yaml")).unwrap();
    assert!(cds.contains("resources: []"));
    assert!(cds.contains("version_info:"));
    let sds = fs::read_to_string(config_file.parent().unwrap().join("sds.yaml")).unwrap();
    assert!(sds.contains("resources: []"));
    assert!(sds.contains("version_info:"));
    fs::write(&config_file, "authoritative\n").unwrap();
    write_initial_config(&machine, &config_file).unwrap();
    assert_eq!(fs::read_to_string(&config_file).unwrap(), "authoritative\n");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn read_generated_config_errors_when_an_xds_file_is_missing() {
    let root = test_root("missing-sds");
    let generated = root.join("ingress").join("envoy");
    fs::create_dir_all(&generated).unwrap();
    fs::write(generated.join("lds.yaml"), "lds: true\n").unwrap();
    fs::write(generated.join("rds.yaml"), "rds: true\n").unwrap();
    fs::write(generated.join("cds.yaml"), "cds: true\n").unwrap();
    let error = super::read_generated_config(&root).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(!generated.join("sds.yaml").exists());
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn rejected_candidate_leaves_live_configuration_untouched() {
    let root = test_root("rejected");
    let live = root.join("bootstrap.yaml");
    fs::create_dir_all(&root).unwrap();
    fs::write(&live, "live-bootstrap\n").unwrap();
    fs::write(root.join("lds.yaml"), "live-lds\n").unwrap();
    fs::write(root.join("rds.yaml"), "live-rds\n").unwrap();
    fs::write(root.join("cds.yaml"), "live-cds\n").unwrap();
    fs::write(root.join("sds.yaml"), "live-sds\n").unwrap();
    fs::create_dir_all(root.join("certs")).unwrap();
    fs::write(root.join("certs/old.crt"), "OLD").unwrap();
    fs::write(root.join("certs/old.key"), "OLD").unwrap();
    let io = FakeIo::rejecting("schema rejected");

    let error = apply(&renderer_projection(), &live, SELECTED_IMAGE, &io)
        .await
        .unwrap_err();

    let expected_digest = render(&renderer_projection()).unwrap().digest().to_owned();
    assert!(matches!(
        error,
        ApplyError::ValidationRejected { ref digest, .. } if digest == &expected_digest
    ));
    assert_eq!(fs::read_to_string(&live).unwrap(), "live-bootstrap\n");
    assert_eq!(
        fs::read_to_string(root.join("lds.yaml")).unwrap(),
        "live-lds\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("rds.yaml")).unwrap(),
        "live-rds\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("cds.yaml")).unwrap(),
        "live-cds\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("sds.yaml")).unwrap(),
        "live-sds\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("certs/old.crt")).unwrap(),
        "OLD"
    );
    assert_eq!(
        fs::read_to_string(root.join("certs/old.key")).unwrap(),
        "OLD"
    );
    assert_eq!(fs::read_dir(root.join("certs")).unwrap().count(), 2);
    assert!(!root.join(".apply-candidate").exists());
    let validations = io.validations.lock().unwrap();
    let (image, candidate) = validations.first().unwrap();
    assert_eq!(image, SELECTED_IMAGE);
    assert!(candidate.contains(&format!("projection_digest: {expected_digest}")));
    assert!(candidate.contains("timeout: 0s"));
    assert!(candidate.contains("acme-challenge"));
    assert!(candidate.contains("filename: /config/certs/secure.example.com-"));
    assert!(candidate.contains(".crt"));
    assert!(candidate.contains(".key"));

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn accepted_candidate_is_published_without_claiming_live_adoption() {
    let root = test_root("published");
    let live = root.join("bootstrap.yaml");
    let projection = renderer_projection();
    let rendered = render(&projection).unwrap();
    let digest = rendered.digest().to_owned();
    let io = FakeIo::accepting();

    let outcome = apply(&projection, &live, SELECTED_IMAGE, &io)
        .await
        .unwrap();

    assert_eq!(outcome, ApplyOutcome::Published { digest });
    assert_eq!(
        fs::read_to_string(root.join("lds.yaml")).unwrap(),
        rendered.lds()
    );
    assert_eq!(
        fs::read_to_string(root.join("rds.yaml")).unwrap(),
        rendered.rds()
    );
    assert_eq!(
        fs::read_to_string(root.join("cds.yaml")).unwrap(),
        rendered.cds()
    );
    assert_eq!(
        fs::read_to_string(root.join("sds.yaml")).unwrap(),
        rendered.sds()
    );
    assert!(!root.join(".apply-candidate").exists());

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn accepted_candidate_writes_certificates_and_removes_stale_files() {
    let root = test_root("certs");
    let live = root.join("bootstrap.yaml");
    let certs = root.join("certs");
    fs::create_dir_all(&certs).unwrap();
    fs::write(certs.join("stale.crt"), "OLD").unwrap();
    fs::write(certs.join("stale.key"), "OLD").unwrap();
    let projection = renderer_projection();
    let io = FakeIo::accepting();

    apply(&projection, &live, SELECTED_IMAGE, &io)
        .await
        .unwrap();

    let site = projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "secure.example.com")
        .unwrap();
    let stem = certificate_file_stem(&site.hostname, site.material().unwrap());
    let cert_path = certs.join(format!("{stem}.crt"));
    let key_path = certs.join(format!("{stem}.key"));
    assert_eq!(
        fs::read_to_string(&cert_path).unwrap(),
        site.material().unwrap().certificate()
    );
    assert_eq!(
        fs::read_to_string(&key_path).unwrap(),
        site.material().unwrap().private_key()
    );
    assert_eq!(
        fs::metadata(&certs).unwrap().permissions().mode() & 0o777,
        0o750
    );
    assert_eq!(
        fs::metadata(&cert_path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    let key_metadata = fs::metadata(&key_path).unwrap();
    assert_eq!(key_metadata.permissions().mode() & 0o777, 0o640);
    if fs::metadata("/proc/self").unwrap().uid() == 0 {
        assert_eq!(key_metadata.gid(), ENVOY_RUNTIME_GID);
    }
    assert!(!certs.join("stale.crt").exists());
    assert!(!certs.join("stale.key").exists());

    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn docker_validation_errors_keep_their_typed_phase() {
    let projection = renderer_projection();
    let validation_root = test_root("validation-error");
    let validation_live = validation_root.join("bootstrap.yaml");
    let mut validation_io = FakeIo::accepting();
    validation_io.validation_error = true;

    let validation_error = apply(
        &projection,
        &validation_live,
        SELECTED_IMAGE,
        &validation_io,
    )
    .await
    .unwrap_err();
    assert!(matches!(validation_error, ApplyError::Validation(_)));
    assert!(!validation_root.join("lds.yaml").exists());

    fs::remove_dir_all(validation_root).unwrap();
}

struct FakeIo {
    validation: ValidationOutcome,
    validations: Mutex<Vec<(String, String)>>,
    validation_error: bool,
}

impl FakeIo {
    fn accepting() -> Self {
        Self {
            validation: ValidationOutcome::Accepted,
            validations: Mutex::default(),
            validation_error: false,
        }
    }

    fn rejecting(reason: &str) -> Self {
        Self {
            validation: ValidationOutcome::Rejected(reason.to_owned()),
            validations: Mutex::default(),
            validation_error: false,
        }
    }
}

impl ApplyIo for FakeIo {
    async fn validate_candidate(
        &self,
        image: &str,
        candidate: &Path,
    ) -> Result<ValidationOutcome, ApplyError> {
        self.validations.lock().unwrap().push((
            image.to_owned(),
            [
                fs::read_to_string(candidate.join("rds.yaml")).unwrap(),
                fs::read_to_string(candidate.join("sds.yaml")).unwrap(),
                candidate_certificate_names(candidate),
            ]
            .join("\n"),
        ));
        if self.validation_error {
            return Err(ApplyError::Validation(
                crate::docker::Error::InvalidContainerConfig("test validation failure".into()),
            ));
        }
        Ok(self.validation.clone())
    }
}

fn candidate_certificate_names(candidate: &Path) -> String {
    let certs = candidate.join("certs");
    if !certs.exists() {
        return String::new();
    }
    let mut names: Vec<_> = fs::read_dir(certs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.join("\n")
}

fn test_root(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ployz-envoy-apply-{label}-{}", MachineId::random()))
}
