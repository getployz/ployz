//! Deterministic Zentinel configuration rendering and support files.

use ployz_core::{
    ContainerId, ContainerObservation, HttpProtocol, INGRESS_VERIFY_PATH, IngressHost,
    IngressProxyBackend, Machine, ingress_proxy_backend,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    fs::File,
    io,
    os::unix::fs::{MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{CertificateMaterial, CertificateRow, ReplicatedStore},
    docker::LocalDocker,
    filesystem::atomic_write,
};

mod apply;

use super::{
    IngressEndpoint, IngressProjection, IngressSite, WatchApply, certificate_directory,
    certificate_file_stem, newest_local_ingress, watch as watch_inputs, write_certificate_files,
};

/// Host-private configuration-dump listener address.
pub(crate) const ADMIN_ADDRESS: &str = "127.0.0.1:2019";
pub(crate) const CONFIG_FILE: &str = "zentinel.kdl";
const CONTAINER_CERTS_DIR: &str = "/config/certs";
const CONTAINER_CONFIG_DIR: &str = "/config";
const ZENTINEL_BOOTSTRAP_CERT_FILE: &str = "ployz-bootstrap.crt";
const ZENTINEL_BOOTSTRAP_KEY_FILE: &str = "ployz-bootstrap.key";
const ZENTINEL_CHALLENGES_DIR: &str = "challenges";
const ZENTINEL_VERIFY_DIR: &str = "verify";
/// Numeric group used by the exact selected Zentinel image.
pub(crate) const ZENTINEL_GID: u32 = 65_532;

#[derive(Clone, Debug, Eq, PartialEq)]
struct IngressProcess {
    container_id: ContainerId,
    image: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WatchInput {
    projection: IngressProjection,
    process: Option<IngressProcess>,
}

impl WatchInput {
    fn derive(
        machine: &Machine,
        observations: &[ContainerObservation],
        certificates: &BTreeMap<IngressHost, CertificateRow>,
    ) -> Result<Self, ployz_core::IngressProxyServiceSpecError> {
        let process = newest_local_ingress(machine, observations)
            .map(|container| {
                let observation = container.as_observation();
                if ingress_proxy_backend(&observation.resolved_spec)?
                    != IngressProxyBackend::Zentinel
                {
                    return Err(ployz_core::IngressProxyServiceSpecError);
                }
                Ok(IngressProcess {
                    container_id: observation.container_id,
                    image: observation.resolved_spec.container.image.clone(),
                })
            })
            .transpose()?;
        Ok(Self {
            projection: IngressProjection::derive(machine, observations, certificates),
            process,
        })
    }
}

/// Watch the common projection and apply it through the selected Zentinel process.
pub(crate) async fn watch(
    machine: Machine,
    replicated: ReplicatedStore,
    config_file: PathBuf,
    docker: LocalDocker,
    shutdown: CancellationToken,
) -> io::Result<()> {
    replicated
        .require_ingress_proxy_backend(IngressProxyBackend::Zentinel)
        .await
        .map_err(io::Error::other)?;
    let http = reqwest::Client::new();
    watch_inputs(
        machine,
        replicated,
        shutdown,
        |machine, observations, certificates| {
            WatchInput::derive(machine, observations, certificates).map_err(io::Error::other)
        },
        async move |input| {
            let Some(process) = input.process.as_ref() else {
                return WatchApply::WaitForChange(io::Error::other(
                    "local Zentinel Ingress Proxy container is missing",
                ));
            };
            classify_apply(
                apply::apply(
                    &input.projection,
                    &config_file,
                    &process.image,
                    &process.container_id,
                    &(&docker, &http),
                )
                .await,
            )
        },
    )
    .await
}

fn classify_apply(result: Result<apply::ApplyOutcome, apply::Error>) -> WatchApply {
    match result {
        Ok(apply::ApplyOutcome::Confirmed { .. }) => WatchApply::Applied,
        Ok(apply::ApplyOutcome::ReloadUnconfirmed { .. }) => {
            WatchApply::WaitForChange(io::Error::other("Zentinel reload was not confirmed"))
        }
        Err(error @ apply::Error::ValidationRejected { .. }) => {
            WatchApply::WaitForChange(io::Error::other(error))
        }
        Err(error) => WatchApply::Retry(io::Error::other(error)),
    }
}

#[cfg(test)]
mod watch_tests {
    use super::apply::{ApplyOutcome, Error as ApplyError};
    use super::*;

    #[test]
    fn rejection_and_unconfirmed_reload_wait_for_changed_input() {
        assert!(matches!(
            classify_apply(Err(ApplyError::ValidationRejected {
                digest: "digest".into(),
                reason: "invalid".into(),
            })),
            WatchApply::WaitForChange(_)
        ));
        assert!(matches!(
            classify_apply(Ok(ApplyOutcome::ReloadUnconfirmed {
                digest: "digest".into(),
                last_observed_digest: None,
            })),
            WatchApply::WaitForChange(_)
        ));
    }

    #[test]
    fn transient_apply_error_retries() {
        assert!(matches!(
            classify_apply(Err(ApplyError::Filesystem(io::Error::other("temporary")))),
            WatchApply::Retry(_)
        ));
    }
}

#[must_use]
pub(crate) fn config_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("ingress").join("zentinel").join(CONFIG_FILE)
}

/// Install the generated configuration required before the first Zentinel process starts.
///
/// Existing configuration is authoritative and is only replaced by the validated apply path.
///
/// # Errors
///
/// Returns when the empty local projection cannot be rendered or durably written.
pub(crate) fn write_initial_config(machine: &Machine, config_file: &Path) -> Result<(), Error> {
    if config_file.try_exists()? {
        return Ok(());
    }
    let projection = IngressProjection::derive(machine, &[], &BTreeMap::new());
    let rendered = render(&projection)?;
    let parent = config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?;
    prepare_directory(parent)?;
    write_support_files(&projection, config_file)?;
    install_initial_config(config_file, rendered.kdl())?;
    Ok(())
}

fn install_initial_config(config_file: &Path, contents: &str) -> io::Result<()> {
    let candidate = config_file.with_extension("bootstrap.kdl");
    atomic_write(&candidate, contents.as_bytes(), 0o640)?;
    set_group(&candidate)?;
    let installed = match fs::hard_link(&candidate, config_file) {
        Ok(()) => File::open(
            config_file
                .parent()
                .expect("candidate and live path share a parent"),
        )?
        .sync_all(),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = fs::remove_file(&candidate) {
        tracing::warn!(path = %candidate.display(), %error, "failed to remove Zentinel bootstrap candidate");
    }
    installed
}

/// Rendered Zentinel configuration tied to one projection digest.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RenderedConfig {
    kdl: String,
    digest: String,
}

impl RenderedConfig {
    /// Exact KDL consumed by the pinned Zentinel binary.
    #[must_use]
    pub(crate) fn kdl(&self) -> &str {
        &self.kdl
    }

    /// Stable digest of the complete private Ingress Projection.
    #[must_use]
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

/// Failure while rendering or preparing Zentinel configuration.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// A tagged fragment belongs to another concrete backend.
    #[error("cannot render caddy Ingress Proxy Fragment with zentinel")]
    BackendMismatch,
    /// A certificate, key, directory, or challenge file operation failed.
    #[error("Zentinel ingress filesystem operation failed: {0}")]
    Filesystem(#[from] io::Error),
    /// The persisted bootstrap certificate is not paired with its private key.
    #[error("Zentinel bootstrap certificate and key do not match")]
    InvalidBootstrapPair,
    /// An HTTP-01 token could escape its hostname's challenge directory.
    #[error("HTTP-01 challenge token is not one path component")]
    InvalidChallengeToken,
}

/// Write and validate the filesystem inputs consumed by rendered Zentinel KDL.
///
/// # Errors
///
/// Returns when certificate or challenge material cannot be written, when a
/// challenge token is not one safe path component, or when an existing
/// bootstrap certificate pair is invalid.
pub(crate) fn write_support_files(
    projection: &IngressProjection,
    config_file: &Path,
) -> Result<(), Error> {
    validate_challenge_tokens(&projection.sites)?;
    write_certificate_files(
        config_file,
        &projection.sites,
        0o640,
        prepare_directory,
        set_group,
    )?;
    let certificates = certificate_directory(config_file)?;
    ensure_bootstrap(&certificates)?;
    write_challenges(config_file, &projection.sites)?;
    write_verification_file(config_file, &projection.machine)
}

/// Render one already-derived projection for Zentinel.
///
/// # Errors
///
/// Returns [`Error::BackendMismatch`] when the projection contains a fragment
/// for another concrete backend.
pub(crate) fn render(projection: &IngressProjection) -> Result<RenderedConfig, Error> {
    if projection.global_fragment.is_some() || !projection.service_fragments.is_empty() {
        return Err(Error::BackendMismatch);
    }
    let digest = projection_digest(projection);
    let mut kdl = format!(
        r#"schema-version "1.0"

system {{
    auto-reload #false
}}

listeners {{
    listener "ployz-http" {{
        address "0.0.0.0:80"
        protocol "http"
        request-timeout-secs 60
        keepalive-timeout-secs 75
    }}

    listener "ployz-https" {{
        address "0.0.0.0:443"
        protocol "https"
        namespace "ployz-https"
        request-timeout-secs 60
        keepalive-timeout-secs 75

        tls {{
            cert-file "{CONTAINER_CERTS_DIR}/{ZENTINEL_BOOTSTRAP_CERT_FILE}"
            key-file "{CONTAINER_CERTS_DIR}/{ZENTINEL_BOOTSTRAP_KEY_FILE}"
            min-version "TLS1.2"
"#
    );
    write_sni_certificates(&mut kdl, &projection.sites);
    let _ = write!(
        kdl,
        r#"        }}
    }}

    listener "ployz-admin-{digest}" {{
        address "{ADMIN_ADDRESS}"
        protocol "http"
        namespace "ployz-admin"
        request-timeout-secs 5
        keepalive-timeout-secs 5
    }}
}}

"#
    );
    write_global_configuration(&mut kdl, projection);
    write_https_namespace(&mut kdl, projection);
    kdl.push_str(
        r#"namespace "ployz-admin" {
    routes {
        route "ployz-config" {
            priority "critical"
            matches {
                path "/config"
            }
            service-type "builtin"
            builtin-handler "config"
        }
    }
}

observability {
    metrics {
        enabled #true
        address "127.0.0.1:9090"
        path "/metrics"
        high-cardinality #false
    }

    logging {
        level "info"
        format "json"

        access-log {
            enabled #true
            file "/dev/stdout"
            format "json"
        }

        error-log {
            enabled #false
        }
    }
}
"#,
    );
    Ok(RenderedConfig { kdl, digest })
}

/// Stable digest of the complete private Ingress Projection.
#[must_use]
pub(crate) fn projection_digest(projection: &IngressProjection) -> String {
    let canonical = serde_json::to_vec(projection)
        .expect("Ingress Projection contains only serializable value types");
    hex::encode(Sha256::digest(canonical))
}

fn write_sni_certificates(output: &mut String, sites: &[IngressSite]) {
    for site in sites {
        let Some(material) = site.material() else {
            continue;
        };
        if site.route(HttpProtocol::Https).is_none() {
            continue;
        }
        let stem = certificate_file_stem(&site.hostname, material);
        let _ = write!(
            output,
            concat!(
                "\n",
                "            sni {{\n",
                "                hostnames \"{}\"\n",
                "                cert-file \"{CONTAINER_CERTS_DIR}/{stem}.crt\"\n",
                "                key-file \"{CONTAINER_CERTS_DIR}/{stem}.key\"\n",
                "            }}\n",
            ),
            site.hostname,
            CONTAINER_CERTS_DIR = CONTAINER_CERTS_DIR,
            stem = stem,
        );
    }
}

fn write_global_configuration(output: &mut String, projection: &IngressProjection) {
    // Exact 26.08_5 initializes proxy/static registries only from top-level resources. Listener
    // namespaces still isolate HTTPS and admin routes, while their handlers consume global pools.
    output.push_str("routes {\n");
    write_verification_route(output);
    write_challenge_routes(output, &projection.sites, "    ");
    write_proxy_routes(output, &projection.sites, HttpProtocol::Http, "    ");
    output.push_str("}\n\nupstreams {\n");
    write_proxy_upstreams(output, &projection.sites, HttpProtocol::Http, "    ");
    write_proxy_upstreams(output, &projection.sites, HttpProtocol::Https, "    ");
    output.push_str("}\n\n");
}

fn write_verification_route(output: &mut String) {
    let _ = write!(
        output,
        concat!(
            "    route \"ployz-verify\" {{\n",
            "        priority \"critical\"\n",
            "        matches {{\n",
            "            path {path:?}\n",
            "        }}\n",
            "        static-files {{\n",
            "            root \"{config_dir}/{verify_dir}\"\n",
            "            directory-listing #false\n",
            "            cache-control \"no-store\"\n",
            "            compress #false\n",
            "        }}\n",
            "    }}\n",
        ),
        path = INGRESS_VERIFY_PATH,
        config_dir = CONTAINER_CONFIG_DIR,
        verify_dir = ZENTINEL_VERIFY_DIR,
    );
}

fn write_https_namespace(output: &mut String, projection: &IngressProjection) {
    output.push_str("namespace \"ployz-https\" {\n    routes {\n");
    write_proxy_routes(output, &projection.sites, HttpProtocol::Https, "        ");
    output.push_str("    }\n}\n\n");
}

fn write_proxy_routes(
    output: &mut String,
    sites: &[IngressSite],
    protocol: HttpProtocol,
    indent: &str,
) {
    for site in sites {
        if renderable_route(site, protocol).is_none() {
            continue;
        }
        let id = route_id(protocol, &site.hostname);
        let _ = write!(
            output,
            concat!(
                "{indent}route \"{id}\" {{\n",
                "{indent}    priority \"normal\"\n",
                "{indent}    matches {{\n",
                "{indent}        host \"{}\"\n",
                "{indent}    }}\n",
                "{indent}    upstream \"{id}\"\n",
                "{indent}    websocket #true\n",
                "{indent}    retry-policy {{\n",
                "{indent}        max-attempts 3\n",
                "{indent}    }}\n",
                "{indent}}}\n",
            ),
            site.hostname,
            id = id,
            indent = indent,
        );
    }
}

fn write_proxy_upstreams(
    output: &mut String,
    sites: &[IngressSite],
    protocol: HttpProtocol,
    indent: &str,
) {
    for site in sites {
        let Some(endpoints) = renderable_route(site, protocol) else {
            continue;
        };
        write_upstream(
            output,
            &route_id(protocol, &site.hostname),
            endpoints,
            indent,
        );
    }
}

fn write_challenge_routes(output: &mut String, sites: &[IngressSite], indent: &str) {
    for site in sites {
        let Some(challenge) = site.challenge() else {
            continue;
        };
        let path = format!("/.well-known/acme-challenge/{}", challenge.token());
        let root = format!(
            "{CONTAINER_CONFIG_DIR}/{ZENTINEL_CHALLENGES_DIR}/{}",
            site.hostname
        );
        let _ = write!(
            output,
            concat!(
                "{indent}route \"ployz-challenge-{}\" {{\n",
                "{indent}    priority \"critical\"\n",
                "{indent}    matches {{\n",
                "{indent}        host \"{}\"\n",
                "{indent}        path {}\n",
                "{indent}    }}\n",
                "{indent}    static-files {{\n",
                "{indent}        root {}\n",
                "{indent}        directory-listing #false\n",
                "{indent}        cache-control \"no-store\"\n",
                "{indent}        compress #false\n",
                "{indent}    }}\n",
                "{indent}}}\n",
            ),
            site.hostname,
            site.hostname,
            quoted(&path),
            quoted(&root),
            indent = indent,
        );
    }
}

fn write_upstream(output: &mut String, id: &str, endpoints: &[IngressEndpoint], indent: &str) {
    let _ = writeln!(output, "{indent}upstream \"{id}\" {{");
    if endpoints.is_empty() {
        let _ = writeln!(output, "{indent}    target \"127.0.0.1:0\"");
    } else {
        for endpoint in endpoints {
            let _ = writeln!(
                output,
                "{indent}    target \"{}:{}\"",
                endpoint.address.0, endpoint.port,
            );
        }
    }
    let _ = write!(
        output,
        concat!(
            "{indent}    load-balancing \"round_robin\"\n",
            "\n",
            "{indent}    circuit-breaker {{\n",
            "{indent}        failure-threshold 5\n",
            "{indent}        success-threshold 2\n",
            "{indent}        timeout-seconds 30\n",
            "{indent}        half-open-max-requests 1\n",
            "{indent}    }}\n",
        ),
        indent = indent,
    );
    if endpoints.is_empty() {
        let _ = write!(
            output,
            concat!(
                "\n",
                "{indent}    timeouts {{\n",
                "{indent}        connect-secs 1\n",
                "{indent}        request-secs 2\n",
                "{indent}        read-secs 2\n",
                "{indent}        write-secs 2\n",
                "{indent}    }}\n",
            ),
            indent = indent,
        );
    }
    let _ = writeln!(output, "{indent}}}");
}

fn renderable_route(site: &IngressSite, protocol: HttpProtocol) -> Option<&[IngressEndpoint]> {
    let endpoints = site.route(protocol)?;
    (protocol == HttpProtocol::Http || site.material().is_some()).then_some(endpoints)
}

fn route_id(protocol: HttpProtocol, hostname: &IngressHost) -> String {
    format!("ployz-{}-{hostname}", protocol_name(protocol))
}

const fn protocol_name(protocol: HttpProtocol) -> &'static str {
    match protocol {
        HttpProtocol::Http => "http",
        HttpProtocol::Https => "https",
    }
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn ensure_bootstrap(directory: &Path) -> Result<(), Error> {
    use rcgen::CertifiedKey;

    let certificate_path = directory.join(ZENTINEL_BOOTSTRAP_CERT_FILE);
    let key_path = directory.join(ZENTINEL_BOOTSTRAP_KEY_FILE);
    if !certificate_path.exists() || !key_path.exists() {
        let CertifiedKey { cert, signing_key } =
            rcgen::generate_simple_self_signed(["ployz-bootstrap.invalid".to_owned()])
                .map_err(io::Error::other)?;
        atomic_write(&certificate_path, cert.pem().as_bytes(), 0o644)?;
        atomic_write(&key_path, signing_key.serialize_pem().as_bytes(), 0o640)?;
    }
    let certificate = fs::read_to_string(&certificate_path)?;
    let key = fs::read_to_string(&key_path)?;
    if CertificateMaterial::new(certificate, key).is_none() {
        return Err(Error::InvalidBootstrapPair);
    }
    secure_file(&certificate_path, 0o644)?;
    secure_file(&key_path, 0o640)?;
    Ok(())
}

fn write_challenges(config_file: &Path, sites: &[IngressSite]) -> Result<(), Error> {
    let root = config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?
        .join(ZENTINEL_CHALLENGES_DIR);
    prepare_directory(&root)?;
    for site in sites {
        let Some(challenge) = site.challenge() else {
            continue;
        };
        let token = Path::new(challenge.token());
        let hostname_directory = root.join(site.hostname.as_str());
        prepare_directory(&hostname_directory)?;
        let well_known_directory = hostname_directory.join(".well-known");
        prepare_directory(&well_known_directory)?;
        let directory = well_known_directory.join("acme-challenge");
        prepare_directory(&directory)?;
        let path = directory.join(token);
        atomic_write(&path, challenge.response().as_bytes(), 0o644)?;
        set_group(&path)?;
    }
    Ok(())
}

fn write_verification_file(config_file: &Path, machine: &Machine) -> Result<(), Error> {
    let root = config_file
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "config file has no parent"))?
        .join(ZENTINEL_VERIFY_DIR);
    prepare_directory(&root)?;
    let path = root.join(INGRESS_VERIFY_PATH.trim_start_matches('/'));
    atomic_write(&path, machine.id.to_string().as_bytes(), 0o644)?;
    set_group(&path)?;
    Ok(())
}

fn validate_challenge_tokens(sites: &[IngressSite]) -> Result<(), Error> {
    for challenge in sites.iter().filter_map(IngressSite::challenge) {
        let mut components = Path::new(challenge.token()).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(Error::InvalidChallengeToken);
        }
    }
    Ok(())
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    set_group(path)
}

fn secure_file(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    set_group(path)
}

/// Assign the exact selected Zentinel image's numeric group when running as root.
///
/// # Errors
///
/// Returns when the path cannot be inspected or its group cannot be changed.
pub(crate) fn set_group(path: &Path) -> io::Result<()> {
    fs::metadata(path)?;
    if fs::metadata("/proc/self")?.uid() == 0 {
        chown(path, None, Some(ZENTINEL_GID))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../zentinel_tests.rs"]
mod tests;
