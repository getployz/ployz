use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::Path;
use std::process::Command;

use hickory_proto::op::{Message, MessageType, OpCode, Query};
use hickory_proto::rr::{Name, RData, RecordType};

use super::{DNS_WAIT_TIMEOUT, ServingRoleProbe};

pub(super) fn parse_role_probe(response: serde_json::Value) -> Result<ServingRoleProbe, String> {
    if response
        .pointer("/status")
        .and_then(serde_json::Value::as_str)
        != Some("success")
    {
        return Err(format!("role request failed: {response}"));
    }
    let event = response
        .pointer("/data/event")
        .and_then(serde_json::Value::as_str);
    if !matches!(event, Some("status" | "reloaded")) {
        return Err(format!("unexpected role response: {response}"));
    }
    let listen_addr = response
        .pointer("/data/listen_addr")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing listen_addr: {response}"))?
        .parse::<SocketAddr>()
        .map_err(|error| format!("parse role listen_addr: {error}; response={response}"))?;
    let loaded_gateway_revision = response
        .pointer("/data/serving/loaded_revisions/gateway")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing gateway revision: {response}"))?
        .to_string();
    let tls_listen_addr = response
        .pointer("/data/tls_listen_addr")
        .and_then(serde_json::Value::as_str)
        .map(|value| {
            value.parse::<SocketAddr>().map_err(|error| {
                format!("parse role tls_listen_addr: {error}; response={response}")
            })
        })
        .transpose()?;
    let loaded_generation = response
        .pointer("/data/serving/loaded_revisions/generation")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing generation: {response}"))?
        .to_string();
    let loaded_dns_revision = response
        .pointer("/data/serving/loaded_revisions/dns")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing dns revision: {response}"))?
        .to_string();
    let freshness = response
        .pointer("/data/serving/freshness")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("role response missing freshness: {response}"))?
        .to_string();
    Ok(ServingRoleProbe {
        listen_addr,
        tls_listen_addr,
        loaded_generation,
        loaded_gateway_revision,
        loaded_dns_revision,
        freshness,
    })
}

pub(super) fn http_get(addr: SocketAddr, host: &str) -> Result<String, String> {
    let mut stream =
        TcpStream::connect(addr).map_err(|error| format!("connect HTTP {addr}: {error}"))?;
    stream
        .write_all(format!("GET / HTTP/1.1\r\nhost: {host}\r\n\r\n").as_bytes())
        .map_err(|error| format!("write HTTP {addr}: {error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("read HTTP {addr}: {error}"))?;
    response
        .split_once("\r\n\r\n")
        .map(|(_headers, body)| body.to_string())
        .ok_or_else(|| format!("HTTP response missing body separator: {response}"))
}

pub(super) fn curl_https(addr: SocketAddr, host: &str, root_ca: &Path) -> Result<String, String> {
    let output = Command::new("curl")
        .arg("--silent")
        .arg("--show-error")
        .arg("--fail")
        .arg("--http1.1")
        .arg("--noproxy")
        .arg("*")
        .arg("--cacert")
        .arg(root_ca)
        .arg("--resolve")
        .arg(format!("{host}:{}:127.0.0.1", addr.port()))
        .arg("--header")
        .arg(format!("Host: {host}"))
        .arg(format!("https://{host}:{}/", addr.port()))
        .output()
        .map_err(|error| format!("run curl HTTPS check: {error}"))?;
    if output.status.success() {
        return String::from_utf8(output.stdout).map_err(|error| error.to_string());
    }
    Err(format!(
        "curl HTTPS check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    ))
}

pub(super) fn dns_a_lookup(addr: SocketAddr, host: &str) -> Result<String, String> {
    let name = Name::from_ascii(format!("{host}."))
        .map_err(|error| format!("parse DNS query name '{host}': {error}"))?;
    let mut request = Message::new(9, MessageType::Query, OpCode::Query);
    request.add_query(Query::query(name, RecordType::A));
    let request = request
        .to_vec()
        .map_err(|error| format!("encode DNS query: {error}"))?;
    let socket =
        UdpSocket::bind("127.0.0.1:0").map_err(|error| format!("bind DNS client: {error}"))?;
    socket
        .set_read_timeout(Some(DNS_WAIT_TIMEOUT))
        .map_err(|error| format!("set DNS timeout: {error}"))?;
    socket
        .send_to(&request, addr)
        .map_err(|error| format!("send DNS query to {addr}: {error}"))?;
    let mut packet = [0_u8; 1232];
    let (len, _) = socket
        .recv_from(&mut packet)
        .map_err(|error| format!("receive DNS response from {addr}: {error}"))?;
    let response = Message::from_vec(&packet[..len])
        .map_err(|error| format!("decode DNS response: {error}"))?;
    response
        .answers
        .iter()
        .find_map(|record| match &record.data {
            RData::A(address) => Some(address.to_string()),
            _ => None,
        })
        .ok_or_else(|| format!("DNS response contained no A answer: {response:?}"))
}
