//! Deterministic Caddy configuration rendering and application.

use chrono::{SecondsFormat, Utc};
use ployz_core::{
    HttpProtocol, INGRESS_VERIFY_PATH, IngressHost, IngressProxyFragment, Machine,
    QualifiedService, ServiceName,
};
use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    future::Future,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{CertificateChallenge, ReplicatedStore},
    filesystem::{atomic_write, set_ployz_group},
    ingress::{IngressEndpoint, IngressProjection, IngressSite},
};

pub const CONFIG_FILE: &str = "Caddyfile";
const CONTAINER_CERTS_DIR: &str = "/config/caddy/certs";
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Failure while rendering or applying Caddy configuration.
#[derive(Debug, Error)]
pub(crate) enum Error {
    /// Local filesystem operation failed.
    #[error(transparent)]
    Io(#[from] io::Error),
    /// Caddy administration request failed.
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    /// Caddy administration response was malformed.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Caddy rejected or omitted an administration result.
    #[error("{0}")]
    Admin(String),
    /// Tagged Caddy fragment could not be rendered.
    #[error("{0}")]
    Template(String),
}

/// Caddy's concrete validation and acknowledged-load interface.
pub(crate) trait CaddyAdmin: Send + Sync {
    /// Adapt and validate one Caddyfile without applying it.
    fn adapt(&self, caddyfile: &str) -> impl Future<Output = Result<String, Error>> + Send;
    /// Load adapted Caddy JSON and wait for acknowledgement.
    fn load(&self, json: &str) -> impl Future<Output = Result<(), Error>> + Send;
}

struct AdminClient {
    client: Client,
}

impl AdminClient {
    async fn connect_if_available(socket: &Path) -> Result<Option<Self>, Error> {
        if !matches!(
            tokio::time::timeout(Duration::from_secs(1), UnixStream::connect(socket)).await,
            Ok(Ok(_))
        ) {
            return Ok(None);
        }
        Ok(Some(Self {
            client: Client::builder()
                .timeout(ADMIN_TIMEOUT)
                .unix_socket(socket)
                .build()?,
        }))
    }

    async fn post(&self, path: &str, content_type: &str, body: String) -> Result<String, Error> {
        let response = self
            .client
            .post(format!("http://localhost{path}"))
            .header(header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if status != StatusCode::OK {
            return Err(Error::Admin(if body.is_empty() {
                format!("Caddy admin returned {status}")
            } else {
                body
            }));
        }
        Ok(body)
    }
}

impl CaddyAdmin for AdminClient {
    async fn adapt(&self, caddyfile: &str) -> Result<String, Error> {
        let body = self
            .post("/adapt", "text/caddyfile", caddyfile.to_owned())
            .await?;
        let response: Value = serde_json::from_str(&body)?;
        response
            .get("result")
            .map(Value::to_string)
            .ok_or_else(|| Error::Admin("Caddy /adapt response omitted result".into()))
    }

    async fn load(&self, json: &str) -> Result<(), Error> {
        self.post("/load", "application/json", json.to_owned())
            .await?;
        Ok(())
    }
}

#[must_use]
/// Return the Caddy configuration path beneath the shared ingress data root.
pub(crate) fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ingress").join("caddy").join(CONFIG_FILE)
}

pub async fn run(
    machine: Machine,
    replicated: ReplicatedStore,
    config_file: PathBuf,
    admin_socket: PathBuf,
    shutdown: CancellationToken,
) -> io::Result<()> {
    replicated
        .require_ingress_proxy_backend(ployz_core::IngressProxyBackend::Caddy)
        .await
        .map_err(io::Error::other)?;
    prepare_directory(
        admin_socket
            .parent()
            .ok_or_else(|| io::Error::other("Caddy admin socket has no parent"))?,
    )?;
    super::watch_caddy(machine, replicated, config_file, shutdown, move || {
        let admin_socket = admin_socket.clone();
        async move { AdminClient::connect_if_available(&admin_socket).await }
    })
    .await
}

/// Render and apply one already-derived projection through Caddy.
///
/// # Errors
///
/// Returns a filesystem, rendering, validation, or acknowledged-load error.
pub(crate) async fn reconcile<A: CaddyAdmin>(
    projection: &IngressProjection,
    config_file: &Path,
    admin: Option<&A>,
) -> Result<(), Error> {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let caddyfile = generate_caddyfile(projection, &timestamp, admin).await;
    if let Some(admin) = admin {
        let json = admin.adapt(&caddyfile).await?;
        admin.load(&json).await?;
    }
    write_caddyfile(config_file, &caddyfile)?;
    Ok(())
}

fn write_caddyfile(path: &Path, caddyfile: &str) -> Result<(), Error> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caddyfile has no parent"))?;
    prepare_directory(directory)?;
    atomic_write(path, caddyfile.as_bytes(), 0o640)?;
    set_ployz_group(path)?;
    Ok(())
}

fn prepare_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
    set_ployz_group(path)
}

async fn generate_caddyfile<A: CaddyAdmin>(
    projection: &IngressProjection,
    timestamp: &str,
    admin: Option<&A>,
) -> String {
    let mut output = automatic_caddyfile(projection, timestamp, None);
    let Some(admin) = admin else {
        output.push_str(
            "\n# User-defined Caddy configs are unavailable because Caddy's admin API is not reachable.\n\
# Run `ployz ingress logs` and `ployz ingress config` to troubleshoot.\n",
        );
        return output;
    };

    let mut skipped = Vec::new();
    if let Some(config) = projection
        .global_fragment
        .as_ref()
        .and_then(IngressProxyFragment::as_caddy)
    {
        match render_custom_config(
            config,
            &QualifiedService::system_ingress(),
            &projection.upstreams,
        ) {
            Ok(rendered) => {
                let candidate = automatic_caddyfile(projection, timestamp, Some(&rendered));
                // TODO(UT-119): /adapt remains the only validation for custom Caddyfile
                // candidates and may accept a configuration that /load rejects.
                match admin.adapt(&candidate).await {
                    Ok(_) => output = candidate,
                    Err(error) => {
                        skipped.push(format!(
                            "Service '{}': validation failed: {error}",
                            QualifiedService::system_ingress()
                        ));
                    }
                }
            }
            Err(error) => skipped.push(format!(
                "Service '{}': rendering failed: {error}",
                QualifiedService::system_ingress()
            )),
        }
    }

    for (identity, fragment) in &projection.service_fragments {
        let Some(config) = fragment.as_caddy() else {
            continue;
        };
        let rendered = match render_custom_config(config, identity, &projection.upstreams) {
            Ok(rendered) => rendered,
            Err(error) => {
                skipped.push(format!("Service '{identity}': rendering failed: {error}"));
                continue;
            }
        };
        let fragment = format!("\n# User-defined config for Service '{identity}'.\n{rendered}\n");
        let candidate = format!("{output}{fragment}");
        match admin.adapt(&candidate).await {
            Ok(_) => output = candidate,
            Err(error) => skipped.push(format!("Service '{identity}': validation failed: {error}")),
        }
    }
    if !skipped.is_empty() {
        output.push_str("\n# Skipped invalid user-defined configs:\n");
        for error in skipped {
            for (index, line) in error.lines().enumerate() {
                let _ = writeln!(output, "# {} {line}", if index == 0 { "-" } else { " " });
            }
        }
    }
    output
}

fn render_custom_config(
    template: &str,
    current_service: &QualifiedService,
    upstreams_by_service: &BTreeMap<QualifiedService, Vec<ployz_core::ContainerAddress>>,
) -> Result<String, Error> {
    let mut rendered = String::new();
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = expression
            .find("}}")
            .ok_or_else(|| Error::Template("unclosed template expression".into()))?;
        let tokens = shell_words::split(expression[..end].trim())
            .map_err(|error| Error::Template(error.to_string()))?;
        let replacement = match tokens.as_slice() {
            [name] if name == ".Name" => current_service.to_string(),
            [helper] if helper == "upstreams" => {
                upstreams(upstreams_by_service, current_service, None)
            }
            [helper, argument] if helper == "upstreams" => match argument.parse::<u16>() {
                Ok(port) => upstreams(upstreams_by_service, current_service, Some(port)),
                Err(_) => {
                    let identity =
                        upstream_identity(current_service, argument, upstreams_by_service)?;
                    upstreams(upstreams_by_service, &identity, None)
                }
            },
            [helper, service, port] if helper == "upstreams" => {
                let identity = upstream_identity(current_service, service, upstreams_by_service)?;
                let port = port
                    .parse::<u16>()
                    .map_err(|_| Error::Template(format!("invalid upstream port '{port}'")))?;
                upstreams(upstreams_by_service, &identity, Some(port))
            }
            _ => {
                return Err(Error::Template(format!(
                    "unsupported template expression '{{{{{}}}}}'",
                    expression[..end].trim()
                )));
            }
        };
        rendered.push_str(&replacement);
        remaining = &expression[end + 2..];
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

fn upstream_identity(
    current_service: &QualifiedService,
    argument: &str,
    upstreams: &BTreeMap<QualifiedService, Vec<ployz_core::ContainerAddress>>,
) -> Result<QualifiedService, Error> {
    if argument == ".Name" {
        return Ok(current_service.clone());
    }
    let identity = if let Ok(identity) = QualifiedService::parse(argument) {
        identity
    } else {
        let name = ServiceName::parse(argument)
            .map_err(|_| Error::Template(format!("Service '{argument}' was not found")))?;
        QualifiedService::new(current_service.project.clone(), name)
    };
    if upstreams.contains_key(&identity) {
        Ok(identity)
    } else {
        Err(Error::Template(format!(
            "Service '{argument}' was not found"
        )))
    }
}

fn upstreams(
    upstreams: &BTreeMap<QualifiedService, Vec<ployz_core::ContainerAddress>>,
    identity: &QualifiedService,
    port: Option<u16>,
) -> String {
    upstreams
        .get(identity)
        .into_iter()
        .flatten()
        .map(|address| match port {
            Some(port) => format!("{}:{port}", address.0),
            None => address.0.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn automatic_caddyfile(
    projection: &IngressProjection,
    timestamp: &str,
    global_config: Option<&str>,
) -> String {
    let machine_name = projection.machine.name.as_str();
    let local_machine = &projection.machine.id;
    let mut output = format!(
        "# Caddyfile autogenerated by Ployz on Machine '{machine_name}' (DO NOT EDIT): {timestamp}\n\
# Automatically updated on Service or health status changes.\n\
# Docs: https://github.com/getployz/ployz2\n\
\n"
    );
    write_global_options(&mut output, global_config);
    let _ = write!(
        output,
        "# Health check endpoint to verify Caddy reachability on this Machine.\n\
http:// {{\n\
\thandle {INGRESS_VERIFY_PATH} {{\n\
\t\trespond \"{local_machine}\" 200\n\
\t}}\n\
\trespond \"Not Found\" 404\n\
\tlog\n\
}}\n\
\n\
(common_proxy) {{\n\
\t# Retry failed requests up to lb_retries times against other available upstreams.\n\
\tlb_retries 3\n\
\t# Upstreams are marked unhealthy for fail_duration after a failed request (passive health checking).\n\
\tfail_duration 30s\n\
}}\n"
    );
    if projection.sites.iter().any(|site| {
        site.route(HttpProtocol::Http).is_some()
            || site.challenge().is_some()
            || (site.route(HttpProtocol::Https).is_some() && site.material().is_some())
    }) {
        output.push_str("\n# Sites generated from Service ports.\n");
    }
    for site in &projection.sites {
        let http = site.route(HttpProtocol::Http);
        if http.is_some() || site.challenge().is_some() {
            write_site(
                &mut output,
                "http",
                &site.hostname,
                http.unwrap_or_default(),
                "",
                site.challenge(),
            );
        }
        let Some(material) = site.material() else {
            continue;
        };
        let Some(route) = site.route(HttpProtocol::Https) else {
            continue;
        };
        let stem = super::certificate_file_stem(&site.hostname, material);
        let tls =
            format!("\ttls {CONTAINER_CERTS_DIR}/{stem}.crt {CONTAINER_CERTS_DIR}/{stem}.key\n");
        write_site(&mut output, "https", &site.hostname, route, &tls, None);
    }
    write_certificate_errors(&mut output, &projection.sites);
    output
}

fn write_certificate_errors(output: &mut String, sites: &[IngressSite]) {
    let mut header = false;
    for site in sites {
        let Some(certificate) = &site.certificate else {
            continue;
        };
        if certificate.material.is_some() {
            continue;
        }
        let Some(error) = certificate
            .last_error
            .as_deref()
            .filter(|error| !error.is_empty())
        else {
            continue;
        };
        if !header {
            output.push_str("\n# Skipped certificate issuance:\n");
            header = true;
        }
        let _ = writeln!(output, "# - {}: {error}", site.hostname);
    }
}

fn write_global_options(output: &mut String, global_config: Option<&str>) {
    // Caddy never issues certificates. The daemon pins material when it has any.
    match global_config {
        Some(user) => {
            let _ = writeln!(
                output,
                "# User-defined global config from Service '{}'.",
                QualifiedService::system_ingress()
            );
            output.push_str(&merge_auto_https(user));
            output.push_str("\n\n");
        }
        None => output.push_str("{\n\tauto_https off\n}\n\n"),
    }
}

fn merge_auto_https(user: &str) -> String {
    let trimmed = user.trim();
    if let Some(inner) = trimmed
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        format!("{{\n\tauto_https off{inner}}}")
    } else {
        format!("{{\n\tauto_https off\n}}\n\n{user}")
    }
}

fn write_site(
    output: &mut String,
    protocol: &str,
    hostname: &IngressHost,
    endpoints: &[IngressEndpoint],
    tls: &str,
    challenge: Option<&CertificateChallenge>,
) {
    let handle = challenge
        .map(|challenge| {
            format!(
                "\thandle /.well-known/acme-challenge/{} {{\n\t\trespond \"{}\" 200\n\t}}\n",
                challenge.token(),
                challenge.response()
            )
        })
        .unwrap_or_default();
    let proxy = if endpoints.is_empty() {
        "\trespond \"Bad Gateway\" 502\n".to_owned()
    } else {
        format!(
            "\treverse_proxy {} {{\n\t\timport common_proxy\n\t}}\n",
            endpoints
                .iter()
                .map(|endpoint| format!("{}:{}", endpoint.address.0, endpoint.port))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let _ = write!(
        output,
        "\n{protocol}://{hostname} {{\n{tls}{handle}{proxy}\tlog\n}}\n"
    );
}

#[cfg(test)]
#[path = "../caddy_tests.rs"]
mod tests;
