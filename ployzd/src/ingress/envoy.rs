//! Deterministic Envoy file-based xDS rendering and live apply.

use ployz_core::{
    ContainerId, ContainerObservation, HttpProtocol, INGRESS_VERIFY_PATH, IngressHost,
    IngressProxyBackend, Machine, ingress_proxy_backend,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{corrosion::ReplicatedStore, docker::LocalDocker, filesystem::atomic_write};

mod apply;

use super::{
    IngressEndpoint, IngressProjection, IngressSite, WatchApply, certificate_file_stem,
    newest_local_ingress, watch as watch_inputs,
};

pub(crate) const CONFIG_FILE: &str = "bootstrap.yaml";
const LDS_FILE: &str = "lds.yaml";
const RDS_FILE: &str = "rds.yaml";
const CDS_FILE: &str = "cds.yaml";
const SDS_FILE: &str = "sds.yaml";
const HTTP_CONTAINER_PORT: u16 = 8080;
const HTTPS_CONTAINER_PORT: u16 = 8443;
const CONTAINER_CERTS_DIR: &str = "/config/certs";
const SDS_PATH: &str = "/config/sds.yaml";
const ROUTE_TIMEOUT: &str = "0s";
const CONNECT_TIMEOUT: &str = "5s";
/// Envoy rejects `port_value` 0; `127.0.0.1:1` fails closed as HTTP 503.
const EMPTY_UPSTREAM_PORT: u16 = 1;

/// Static bootstrap: file-watched xDS, admin omitted.
pub(crate) const BOOTSTRAP: &str = "\
node:
  id: ingress
  cluster: ployz
dynamic_resources:
  lds_config:
    path_config_source:
      path: /config/lds.yaml
      watched_directory:
        path: /config
    resource_api_version: V3
  cds_config:
    path_config_source:
      path: /config/cds.yaml
      watched_directory:
        path: /config
    resource_api_version: V3
";

const LISTENER_TYPE: &str = "type.googleapis.com/envoy.config.listener.v3.Listener";
const ROUTE_TYPE: &str = "type.googleapis.com/envoy.config.route.v3.RouteConfiguration";
const CLUSTER_TYPE: &str = "type.googleapis.com/envoy.config.cluster.v3.Cluster";
const SECRET_TYPE: &str = "type.googleapis.com/envoy.extensions.transport_sockets.tls.v3.Secret";
const DOWNSTREAM_TLS_TYPE: &str =
    "type.googleapis.com/envoy.extensions.transport_sockets.tls.v3.DownstreamTlsContext";
const HCM_TYPE: &str = "type.googleapis.com/envoy.extensions.filters.network.http_connection_manager.v3.HttpConnectionManager";
const ROUTER_TYPE: &str = "type.googleapis.com/envoy.extensions.filters.http.router.v3.Router";
const STDOUT_ACCESS_LOG_TYPE: &str =
    "type.googleapis.com/envoy.extensions.access_loggers.stream.v3.StdoutAccessLog";

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
        certificates: &BTreeMap<IngressHost, crate::corrosion::CertificateRow>,
    ) -> Result<Self, ployz_core::IngressProxyServiceSpecError> {
        let process = newest_local_ingress(machine, observations)
            .map(|container| {
                let observation = container.as_observation();
                if ingress_proxy_backend(&observation.resolved_spec)? != IngressProxyBackend::Envoy
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

/// Watch the common projection and apply it through file-watched Envoy xDS.
pub(crate) async fn watch(
    machine: Machine,
    replicated: ReplicatedStore,
    config_file: PathBuf,
    docker: LocalDocker,
    shutdown: CancellationToken,
) -> io::Result<()> {
    replicated
        .require_ingress_proxy_backend(IngressProxyBackend::Envoy)
        .await
        .map_err(io::Error::other)?;
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
                    "local Envoy Ingress Proxy container is missing",
                ));
            };
            classify_apply(
                apply::apply(&input.projection, &config_file, &process.image, &docker).await,
            )
        },
    )
    .await
}

fn classify_apply(result: Result<apply::ApplyOutcome, apply::Error>) -> WatchApply {
    match result {
        Ok(apply::ApplyOutcome::Published { .. }) => WatchApply::Applied,
        Err(error @ apply::Error::ValidationRejected { .. }) => {
            WatchApply::WaitForChange(io::Error::other(error))
        }
        Err(error) => WatchApply::Retry(io::Error::other(error)),
    }
}

/// Return the Envoy bootstrap path beneath the shared ingress data root.
#[must_use]
pub(crate) fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ingress").join("envoy").join(CONFIG_FILE)
}

/// Concatenate the live xDS files the operator-visible config RPC returns.
///
/// # Errors
///
/// Returns when any live xDS file cannot be read.
pub(crate) fn read_generated_config(data_dir: &Path) -> io::Result<String> {
    let config_file = config_path(data_dir);
    let directory = config_file.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Envoy bootstrap path has no parent",
        )
    })?;
    let lds = fs::read_to_string(directory.join(LDS_FILE))?;
    let rds = fs::read_to_string(directory.join(RDS_FILE))?;
    let cds = fs::read_to_string(directory.join(CDS_FILE))?;
    let sds = fs::read_to_string(directory.join(SDS_FILE))?;
    Ok(format!("{lds}\n---\n{rds}\n---\n{cds}\n---\n{sds}"))
}

/// Install the bootstrap and empty xDS required before the first Envoy process starts.
///
/// Existing bootstrap is authoritative and is only replaced by the validated apply path.
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
    fs::create_dir_all(parent)?;
    write_xds(parent, &rendered)?;
    atomic_write(config_file, BOOTSTRAP.as_bytes(), 0o644)?;
    Ok(())
}

fn write_xds(directory: &Path, rendered: &RenderedConfig) -> io::Result<()> {
    atomic_write(&directory.join(LDS_FILE), rendered.lds().as_bytes(), 0o644)?;
    atomic_write(&directory.join(RDS_FILE), rendered.rds().as_bytes(), 0o644)?;
    atomic_write(&directory.join(CDS_FILE), rendered.cds().as_bytes(), 0o644)?;
    atomic_write(&directory.join(SDS_FILE), rendered.sds().as_bytes(), 0o644)
}

/// Rendered Envoy xDS tied to one projection digest.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct RenderedConfig {
    lds: String,
    rds: String,
    cds: String,
    sds: String,
    digest: String,
}

impl RenderedConfig {
    /// Listener Discovery Service YAML consumed by file-watched Envoy.
    #[must_use]
    pub(crate) fn lds(&self) -> &str {
        &self.lds
    }

    /// Route Discovery Service YAML consumed by file-watched Envoy.
    #[must_use]
    pub(crate) fn rds(&self) -> &str {
        &self.rds
    }

    /// Cluster Discovery Service YAML consumed by file-watched Envoy.
    #[must_use]
    pub(crate) fn cds(&self) -> &str {
        &self.cds
    }

    /// Secret Discovery Service YAML consumed by file-watched Envoy.
    #[must_use]
    pub(crate) fn sds(&self) -> &str {
        &self.sds
    }

    /// Stable digest of the complete private Ingress Projection.
    #[must_use]
    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

/// Failure while rendering Envoy configuration.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// A tagged fragment belongs to another concrete backend.
    #[error("cannot render caddy Ingress Proxy Fragment with envoy")]
    BackendMismatch,
    /// A bootstrap or xDS file operation failed.
    #[error("Envoy ingress filesystem operation failed: {0}")]
    Filesystem(#[from] io::Error),
}

/// Render one already-derived projection as file-based xDS.
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
    Ok(RenderedConfig {
        lds: render_lds(projection, &digest),
        rds: render_rds(projection, &digest),
        cds: render_cds(projection, &digest),
        sds: render_sds(projection, &digest),
        digest,
    })
}

fn projection_digest(projection: &IngressProjection) -> String {
    let canonical = serde_json::to_vec(projection)
        .expect("Ingress Projection contains only serializable value types");
    hex::encode(Sha256::digest(canonical))
}

fn render_lds(projection: &IngressProjection, digest: &str) -> String {
    let mut yaml = format!(
        "\
version_info: \"{digest}\"
resources:
- \"@type\": {LISTENER_TYPE}
  name: ployz-http
  per_connection_buffer_limit_bytes: 32768
  address:
    socket_address:
      address: 0.0.0.0
      port_value: {HTTP_CONTAINER_PORT}
  metadata:
    filter_metadata:
      com.ployz:
        projection_digest: {digest}
  filter_chains:
  - filters:
"
    );
    write_hcm(&mut yaml, "ingress_http", "http");
    let mut https = false;
    for site in &projection.sites {
        if tls_route(site).is_none() {
            continue;
        }
        if !https {
            let _ = write!(
                yaml,
                concat!(
                    "- \"@type\": {listener}\n",
                    "  name: ployz-https\n",
                    "  per_connection_buffer_limit_bytes: 32768\n",
                    "  address:\n",
                    "    socket_address:\n",
                    "      address: 0.0.0.0\n",
                    "      port_value: {port}\n",
                    "  metadata:\n",
                    "    filter_metadata:\n",
                    "      com.ployz:\n",
                    "        projection_digest: {digest}\n",
                    "  filter_chains:\n",
                ),
                listener = LISTENER_TYPE,
                port = HTTPS_CONTAINER_PORT,
                digest = digest,
            );
            https = true;
        }
        write_tls_filter_chain(&mut yaml, site);
    }
    yaml
}

fn write_tls_filter_chain(yaml: &mut String, site: &IngressSite) {
    let secret = secret_id(&site.hostname);
    let _ = write!(
        yaml,
        concat!(
            "  - filter_chain_match:\n",
            "      server_names:\n",
            "      - {hostname}\n",
            "    transport_socket:\n",
            "      name: envoy.transport_sockets.tls\n",
            "      typed_config:\n",
            "        \"@type\": {tls}\n",
            "        common_tls_context:\n",
            "          tls_params:\n",
            "            tls_minimum_protocol_version: TLSv1_2\n",
            "          alpn_protocols:\n",
            "          - h2\n",
            "          - http/1.1\n",
            "          tls_certificate_sds_secret_configs:\n",
            "          - name: {secret}\n",
            "            sds_config:\n",
            "              resource_api_version: V3\n",
            "              path_config_source:\n",
            "                path: {sds}\n",
            "                watched_directory:\n",
            "                  path: /config\n",
            "    filters:\n",
        ),
        hostname = site.hostname,
        tls = DOWNSTREAM_TLS_TYPE,
        secret = secret,
        sds = SDS_PATH,
    );
    write_hcm(yaml, "ingress_https", "https");
}

fn write_hcm(yaml: &mut String, stat_prefix: &str, route_config_name: &str) {
    let _ = write!(
        yaml,
        concat!(
            "    - name: envoy.filters.network.http_connection_manager\n",
            "      typed_config:\n",
            "        \"@type\": {hcm}\n",
            "        stat_prefix: {stat}\n",
            "        codec_type: AUTO\n",
            "        use_remote_address: true\n",
            "        generate_request_id: true\n",
            "        preserve_external_request_id: false\n",
            "        always_set_request_id_in_response: true\n",
            "        common_http_protocol_options:\n",
            "          idle_timeout: 300s\n",
            "        http2_protocol_options:\n",
            "          max_concurrent_streams: 100\n",
            "          initial_stream_window_size: 65536\n",
            "          initial_connection_window_size: 1048576\n",
            "        stream_idle_timeout: 300s\n",
            "        request_headers_timeout: 15s\n",
            "        upgrade_configs:\n",
            "        - upgrade_type: websocket\n",
            "        access_log:\n",
            "        - name: envoy.access_loggers.stdout\n",
            "          typed_config:\n",
            "            \"@type\": {stdout_access_log}\n",
            "            log_format:\n",
            "              json_format:\n",
            "                request_id: \"%REQ(X-REQUEST-ID)%\"\n",
            "                hostname: \"%REQ(:AUTHORITY)%\"\n",
            "                method: \"%REQ(:METHOD)%\"\n",
            "                path: \"%PATH(NQ:PATH)%\"\n",
            "                status: \"%RESPONSE_CODE%\"\n",
            "                duration_ms: \"%DURATION%\"\n",
            "                upstream: \"%UPSTREAM_HOST%\"\n",
            "                envoy_failure_details: \"%RESPONSE_CODE_DETAILS%\"\n",
            "        rds:\n",
            "          route_config_name: {route}\n",
            "          config_source:\n",
            "            resource_api_version: V3\n",
            "            path_config_source:\n",
            "              path: /config/rds.yaml\n",
            "              watched_directory:\n",
            "                path: /config\n",
            "        http_filters:\n",
            "        - name: envoy.filters.http.router\n",
            "          typed_config:\n",
            "            \"@type\": {router}\n",
        ),
        hcm = HCM_TYPE,
        stat = stat_prefix,
        route = route_config_name,
        router = ROUTER_TYPE,
        stdout_access_log = STDOUT_ACCESS_LOG_TYPE,
    );
}

fn render_rds(projection: &IngressProjection, digest: &str) -> String {
    let mut yaml = format!(
        "\
version_info: \"{digest}\"
resources:
- \"@type\": {ROUTE_TYPE}
  name: http
  metadata:
    filter_metadata:
      com.ployz:
        projection_digest: {digest}
  virtual_hosts:
"
    );
    for site in &projection.sites {
        let http = site.route(HttpProtocol::Http);
        let challenge = site.challenge();
        if http.is_none() && challenge.is_none() {
            continue;
        }
        let id = route_id(HttpProtocol::Http, &site.hostname);
        let _ = writeln!(yaml, "  - name: {id}");
        let _ = writeln!(yaml, "    domains:");
        let _ = writeln!(yaml, "    - {}", site.hostname);
        let _ = writeln!(yaml, "    routes:");
        if let Some(challenge) = challenge {
            let path = format!("/.well-known/acme-challenge/{}", challenge.token());
            let _ = writeln!(yaml, "    - match:");
            let _ = writeln!(yaml, "        path: {}", quoted(&path));
            let _ = writeln!(yaml, "      direct_response:");
            let _ = writeln!(yaml, "        status: 200");
            let _ = writeln!(yaml, "        body:");
            let _ = writeln!(
                yaml,
                "          inline_string: {}",
                quoted(challenge.response())
            );
        }
        if http.is_some() {
            let _ = writeln!(yaml, "    - match:");
            let _ = writeln!(yaml, "        prefix: /");
            let _ = writeln!(yaml, "      route:");
            let _ = writeln!(yaml, "        cluster: {id}");
            let _ = writeln!(yaml, "        timeout: {ROUTE_TIMEOUT}");
            write_retry_policy(&mut yaml);
        }
    }
    let _ = writeln!(yaml, "  - name: ployz-verify");
    let _ = writeln!(yaml, "    domains:");
    let _ = writeln!(yaml, "    - \"*\"");
    let _ = writeln!(yaml, "    routes:");
    let _ = writeln!(yaml, "    - match:");
    let _ = writeln!(yaml, "        path: {INGRESS_VERIFY_PATH}");
    let _ = writeln!(yaml, "      direct_response:");
    let _ = writeln!(yaml, "        status: 200");
    let _ = writeln!(yaml, "        body:");
    let _ = writeln!(yaml, "          inline_string: {}", projection.machine.id);
    if projection
        .sites
        .iter()
        .any(|site| tls_route(site).is_some())
    {
        let _ = write!(
            yaml,
            concat!(
                "- \"@type\": {route}\n",
                "  name: https\n",
                "  metadata:\n",
                "    filter_metadata:\n",
                "      com.ployz:\n",
                "        projection_digest: {digest}\n",
                "  virtual_hosts:\n",
            ),
            route = ROUTE_TYPE,
            digest = digest,
        );
        for site in &projection.sites {
            if tls_route(site).is_none() {
                continue;
            }
            let id = route_id(HttpProtocol::Https, &site.hostname);
            let _ = writeln!(yaml, "  - name: {id}");
            let _ = writeln!(yaml, "    domains:");
            let _ = writeln!(yaml, "    - {}", site.hostname);
            let _ = writeln!(yaml, "    routes:");
            let _ = writeln!(yaml, "    - match:");
            let _ = writeln!(yaml, "        prefix: /");
            let _ = writeln!(yaml, "      route:");
            let _ = writeln!(yaml, "        cluster: {id}");
            let _ = writeln!(yaml, "        timeout: {ROUTE_TIMEOUT}");
            write_retry_policy(&mut yaml);
        }
    }
    yaml
}

fn render_cds(projection: &IngressProjection, digest: &str) -> String {
    let mut body = String::new();
    for site in &projection.sites {
        if let Some(endpoints) = site.route(HttpProtocol::Http) {
            write_cluster(
                &mut body,
                &route_id(HttpProtocol::Http, &site.hostname),
                digest,
                endpoints,
            );
        }
        if let Some(endpoints) = tls_route(site) {
            write_cluster(
                &mut body,
                &route_id(HttpProtocol::Https, &site.hostname),
                digest,
                endpoints,
            );
        }
    }
    if body.is_empty() {
        format!("version_info: \"{digest}\"\nresources: []\n")
    } else {
        format!("version_info: \"{digest}\"\nresources:\n{body}")
    }
}

fn write_retry_policy(yaml: &mut String) {
    yaml.push_str(
        "        retry_policy:\n          retry_on: connect-failure\n          num_retries: 1\n",
    );
}

fn render_sds(projection: &IngressProjection, digest: &str) -> String {
    let mut body = String::new();
    for site in &projection.sites {
        let Some(material) = site.material() else {
            continue;
        };
        if site.route(HttpProtocol::Https).is_none() {
            continue;
        }
        let stem = certificate_file_stem(&site.hostname, material);
        let id = secret_id(&site.hostname);
        let _ = write!(
            body,
            concat!(
                "- \"@type\": {secret_type}\n",
                "  name: {id}\n",
                "  tls_certificate:\n",
                "    certificate_chain:\n",
                "      filename: {certs}/{stem}.crt\n",
                "    private_key:\n",
                "      filename: {certs}/{stem}.key\n",
            ),
            secret_type = SECRET_TYPE,
            id = id,
            certs = CONTAINER_CERTS_DIR,
            stem = stem,
        );
    }
    if body.is_empty() {
        format!("version_info: \"{digest}\"\nresources: []\n")
    } else {
        format!("version_info: \"{digest}\"\nresources:\n{body}")
    }
}

fn write_cluster(yaml: &mut String, id: &str, digest: &str, endpoints: &[IngressEndpoint]) {
    let _ = write!(
        yaml,
        concat!(
            "- \"@type\": {cluster_type}\n",
            "  name: {id}\n",
            "  type: STATIC\n",
            "  connect_timeout: {connect}\n",
            "  per_connection_buffer_limit_bytes: 32768\n",
            "  lb_policy: ROUND_ROBIN\n",
            "  outlier_detection:\n",
            "    split_external_local_origin_errors: true\n",
            "    consecutive_5xx: 0\n",
            "    enforcing_success_rate: 0\n",
            "    enforcing_local_origin_success_rate: 0\n",
            "    consecutive_local_origin_failure: 3\n",
            "    enforcing_consecutive_local_origin_failure: 100\n",
            "    base_ejection_time: 30s\n",
            "    max_ejection_time: 30s\n",
            "    max_ejection_percent: 50\n",
            "  metadata:\n",
            "    filter_metadata:\n",
            "      com.ployz:\n",
            "        projection_digest: {digest}\n",
            "  load_assignment:\n",
            "    cluster_name: {id}\n",
            "    endpoints:\n",
            "    - lb_endpoints:\n",
        ),
        cluster_type = CLUSTER_TYPE,
        id = id,
        connect = CONNECT_TIMEOUT,
        digest = digest,
    );
    if endpoints.is_empty() {
        yaml.push_str(&socket_endpoint("127.0.0.1", EMPTY_UPSTREAM_PORT));
    } else {
        for endpoint in endpoints {
            yaml.push_str(&socket_endpoint(endpoint.address.0, endpoint.port.get()));
        }
    }
}

fn socket_endpoint(address: impl std::fmt::Display, port: u16) -> String {
    format!(
        concat!(
            "      - endpoint:\n",
            "          address:\n",
            "            socket_address:\n",
            "              address: {address}\n",
            "              port_value: {port}\n",
        ),
        address = address,
        port = port,
    )
}

fn tls_route(site: &IngressSite) -> Option<&[IngressEndpoint]> {
    let endpoints = site.route(HttpProtocol::Https)?;
    site.material()?;
    Some(endpoints)
}

fn route_id(protocol: HttpProtocol, hostname: &IngressHost) -> String {
    match protocol {
        HttpProtocol::Http => format!("ployz-http-{hostname}"),
        HttpProtocol::Https => format!("ployz-https-{hostname}"),
    }
}

fn secret_id(hostname: &IngressHost) -> String {
    format!("ployz-tls-{hostname}")
}

fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

#[cfg(test)]
mod watch_tests {
    use super::apply::{ApplyOutcome, Error as ApplyError};
    use super::*;

    #[test]
    fn rejection_waits_for_changed_input() {
        assert!(matches!(
            classify_apply(Err(ApplyError::ValidationRejected {
                digest: "digest".into(),
                reason: "invalid".into(),
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

    #[test]
    fn published_apply_is_terminal_for_this_input() {
        assert!(matches!(
            classify_apply(Ok(ApplyOutcome::Published {
                digest: "digest".into(),
            })),
            WatchApply::Applied
        ));
    }
}

#[cfg(test)]
#[path = "../envoy_tests.rs"]
mod tests;
