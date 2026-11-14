//! The public trace page at `/.well-known/ployz/trace` and the `ployz-*` response headers.
//!
//! Caddy answers the page itself. Facts known at render time reach it as snippet arguments;
//! everything about the request stays a Caddy placeholder, so a request never reloads Caddy.

use ployz_core::{CLOUDFLARE_RANGES, MachineId, tunnel_origin_listener};
use std::fmt::Write as _;

use crate::ingress::{
    error_page::ErrorPage,
    projection::{IngressEndpoint, PublicationTrace},
};

/// Path the trace page answers on every routed hostname.
pub(crate) const TRACE_PATH: &str = "/.well-known/ployz/trace";

/// The `servers` global options. The public listeners read the visitor's address from
/// a Cloudflare peer, and the origin socket from the Tunnel door, its only peer. Caddy
/// applies an unaddressed block to every server, the socket's included, so each
/// listener is addressed by the address it binds.
pub(super) fn servers_options() -> String {
    let mut output = String::new();
    for address in [":80", ":443"] {
        let _ = write!(
            output,
            "\tservers {address} {{\n\t\ttrusted_proxies static {}\n\t\tclient_ip_headers CF-Connecting-IP\n\t}}\n",
            CLOUDFLARE_RANGES.join(" ")
        );
    }
    let _ = write!(
        output,
        "\tservers {} {{\n\t\ttrusted_proxies_unix\n\t\tclient_ip_headers CF-Connecting-IP X-Forwarded-For\n\t}}\n",
        tunnel_origin_listener()
    );
    output
}

/// One line of the page. Keys are only ever appended, so readers can rely on the order.
struct TraceField {
    key: &'static str,
    value: &'static str,
    cloudflare_only: bool,
}

const fn field(key: &'static str, value: &'static str) -> TraceField {
    TraceField {
        key,
        value,
        cloudflare_only: false,
    }
}

const fn cloudflare(key: &'static str, value: &'static str) -> TraceField {
    TraceField {
        key,
        value,
        cloudflare_only: true,
    }
}

/// The page's schema. Both the direct and the Cloudflare body walk it.
const TRACE_FIELDS: &[TraceField] = &[
    field("server", "{args[0]}"),
    field("deploy", "{args[1]}"),
    field("replicas", "{args[2]}"),
    field("status", "{args[3]}"),
    field("ip", "{http.request.remote.host}"),
    field("http", "{http.request.proto}"),
    field("tls", "{ployz_trace_tls}"),
    field("sni", "{ployz_trace_sni}"),
    field("request_id", "{http.request.uuid}"),
    field("ts", "{time.now.unix}"),
    cloudflare("via", "cloudflare"),
    cloudflare("cf_ray", "{ployz_trace_ray}"),
    cloudflare("client_ip", "{client_ip}"),
];

/// What a routed hostname's trace page reports as `status`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TraceStatus {
    /// This Server's Caddy has at least one endpoint for the hostname.
    Ok,
    /// The page the hostname answers with: NOT_RUNNING when it is published here but has no
    /// endpoint to proxy to, NOT_DEPLOYED when nothing is published for it on this Server.
    Failing(ErrorPage),
}

impl TraceStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failing(page) => page.as_str(),
        }
    }
}

/// `import ployz_headers <id>`: the `ployz-server` and `ployz-request-id` headers.
pub(super) fn headers_import(machine: &MachineId) -> String {
    format!("\timport ployz_headers {machine}\n")
}

/// `import ployz_trace` for a site that answers nothing published.
pub(super) fn unpublished_import(machine: &MachineId) -> String {
    import(
        machine,
        "\"\"",
        "\"\"",
        TraceStatus::Failing(ErrorPage::NotDeployed),
    )
}

/// `import ployz_trace` for a published site proxying to `endpoints`.
pub(super) fn published_import(
    machine: &MachineId,
    trace: &PublicationTrace,
    endpoints: &[IngressEndpoint],
) -> String {
    let deploy = trace
        .deploy
        .as_ref()
        .map_or_else(|| "\"\"".to_owned(), ToString::to_string);
    let placement = if trace.placement.is_empty() {
        "\"\"".to_owned()
    } else {
        trace
            .placement
            .iter()
            .map(|(machine, count)| format!("{machine}:{count}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let status = if endpoints.is_empty() {
        TraceStatus::Failing(ErrorPage::NotRunning)
    } else {
        TraceStatus::Ok
    };
    import(machine, &deploy, &placement, status)
}

fn import(machine: &MachineId, deploy: &str, placement: &str, status: TraceStatus) -> String {
    format!(
        "\timport ployz_trace {machine} {deploy} {placement} {}\n",
        status.as_str()
    )
}

/// The `ployz_headers` and `ployz_trace` snippets.
pub(super) fn write_snippets(output: &mut String) {
    // Deferred, so they replace any same-named header an app sends.
    output.push_str(
        "(ployz_headers) {\n\
\theader {\n\
\t\t>ployz-server {args[0]}\n\
\t\t>ployz-request-id {http.request.uuid}\n\
\t}\n\
\tlog_append request_id {http.request.uuid}\n\
}\n\
\n",
    );
    let _ = write!(
        output,
        "(ployz_trace) {{\n\
\t@ployz_trace_request {{\n\
\t\tmethod GET\n\
\t\tpath {TRACE_PATH}\n\
\t}}\n\
\thandle @ployz_trace_request {{\n\
\t\theader Content-Type \"text/plain; charset=utf-8\"\n\
\t\theader Cache-Control no-store\n"
    );
    // Values that reach the page from the request are matched whole, or left empty.
    write_map(
        output,
        "http.request.tls.version",
        "ployz_trace_tls",
        "tls[0-9]+[.][0-9]+",
    );
    write_map(
        output,
        "http.request.tls.server_name",
        "ployz_trace_sni",
        "[A-Za-z0-9._:-]+",
    );
    write_map(
        output,
        "http.request.header.Cf-Ray",
        "ployz_trace_ray",
        "[0-9A-Fa-f]{16,32}-[A-Z]{3}",
    );
    let _ = write!(
        output,
        "\t\t@ployz_trace_cloudflare remote_ip {}\n\
\t\thandle @ployz_trace_cloudflare {{\n",
        CLOUDFLARE_RANGES.join(" ")
    );
    write_body(output, "TRACE_CF", true);
    output.push_str("\t\t}\n\t\thandle {\n");
    write_body(output, "TRACE_DIRECT", false);
    output.push_str("\t\t}\n\t}\n}\n\n");
}

fn write_map(output: &mut String, input: &str, output_name: &str, pattern: &str) {
    let _ = write!(
        output,
        "\t\tmap {{{input}}} {{{output_name}}} {{\n\
\t\t\t~^({pattern})$ \"${{1}}\"\n\
\t\t\tdefault \"\"\n\
\t\t}}\n"
    );
}

/// One heredoc body. The blank line before the end marker keeps the trailing newline.
fn write_body(output: &mut String, marker: &str, cloudflare: bool) {
    let _ = writeln!(output, "\t\t\trespond <<{marker}");
    for field in TRACE_FIELDS
        .iter()
        .filter(|field| cloudflare || !field.cloudflare_only)
    {
        let _ = writeln!(output, "{}={}", field.key, field.value);
    }
    let _ = write!(output, "\n{marker} 200\n");
}
