//! Deterministic Caddy configuration rendering and application.

use chrono::{SecondsFormat, Utc};
use ployz_core::{
    HOSTNAME_VERIFY_PATH, HttpProtocol, INGRESS_RUNTIME_MOUNT, INGRESS_VERIFY_PATH, Machine,
    MachineId, TUNNEL_SITE_PORT, ingress_certs_dir, ingress_upstream, tunnel_origin_listener,
};
use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    future::Future,
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::{CertificateChallenge, Error as CorrosionError, ReplicatedStore},
    filesystem::{atomic_write, set_ployz_group},
    ingress::{
        certificates::certificate_file_stem,
        error_page::{ErrorPage, write_error_page},
        projection::{
            IngressEndpoint, IngressProjection, IngressSite, Serve, Unrouted, prepare_directory,
        },
    },
};

mod trace;

pub const CONFIG_FILE: &str = "Caddyfile";
/// Request fields that name the visitor. Every header goes, since any of them can carry an address.
const VISITOR_FIELDS: [&str; 3] = ["request>remote_ip", "request>client_ip", "request>headers"];
/// Fields Caddy's debug logs put the client address in outside any request, as TLS handshakes do.
/// `http.stdlib` and `events` put it in a message or nested payload no filter reaches, so the
/// default log drops those two loggers.
const CONNECTION_FIELDS: [&str; 3] = ["remote_ip", "remote", "client_ip"];
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Set to any value to turn on Caddy's debug log: every upstream selection, failure and retry.
pub(crate) const DEBUG_ENV: &str = "PLOYZ_INGRESS_DEBUG";

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
}

/// Caddy's concrete validation and acknowledged-load interface.
pub(crate) trait CaddyAdmin: Send + Sync {
    /// Adapt and validate one Caddyfile without applying it.
    fn adapt(&self, caddyfile: &str) -> impl Future<Output = Result<String, Error>> + Send;
    /// Load adapted Caddy JSON and wait for acknowledgement.
    fn load(&self, json: &str) -> impl Future<Output = Result<(), Error>> + Send;
    /// File stems of the certificates the running config loads.
    fn loaded_certificates(&self) -> impl Future<Output = Result<BTreeSet<String>, Error>> + Send;
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
        Self::body(response).await
    }

    async fn get(&self, path: &str) -> Result<String, Error> {
        Self::body(
            self.client
                .get(format!("http://localhost{path}"))
                .send()
                .await?,
        )
        .await
    }

    async fn body(response: reqwest::Response) -> Result<String, Error> {
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

    async fn loaded_certificates(&self) -> Result<BTreeSet<String>, Error> {
        // Caddy refuses the path when the running config has no TLS app, so it loads nothing.
        let files = match self.get("/config/apps/tls/certificates/load_files").await {
            Ok(files) => files,
            Err(Error::Admin(_)) => return Ok(BTreeSet::new()),
            Err(error) => return Err(error),
        };
        let prefix = format!("{}/", ingress_certs_dir());
        Ok(serde_json::from_str::<Option<Vec<Value>>>(&files)?
            .unwrap_or_default()
            .iter()
            .filter_map(|file| {
                let path = file.get("certificate")?.as_str()?;
                Some(path.strip_prefix(&prefix)?.strip_suffix(".crt")?.to_owned())
            })
            .collect())
    }
}

/// Make the directory Caddy binds the origin socket in, since Caddy makes no parent
/// directory of a socket. The Tunnel Origin mounts it alone, and other users may enter
/// it to reach the socket but list nothing.
///
/// # Errors
///
/// Returns when the directory can't be made or its mode set.
pub(crate) fn prepare_origin_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o711))
}

#[must_use]
/// Return the Caddy configuration path beneath the shared ingress data root.
pub(crate) fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("ingress").join("caddy").join(CONFIG_FILE)
}

pub async fn run<LoadDown, DownFuture>(
    machine: Machine,
    replicated: ReplicatedStore,
    load_down: LoadDown,
    config_file: PathBuf,
    admin_socket: PathBuf,
    route_publishes: tokio::sync::watch::Sender<super::PublishedRoutes>,
    shutdown: CancellationToken,
) -> io::Result<()>
where
    LoadDown: FnMut() -> DownFuture,
    DownFuture: Future<Output = Result<BTreeSet<MachineId>, CorrosionError>>,
{
    prepare_directory(
        admin_socket
            .parent()
            .ok_or_else(|| io::Error::other("Caddy admin socket has no parent"))?,
    )?;
    super::watch_caddy(
        machine,
        replicated,
        load_down,
        config_file,
        route_publishes,
        shutdown,
        move || {
            let admin_socket = admin_socket.clone();
            async move { AdminClient::connect_if_available(&admin_socket).await }
        },
    )
    .await
}

/// Render one already-derived projection and load it when Caddy runs, returning the
/// Caddyfile to persist.
///
/// # Errors
///
/// Returns a validation or acknowledged-load error.
pub(crate) async fn load<A: CaddyAdmin>(
    projection: &IngressProjection,
    admin: Option<&A>,
) -> Result<String, Error> {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let caddyfile = render_caddyfile(projection, &timestamp);
    tracing::info!(routes = %route_summary(projection), "ingress config");
    if let Some(admin) = admin {
        let json = admin.adapt(&caddyfile).await?;
        admin.load(&json).await?;
    }
    Ok(caddyfile)
}

pub(crate) fn write_caddyfile(path: &Path, caddyfile: &str) -> Result<(), Error> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Caddyfile has no parent"))?;
    prepare_directory(directory)?;
    atomic_write(path, caddyfile.as_bytes(), 0o640)?;
    set_ployz_group(path)?;
    Ok(())
}

fn render_caddyfile(projection: &IngressProjection, timestamp: &str) -> String {
    let machine_name = projection.machine.name.as_str();
    let local_machine = &projection.machine.id;
    let mut output = format!(
        "# Caddyfile autogenerated by Ployz on Machine '{machine_name}' (DO NOT EDIT): {timestamp}\n\
# Automatically updated on Service or health status changes.\n\
# Docs: https://github.com/getployz/ployz\n\
\n"
    );
    // Caddy never issues certificates. The daemon pins material when it has any.
    let debug = if std::env::var_os(DEBUG_ENV).is_some() {
        "\tdebug\n"
    } else {
        ""
    };
    let _ = write!(
        output,
        "{{\n{debug}\tauto_https off\n{}\tlog default {{\n\t\texclude http.stdlib events\n{}\t}}\n}}\n\n",
        trace::servers_options(),
        visitor_filter(
            "\t\t",
            &[&VISITOR_FIELDS[..], &CONNECTION_FIELDS[..]].concat()
        ),
    );
    trace::write_snippets(&mut output);
    write_log_snippets(&mut output);
    write_error_page(&mut output);
    let ingress_verify = verify_handle(INGRESS_VERIFY_PATH, local_machine);
    let hostname_verify = verify_handle(HOSTNAME_VERIFY_PATH, local_machine);
    let headers = trace::headers_import(local_machine);
    let unpublished = trace::unpublished_import(local_machine);
    let not_deployed = ErrorPage::NotDeployed.directive();
    let _ = write!(
        output,
        "# Health check endpoint to verify Caddy reachability on this Machine.\n\
http:// {{\n\
{headers}{unpublished}{ingress_verify}{hostname_verify}\
{not_deployed}\
\timport error_page {local_machine}\n\
\timport ployz_log\n\
}}\n\
\n\
# Unknown hostnames over HTTPS: a wildcard certificate another site pins may cover them,\n\
# and Caddy would answer an unmatched request 200 with an empty body.\n\
https:// {{\n\
{headers}{unpublished}\
{not_deployed}\
\timport error_page {local_machine}\n\
\timport ployz_log\n\
}}\n\
\n\
(common_proxy) {{\n\
\t# A slow upstream holds more requests open, so least_conn sends it fewer new ones.\n\
\tlb_policy least_conn\n\
\t# Retry failed requests up to lb_retries times against other available upstreams.\n\
\tlb_retries 3\n\
\t# Passive health checking: an upstream failing max_fails requests within fail_duration is\n\
\t# skipped until they expire, so one stray error never ejects a Service's only upstream.\n\
\tfail_duration 30s\n\
\tmax_fails 2\n\
\t# A trusted Cloudflare peer would pass its incoming X-Forwarded-* through; the app gets only\n\
\t# what Caddy vouches for, so trusting Cloudflare changes nothing but the client address.\n\
\theader_up X-Forwarded-For {{client_ip}}\n\
\theader_up CF-Connecting-IP {{client_ip}}\n\
\theader_up X-Real-IP {{client_ip}}\n\
\theader_up X-Forwarded-Host {{hostport}}\n\
\theader_up X-Forwarded-Proto {{scheme}}\n\
\t# A powered-off upstream's Machine never answers the dial; give up fast and retry another.\n\
\t# A request sent on an idle kept-alive connection to it still waits about 14s for retransmits.\n\
\t# An upstream that accepts but never answers would keep its share of traffic, since only an\n\
\t# errored request counts toward max_fails. 60s matches nginx and AWS ALB, under Cloudflare's\n\
\t# 100s; SSE and websockets send their headers at once, so streams outlive it.\n\
\ttransport http {{\n\
\t\tdial_timeout 500ms\n\
\t\tresponse_header_timeout 60s\n\
\t}}\n\
}}\n\
\n\
# The Tunnel door reaches this Machine's public routes on the origin socket and terminates TLS\n\
# itself. It says what the visitor used in X-Forwarded-Proto, which defaults to https.\n\
(tunnel_site) {{\n\
\tbind {}\n\
\timport ployz_headers {{args[0]}}\n\
\t@plain header X-Forwarded-Proto http\n\
\tredir @plain https://{{host}}{{uri}} 308\n\
\t@noproto header !X-Forwarded-Proto\n\
\trequest_header @noproto X-Forwarded-Proto https\n\
}}\n\
\n\
# The later header_up replaces the scheme Caddy saw with the one the door reported.\n\
(tunnel_proxy) {{\n\
\timport common_proxy\n\
\theader_up X-Forwarded-Proto {{header.X-Forwarded-Proto}}\n\
}}\n",
        tunnel_origin_listener(),
    );
    write_tunnel_sites(&mut output, projection, &unpublished, &hostname_verify);
    if projection
        .sites
        .iter()
        .any(|site| site.http().is_some() || site.https().is_some())
    {
        output.push_str("\n# Sites generated from Service ports.\n");
    }
    for site in &projection.sites {
        if let Some(serve) = site.http() {
            let (trace, answer) = answer(site, serve, local_machine, &unpublished);
            // The hostname's own site shadows the catch-all, so it answers the verify probe too.
            write_site(
                &mut output,
                local_machine,
                "http",
                site.hostname.as_str(),
                &format!("{headers}{trace}{hostname_verify}"),
                site.challenge(),
                &answer,
            );
        }
        let Some((material, serve)) = site.https() else {
            continue;
        };
        let stem = certificate_file_stem(&site.hostname, material);
        let certs = ingress_certs_dir();
        let tls = format!("\ttls {certs}/{stem}.crt {certs}/{stem}.key\n");
        let (trace, answer) = answer(site, serve, local_machine, &unpublished);
        write_site(
            &mut output,
            local_machine,
            "https",
            site.hostname.as_str(),
            &format!("{tls}{headers}{trace}"),
            None,
            &answer,
        );
    }
    write_certificate_errors(&mut output, &projection.sites);
    output
}

/// The trace and `ployz_requests` imports of a routed site, which reports its Service.
fn published(site: &IngressSite, machine: &MachineId, endpoints: &[IngressEndpoint]) -> String {
    let publication = site
        .publication
        .as_ref()
        .expect("a routed site has a publication");
    format!(
        "{}\timport ployz_requests {} {}\n",
        trace::published_import(machine, &publication.trace, endpoints),
        publication.owner.namespace,
        publication.owner.name
    )
}

/// The trace imports and the answer of one public listener's site. The redirect's trace
/// reports the HTTPS route it sends visitors to, and an unrouted site publishes nothing, so it
/// answers like the catch-all.
fn answer(
    site: &IngressSite,
    serve: Serve<'_>,
    machine: &MachineId,
    unpublished: &str,
) -> (String, String) {
    match serve {
        Serve::Proxy(endpoints) => (published(site, machine, endpoints), proxy(endpoints)),
        Serve::RedirectToHttps(endpoints) => (
            published(site, machine, endpoints),
            REDIRECT_TO_HTTPS.to_owned(),
        ),
        Serve::Unrouted(Unrouted::NotDeployed) => (
            format!("{unpublished}\timport ployz_log\n"),
            ErrorPage::NotDeployed.directive(),
        ),
    }
}

/// The sites the Tunnel door reaches on the origin socket: one per routed hostname, with or
/// without Certificate Material, since the door terminates TLS itself, and a catch-all that
/// answers a hostname it does not route like the public one. The socket serves nothing else,
/// so a Host header a tunnel's route sets reaches only a route the public listeners serve.
fn write_tunnel_sites(
    output: &mut String,
    projection: &IngressProjection,
    unpublished: &str,
    hostname_verify: &str,
) {
    let local_machine = &projection.machine.id;
    let port = TUNNEL_SITE_PORT;
    let tunnel = format!("\timport tunnel_site {local_machine}\n");
    let not_deployed = ErrorPage::NotDeployed.directive();
    let _ = write!(
        output,
        "\n# Unknown hostnames through the Tunnel door.\n\
http://:{port} {{\n\
{tunnel}{unpublished}{hostname_verify}{not_deployed}\
\timport error_page {local_machine}\n\
\timport ployz_log\n\
}}\n"
    );
    for site in &projection.sites {
        let Some(endpoints) = site.door() else {
            continue;
        };
        let trace = published(site, local_machine, endpoints);
        write_site(
            output,
            local_machine,
            "http",
            &format!("{}:{port}", site.hostname),
            &format!("{tunnel}{trace}{hostname_verify}"),
            None,
            &tunnel_proxy(endpoints),
        );
    }
}

/// Each routed hostname with the upstreams it proxies to, e.g. `http://web.test=[10.0.0.1:8080]`.
fn route_summary(projection: &IngressProjection) -> String {
    let mut summary = String::new();
    for site in &projection.sites {
        for (protocol, scheme) in [(HttpProtocol::Http, "http"), (HttpProtocol::Https, "https")] {
            let Some(endpoints) = site.proxied(protocol) else {
                continue;
            };
            let upstreams = endpoints
                .iter()
                .map(|endpoint| ingress_upstream(endpoint.address, endpoint.port))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = write!(summary, " {scheme}://{}=[{upstreams}]", site.hostname);
        }
    }
    summary
}

fn visitor_filter(indent: &str, fields: &[&str]) -> String {
    let mut filter = format!("{indent}format filter {{\n");
    for field in fields {
        let _ = writeln!(filter, "{indent}\t{field} delete");
    }
    let _ = writeln!(filter, "{indent}}}");
    filter
}

fn write_log_snippets(output: &mut String) {
    let _ = write!(
        output,
        "(ployz_log) {{\n\tlog {{\n{}\t}}\n}}\n\n\
(ployz_requests) {{\n\
\tlog_append namespace {{args[0]}}\n\
\tlog_append service {{args[1]}}\n\
\tlog ployz_requests {{\n\
\t\toutput file {INGRESS_RUNTIME_MOUNT}/{} {{\n\
\t\t\troll_size 10MiB\n\
\t\t\troll_keep 1\n\
\t\t\troll_uncompressed\n\
\t\t\tmode 0600\n\
\t\t}}\n\
\t}}\n\
}}\n\n",
        visitor_filter("\t\t", &VISITOR_FIELDS),
        super::ACCESS_LOG,
    );
}

/// A site handle that answers `path` with this Machine's id.
fn verify_handle(path: &str, machine: &MachineId) -> String {
    format!("\thandle {path} {{\n\t\trespond \"{machine}\" 200\n\t}}\n")
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
            output.push_str("\n# Certificates not served:\n");
            header = true;
        }
        // CA error detail can quote what the domain served, so it stays on this comment line.
        let error: String = error
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect();
        let _ = writeln!(output, "# - {}: {error}", site.hostname);
    }
}

/// A matcherless `handle` runs after the path handles, unlike a bare `redir`, which Caddy orders first.
/// Browsers cache a 308 forever by default; a short max-age lets a later http route take over.
const REDIRECT_TO_HTTPS: &str = "\thandle {\n\t\theader cache-control \"max-age=300\"\n\t\tredir https://{host}{uri} 308\n\t}\n";

/// Proxies to `endpoints`, or answers `NOT_RUNNING` when there are none.
fn proxy(endpoints: &[IngressEndpoint]) -> String {
    proxy_with(endpoints, "common_proxy")
}

/// The same through the Tunnel door, which already said the visitor's scheme.
fn tunnel_proxy(endpoints: &[IngressEndpoint]) -> String {
    proxy_with(endpoints, "tunnel_proxy")
}

fn proxy_with(endpoints: &[IngressEndpoint], snippet: &str) -> String {
    if endpoints.is_empty() {
        return ErrorPage::NotRunning.directive();
    }
    format!(
        "\treverse_proxy {} {{\n\t\timport {snippet}\n\t}}\n\
\tlog_append upstream {{http.reverse_proxy.upstream.hostport}}\n",
        endpoints
            .iter()
            .map(|endpoint| ingress_upstream(endpoint.address, endpoint.port))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn write_site(
    output: &mut String,
    machine: &MachineId,
    protocol: &str,
    address: &str,
    directives: &str,
    challenge: Option<&CertificateChallenge>,
    answer: &str,
) {
    let handle = challenge
        .map(|challenge| {
            format!(
                "\thandle /.well-known/acme-challenge/{} {{\n\t\trespond {} 200\n\t}}\n",
                challenge.token(),
                serde_json::to_string(challenge.response())
                    .expect("string serialization cannot fail")
            )
        })
        .unwrap_or_default();
    let _ = write!(
        output,
        "\n{protocol}://{address} {{\n{directives}{handle}{answer}\timport error_page {machine}\n}}\n"
    );
}

#[cfg(test)]
#[path = "../caddy_tests.rs"]
mod tests;
