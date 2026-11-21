use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::Namespace;
use serde_json::{Value, json};

use crate::observe::salts::Salt;

pub(super) const REQUESTS_MOUNT: &str = "/var/lib/ployz/requests";

const PERSONAL: &str = r"(?i:[A-Za-z0-9._%+-]+(?:@|%40)[A-Za-z0-9.-]+\.[A-Za-z]{2,}|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*|[A-Za-z0-9_-]{32,})";
const QUERY_KEYS: [&str; 6] = [
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "ref",
];
const KEPT: [&str; 22] = [
    "http.request.method",
    "ployz.request.namespace",
    "ployz.request.service",
    "server.address",
    "url.path",
    "http.response.status_code",
    "ployz.request.duration_us",
    "ployz.request.bytes_in",
    "ployz.request.bytes_out",
    "ployz.request.id",
    "user_agent.original",
    "ployz.request.referrer_host",
    "ployz.request.language",
    "ployz.request.content_type",
    "ployz.request.sec_purpose",
    "ployz.stream",
    "ployz.request.utm_source",
    "ployz.request.utm_medium",
    "ployz.request.utm_campaign",
    "ployz.request.utm_term",
    "ployz.request.utm_content",
    "ployz.request.ref",
];

struct Window<'a> {
    namespace: &'a Namespace,
    salt: &'a Salt,
}

fn requests_receiver() -> Value {
    json!({
        "include": [format!("{REQUESTS_MOUNT}/access*.log")],
        "start_at": "end",
        "poll_interval": "500ms",
        "storage": "file_storage",
        "operators": [{
            "type": "json_parser",
            "parse_from": "body",
            "parse_to": "attributes",
            "on_error": "drop",
        }],
    })
}

/// The Collector config that ships every access-log line of the sites Cloud lists. A line
/// leaves the Machine with its client address already hashed into a visitor, so Cloud never
/// holds one. Where Cloud lists no site the file is still read and every line dropped, so
/// its checkpoint keeps up and a site turned on later counts nothing from while it was off.
pub(super) fn fragment(sites: &[(Namespace, Vec<Salt>)]) -> String {
    if sites.is_empty() {
        let config: Value = json!({
            "receivers": { "filelog/requests": requests_receiver() },
            "processors": {
                "filter/requests": {
                    "error_mode": "silent",
                    "logs": { "log_record": ["true"] },
                },
            },
            "service": {
                "pipelines": {
                    "logs/requests": {
                        "receivers": ["filelog/requests"],
                        "processors": ["filter/requests"],
                        "exporters": ["otlp_http"],
                    }
                }
            },
        });
        return serde_json::to_string_pretty(&config)
            .expect("a requests fragment serializes to JSON");
    }
    let windows: Vec<Window<'_>> = sites
        .iter()
        .flat_map(|(namespace, salts)| salts.iter().map(move |salt| Window { namespace, salt }))
        .collect();
    let mut processors = serde_json::Map::new();
    processors.insert(
        "filter/requests".to_owned(),
        json!({
            "error_mode": "silent",
            "logs": { "log_record": [drop_other_sites(sites)] },
        }),
    );
    processors.insert(
        "transform/requests".to_owned(),
        json!({
            "error_mode": "silent",
            "log_statements": [{ "context": "log", "statements": statements(&windows) }],
        }),
    );
    let mut pipeline = vec![
        "memory_limiter".to_owned(),
        "filter/requests".to_owned(),
        "transform/requests".to_owned(),
    ];
    for (n, window) in windows.iter().enumerate() {
        processors.insert(
            format!("redaction/{n}"),
            json!({
                "allow_all_keys": true,
                "summary": "silent",
                "hash_function": "hmac-sha256",
                "hmac_key": URL_SAFE_NO_PAD.encode(window.salt.salt),
                "blocked_key_patterns": [format!("^{}$", regex::escape(&visitor_key(n)))],
            }),
        );
        pipeline.push(format!("redaction/{n}"));
    }
    processors.insert(
        "transform/visitors".to_owned(),
        json!({
            "error_mode": "silent",
            "log_statements": [{ "context": "log", "statements": visitor_statements(windows.len()) }],
        }),
    );
    pipeline.push("transform/visitors".to_owned());
    pipeline.push("batch".to_owned());
    let config: Value = json!({
        "receivers": { "filelog/requests": requests_receiver() },
        "processors": processors,
        "service": {
            "pipelines": {
                "logs/requests": {
                    "receivers": ["filelog/requests"],
                    "processors": pipeline,
                    "exporters": ["otlp_http"],
                }
            }
        },
    });
    serde_json::to_string_pretty(&config).expect("a requests fragment serializes to JSON")
}

fn visitor_key(n: usize) -> String {
    format!("v.{n}")
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes to JSON")
}

fn drop_other_sites(sites: &[(Namespace, Vec<Salt>)]) -> String {
    let others = sites
        .iter()
        .map(|(namespace, _)| {
            format!(
                "attributes[\"namespace\"] != {}",
                quoted(namespace.as_str())
            )
        })
        .collect::<Vec<_>>()
        .join(" and ");
    format!("attributes[\"namespace\"] == nil or ({others})")
}

/// Cuts by character, so a clipped value stays valid UTF-8, which the OTLP encoding needs.
fn clip(to: &str, from: &str, max: usize) -> String {
    format!("set(attributes[\"{to}\"], {})", clip_value(from, max))
}

fn statements(windows: &[Window<'_>]) -> Vec<String> {
    let request = |field: &str| format!("attributes[\"request\"][\"{field}\"]");
    let header = |name: &str| format!("attributes[\"request\"][\"headers\"][\"{name}\"][0]");
    let mut out = vec![
        "set(time_unix_nano, Int(attributes[\"ts\"] * 1000000000.0))".to_owned(),
        format!(
            "set(attributes[\"raw_host\"], ConvertCase(ExtractPatterns({}, \"^(?P<h>\\\\[[^\\\\]]+\\\\]|[^:]+)\")[\"h\"], \"lower\"))",
            request("host")
        ),
        format!("set(attributes[\"raw_uri\"], {})", request("uri")),
        "set(attributes[\"raw_path\"], ExtractPatterns(attributes[\"raw_uri\"], \"^(?s)(?P<p>[^?]{0,1000})\")[\"p\"])".to_owned(),
        "set(attributes[\"raw_query\"], ExtractPatterns(attributes[\"raw_uri\"], \"^[^?]*\\\\?(?P<q>.*)$\")[\"q\"])".to_owned(),
    ];
    for key in QUERY_KEYS {
        let attribute = format!("q_{key}");
        let pattern = format!("(?:^|&){key}=(?P<v>[^&]{{0,256}})");
        out.push(format!(
            "set(attributes[\"{attribute}\"], ExtractPatterns(attributes[\"raw_query\"], {})[\"v\"])",
            quoted(&pattern)
        ));
        out.push(format!(
            "replace_pattern(attributes[\"{attribute}\"], {}, \"redacted\")",
            quoted(PERSONAL)
        ));
    }
    out.push(clip("ua", &header("User-Agent"), 512));
    out.push("set(attributes[\"ua\"], \"\") where attributes[\"ua\"] == nil".to_owned());
    out.extend([
        format!(
            "set(attributes[\"ref_host\"], ConvertCase(ExtractPatterns({}, \"^[A-Za-z][A-Za-z0-9+.-]*://(?:[^/?#@]*@)?(?P<h>[^/?#:]{{1,253}})\")[\"h\"], \"lower\"))",
            header("Referer")
        ),
        "delete_key(attributes, \"ref_host\") where attributes[\"ref_host\"] == attributes[\"raw_host\"]".to_owned(),
        format!(
            "set(attributes[\"lang\"], ConvertCase(ExtractPatterns({}, \"^\\\\s*(?P<l>[A-Za-z]{{1,8}})(?:[-;,\\\\s]|$)\")[\"l\"], \"lower\"))",
            header("Accept-Language")
        ),
        clip("purpose", &header("Sec-Purpose"), 32),
        format!(
            "set(attributes[\"purpose\"], {}) where attributes[\"purpose\"] == nil or attributes[\"purpose\"] == \"\"",
            clip_value(&header("Purpose"), 32)
        ),
        clip(
            "ctype",
            "attributes[\"resp_headers\"][\"Content-Type\"][0]",
            128,
        ),
        clip("method", &request("method"), 16),
        clip("rid", "attributes[\"request_id\"]", 64),
    ]);
    for (n, window) in windows.iter().enumerate() {
        out.push(format!(
            "set(attributes[\"{}\"], Concat([{}, \"|\", {}, \"|\", attributes[\"ua\"]], \"\")) where attributes[\"namespace\"] == {} and time_unix_nano >= {} and time_unix_nano < {}",
            visitor_key(n),
            quoted(window.namespace.as_str()),
            request("client_ip"),
            quoted(window.namespace.as_str()),
            window.salt.start.saturating_mul(1_000_000_000),
            window.salt.end.saturating_mul(1_000_000_000),
        ));
    }
    let wire: [(&str, &str); 16] = [
        ("http.request.method", "attributes[\"method\"]"),
        ("ployz.request.namespace", "attributes[\"namespace\"]"),
        ("ployz.request.service", "attributes[\"service\"]"),
        ("server.address", "attributes[\"raw_host\"]"),
        ("url.path", "attributes[\"raw_path\"]"),
        ("http.response.status_code", "Int(attributes[\"status\"])"),
        (
            "ployz.request.duration_us",
            "Int(attributes[\"duration\"] * 1000000.0)",
        ),
        ("ployz.request.bytes_in", "Int(attributes[\"bytes_read\"])"),
        ("ployz.request.bytes_out", "Int(attributes[\"size\"])"),
        ("ployz.request.id", "attributes[\"rid\"]"),
        ("user_agent.original", "attributes[\"ua\"]"),
        ("ployz.request.referrer_host", "attributes[\"ref_host\"]"),
        ("ployz.request.language", "attributes[\"lang\"]"),
        ("ployz.request.content_type", "attributes[\"ctype\"]"),
        ("ployz.request.sec_purpose", "attributes[\"purpose\"]"),
        ("ployz.stream", "\"requests\""),
    ];
    for (name, value) in wire {
        out.push(format!("set(attributes[\"{name}\"], {value})"));
    }
    for key in QUERY_KEYS {
        out.push(format!(
            "set(attributes[\"ployz.request.{key}\"], attributes[\"q_{key}\"])"
        ));
    }
    out.push("set(body, \"\")".to_owned());
    let keep = KEPT
        .iter()
        .map(|name| (*name).to_owned())
        .chain((0..windows.len()).map(visitor_key));
    out.push(keep_keys(keep));
    out
}

fn clip_value(from: &str, max: usize) -> String {
    let pattern = format!("^(?s)(?P<v>.{{0,{max}}})");
    format!("ExtractPatterns({from}, {})[\"v\"]", quoted(&pattern))
}

fn keep_keys(names: impl Iterator<Item = String>) -> String {
    let names = names
        .map(|name| quoted(&name))
        .collect::<Vec<_>>()
        .join(", ");
    format!("keep_keys(attributes, [{names}])")
}

fn visitor_statements(windows: usize) -> Vec<String> {
    let mut out: Vec<String> = (0..windows)
        .map(|n| {
            let key = visitor_key(n);
            format!("set(attributes[\"ployz.request.visitor\"], attributes[\"{key}\"]) where attributes[\"{key}\"] != nil")
        })
        .collect();
    out.push(keep_keys(
        KEPT.iter()
            .map(|name| (*name).to_owned())
            .chain(std::iter::once("ployz.request.visitor".to_owned())),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn namespace(name: &str) -> Namespace {
        Namespace::parse(name).unwrap()
    }

    fn salt(byte: u8, start: i64, end: i64) -> Salt {
        Salt {
            day: "2026-10-15".to_owned(),
            start,
            end,
            salt: [byte; 32],
        }
    }

    fn parsed(fragment: &str) -> Value {
        serde_json::from_str(fragment).unwrap()
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn with_no_site_the_file_is_still_read_and_every_line_dropped() {
        let config = parsed(&fragment(&[]));
        assert!(config["receivers"]["filelog/requests"].is_object());
        assert_eq!(
            config["processors"]["filter/requests"]["logs"]["log_record"],
            json!(["true"])
        );
        assert_eq!(
            config["service"]["pipelines"]["logs/requests"]["processors"],
            json!(["filter/requests"])
        );
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn each_site_day_window_gets_its_own_redaction_keyed_by_its_salt() {
        let config = parsed(&fragment(&[
            (
                namespace("acme-prod"),
                vec![salt(1, 100, 200), salt(2, 200, 300)],
            ),
            (namespace("acme-stage"), vec![salt(3, 100, 200)]),
        ]));
        let processors = &config["service"]["pipelines"]["logs/requests"]["processors"];
        let names: Vec<&str> = processors
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "memory_limiter",
                "filter/requests",
                "transform/requests",
                "redaction/0",
                "redaction/1",
                "redaction/2",
                "transform/visitors",
                "batch",
            ]
        );
        for (n, byte) in [(0, 1u8), (1, 2), (2, 3)] {
            let redaction = &config["processors"][format!("redaction/{n}")];
            assert_eq!(redaction["hmac_key"], URL_SAFE_NO_PAD.encode([byte; 32]));
            assert_eq!(redaction["hash_function"], "hmac-sha256");
            assert_eq!(redaction["blocked_key_patterns"][0], format!("^v\\.{n}$"));
        }
    }

    #[test]
    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn what_leaves_the_machine_is_an_allowlist_without_the_address_or_headers() {
        let config = parsed(&fragment(&[(
            namespace("acme-prod"),
            vec![salt(1, 100, 200)],
        )]));
        let last = |processor: &str| {
            config["processors"][processor]["log_statements"][0]["statements"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        };
        let sent = last("transform/visitors");
        let list = sent
            .strip_prefix("keep_keys(attributes, ")
            .and_then(|rest| rest.strip_suffix(')'))
            .unwrap();
        let kept: Vec<String> = serde_json::from_str(list).unwrap();
        for private in ["client_ip", "request", "headers", "v.0", "ua", "ts"] {
            assert!(
                !kept.iter().any(|name| name == private),
                "{private} leaves the Machine"
            );
        }
        assert!(kept.iter().any(|name| name == "ployz.request.visitor"));
    }
}
