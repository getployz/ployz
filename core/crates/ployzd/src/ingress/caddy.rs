//! Deterministic Caddy configuration rendering and application.

use chrono::{SecondsFormat, Utc};
use ployz_core::{
    HOSTNAME_VERIFY_PATH, HttpProtocol, INGRESS_VERIFY_PATH, IngressHost, Machine, MachineId,
    ingress_upstream,
};
use reqwest::{Client, StatusCode, header};
use serde_json::Value;
use std::{
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
    corrosion::{CertificateChallenge, ReplicatedStore},
    filesystem::{atomic_write, set_ployz_group},
    ingress::{IngressEndpoint, IngressProjection, IngressSite, prepare_directory},
};

pub const CONFIG_FILE: &str = "Caddyfile";
const CONTAINER_CERTS_DIR: &str = "/config/caddy/certs";
const ADMIN_TIMEOUT: Duration = Duration::from_secs(5);
/// Set to any value to turn on Caddy's debug log: every upstream selection, failure and retry.
pub(crate) const DEBUG_ENV: &str = "PLOYZ_INGRESS_DEBUG";
/// Browsers get this page; other clients get JSON or plain text with the same code.
const ERROR_PAGE: &str = include_str!("error_page.html");

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
/// Returns a filesystem, validation, or acknowledged-load error.
pub(crate) async fn reconcile<A: CaddyAdmin>(
    projection: &IngressProjection,
    config_file: &Path,
    admin: Option<&A>,
) -> Result<(), Error> {
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let caddyfile = render_caddyfile(projection, &timestamp);
    tracing::info!(routes = %route_summary(projection), "ingress config");
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
    let _ = write!(output, "{{\n{debug}\tauto_https off\n}}\n\n");
    write_error_page(&mut output);
    let ingress_verify = verify_handle(INGRESS_VERIFY_PATH, local_machine);
    let hostname_verify = verify_handle(HOSTNAME_VERIFY_PATH, local_machine);
    let _ = write!(
        output,
        "# Health check endpoint to verify Caddy reachability on this Machine.\n\
http:// {{\n\
{ingress_verify}{hostname_verify}\
\terror DOMAIN_NOT_FOUND 404\n\
\timport error_page\n\
\tlog\n\
}}\n\
\n\
# Unknown hostnames over HTTPS: a wildcard certificate another site pins may cover them,\n\
# and Caddy would answer an unmatched request 200 with an empty body.\n\
https:// {{\n\
\terror DOMAIN_NOT_FOUND 404\n\
\timport error_page\n\
\tlog\n\
}}\n\
\n\
(common_proxy) {{\n\
\t# Retry failed requests up to lb_retries times against other available upstreams.\n\
\tlb_retries 3\n\
\t# Passive health checking: an upstream failing max_fails requests within fail_duration is\n\
\t# skipped until they expire, so one stray error never ejects a Service's only upstream.\n\
\tfail_duration 30s\n\
\tmax_fails 2\n\
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
            // The hostname's own site shadows the catch-all, so it answers the verify probe too.
            write_site(
                &mut output,
                "http",
                &site.hostname,
                http.unwrap_or_default(),
                &hostname_verify,
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

/// Each routed hostname with the upstreams it proxies to, e.g. `http://web.test=[10.0.0.1:8080]`.
fn route_summary(projection: &IngressProjection) -> String {
    let mut summary = String::new();
    for site in &projection.sites {
        for (protocol, scheme) in [(HttpProtocol::Http, "http"), (HttpProtocol::Https, "https")] {
            let Some(endpoints) = site.route(protocol) else {
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

/// The `error_page` snippet: one code per failure, negotiated as HTML, JSON or plain text.
///
/// Ployz's own errors carry their code as the message. Any proxy failure is `NO_RESPONSE`: a
/// refused or timed-out dial is 502, then passive health empties the pool and the rest are 503.
fn write_error_page(output: &mut String) {
    output.push_str(
        r##"(error_page) {
	handle_errors {
		map "{err.status_code} {err.message}" {ployz_error} {ployz_message} {ployz_docs} {
			"503 NOT_RUNNING" NOT_RUNNING "The app isn't running." "#this-app-isnt-running"
			"404 DOMAIN_NOT_FOUND" DOMAIN_NOT_FOUND "Nothing is deployed at this domain." "#nothing-is-deployed-here"
			~^50[234] NO_RESPONSE "The app didn't respond." "#this-app-didnt-respond"
			default INTERNAL_ERROR "Something went wrong." ""
		}
		header {
			ployz-error {ployz_error}
			ployz-request-id {http.request.uuid}
			cache-control no-store
		}
		@not_running vars {ployz_error} NOT_RUNNING
		header @not_running {
			retry-after 10
			refresh 10
		}
		log_append request_id {http.request.uuid}
		@html header Accept *text/html*
		handle @html {
			header content-type "text/html; charset=utf-8"
			respond <<ERROR_PAGE
"##,
    );
    output.push_str(ERROR_PAGE);
    output.push_str(
        r##"ERROR_PAGE {err.status_code}
		}
		@json header Accept *json*
		handle @json {
			header content-type application/json
			respond `{"error":{"status":{err.status_code},"code":"{ployz_error}","message":"{ployz_message}","request_id":"{http.request.uuid}","docs":"https://ployz.dev/docs/troubleshooting/app-not-reachable?utm_source=error-page&utm_medium=json&utm_campaign={ployz_error}&utm_content=docs{ployz_docs}"}}` {err.status_code}
		}
		handle {
			header content-type "text/plain; charset=utf-8"
			respond <<TEXT
				{err.status_code} {ployz_error}: {ployz_message}
				Request: {http.request.uuid}
				https://ployz.dev/docs/troubleshooting/app-not-reachable?utm_source=error-page&utm_medium=text&utm_campaign={ployz_error}&utm_content=docs{ployz_docs}

				TEXT {err.status_code}
		}
	}
}

"##,
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
            output.push_str("\n# Skipped certificate issuance:\n");
            header = true;
        }
        let _ = writeln!(output, "# - {}: {error}", site.hostname);
    }
}

fn write_site(
    output: &mut String,
    protocol: &str,
    hostname: &IngressHost,
    endpoints: &[IngressEndpoint],
    directives: &str,
    challenge: Option<&CertificateChallenge>,
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
    let proxy = if endpoints.is_empty() {
        "\terror NOT_RUNNING 503\n".to_owned()
    } else {
        format!(
            "\treverse_proxy {} {{\n\t\timport common_proxy\n\t}}\n\
\tlog_append upstream {{http.reverse_proxy.upstream.hostport}}\n",
            endpoints
                .iter()
                .map(|endpoint| ingress_upstream(endpoint.address, endpoint.port))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let _ = write!(
        output,
        "\n{protocol}://{hostname} {{\n{directives}{handle}{proxy}\timport error_page\n\tlog\n}}\n"
    );
}

#[cfg(test)]
#[path = "../caddy_tests.rs"]
mod tests;
