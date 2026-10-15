//! The public trace page at `/.well-known/ployz/trace` and the `ployz-*` response headers.
//!
//! Caddy answers the page itself. Facts known at render time reach it as snippet arguments;
//! everything about the request stays a Caddy placeholder, so a request never reloads Caddy.

use ployz_core::MachineId;
use std::fmt::Write as _;

use crate::ingress::{IngressEndpoint, PublicationTrace};

/// Path the trace page answers on every routed hostname.
pub(crate) const TRACE_PATH: &str = "/.well-known/ployz/trace";

/// Cloudflare's published edge ranges, checked 2026-10-13 against
/// <https://www.cloudflare.com/ips-v4> and <https://www.cloudflare.com/ips-v6>.
/// Only a peer in these ranges may set the client address or get the Cloudflare keys.
pub(super) const CLOUDFLARE_RANGES: [&str; 22] = [
    "173.245.48.0/20",
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "141.101.64.0/18",
    "108.162.192.0/18",
    "190.93.240.0/20",
    "188.114.96.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    "162.158.0.0/15",
    "104.16.0.0/13",
    "104.24.0.0/14",
    "172.64.0.0/13",
    "131.0.72.0/22",
    "2400:cb00::/32",
    "2606:4700::/32",
    "2803:f800::/32",
    "2405:b500::/32",
    "2405:8100::/32",
    "2a06:98c0::/29",
    "2c0f:f248::/32",
];

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
    /// The hostname is published here but has no endpoint to proxy to.
    NotRunning,
    /// Nothing is published for the hostname on this Server.
    NotDeployed,
}

impl TraceStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotRunning => "NOT_RUNNING",
            Self::NotDeployed => "NOT_DEPLOYED",
        }
    }
}

/// `import ployz_headers <id>`: the `ployz-server` and `ployz-request-id` headers.
pub(super) fn headers_import(machine: &MachineId) -> String {
    format!("\timport ployz_headers {machine}\n")
}

/// `import ployz_trace` for a site that answers nothing published.
pub(super) fn unpublished_import(machine: &MachineId) -> String {
    import(machine, "\"\"", "\"\"", TraceStatus::NotDeployed)
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
        TraceStatus::NotRunning
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
