use std::{net::IpAddr, num::NonZeroU16};

use ployz_core::{HostBind, HttpProtocol, IngressHostname, PortPublication, TransportProtocol};

const UNSUPPORTED_TRANSPORT_INGRESS: &str = "TCP/UDP ingress is not supported; use host publication (for example 8080:80/tcp@host or mode: host)";

use super::{
    convert::{invalid, scalar, string_list},
    model::{ComposeError, RawPort, RawService},
};

pub(super) fn ports(name: &str, raw: &RawService) -> Result<Vec<PortPublication>, ComposeError> {
    let extension = raw.extension_ports.as_ref();
    if !raw.ports.is_empty() && extension.is_some() {
        return Err(invalid(format!(
            "service '{name}' cannot specify both 'ports' and 'x-ports'"
        )));
    }
    if let Some(extension) = extension {
        return string_list(extension)?
            .into_iter()
            .map(|port| parse_extension_port(&port))
            .collect();
    }
    raw.ports.iter().map(convert_standard_port).collect()
}

fn convert_standard_port(port: &RawPort) -> Result<PortPublication, ComposeError> {
    match port {
        RawPort::Short(port) => parse_standard_short_port(port),
        RawPort::Long {
            target,
            published,
            host_ip,
            protocol,
            mode,
        } => {
            let published = published
                .as_ref()
                .map(|value| {
                    scalar(value).ok_or_else(|| invalid("published port must be a scalar"))
                })
                .transpose()?;
            make_standard_port(
                *target,
                published.as_deref(),
                host_ip.as_deref(),
                protocol.as_deref().unwrap_or("tcp"),
                mode.as_deref().unwrap_or("ingress"),
            )
        }
    }
}

fn parse_standard_short_port(value: &str) -> Result<PortPublication, ComposeError> {
    let (value, protocol) = value
        .rsplit_once('/')
        .filter(|(_, protocol)| matches!(*protocol, "tcp" | "udp"))
        .unwrap_or((value, "tcp"));
    let parts = split_port_parts(value);
    match parts.as_slice() {
        [target] => make_standard_port(parse_port(target)?, None, None, protocol, "ingress"),
        [published, target] => make_standard_port(
            parse_port(target)?,
            Some(published),
            None,
            protocol,
            "ingress",
        ),
        [host, published, target] => make_standard_port(
            parse_port(target)?,
            Some(published),
            Some(host),
            protocol,
            "ingress",
        ),
        _ => Err(invalid(format!("invalid port '{value}'"))),
    }
}

fn make_standard_port(
    target: u16,
    published: Option<&str>,
    host: Option<&str>,
    protocol: &str,
    mode: &str,
) -> Result<PortPublication, ComposeError> {
    let target = nonzero_port(target, "container")?;
    if published.is_some_and(|port| port.contains('-')) {
        return Err(invalid("published port ranges are not supported"));
    }
    let published = published
        .map(parse_port)
        .transpose()?
        .map(|port| nonzero_port(port, "published"))
        .transpose()?;
    let protocol = transport_protocol(protocol)?;
    if mode == "host" {
        return Ok(PortPublication::Host {
            bind: host_bind(host)?,
            published_port: published
                .ok_or_else(|| invalid("published port is required in host mode"))?,
            container_port: target,
            transport_protocol: protocol,
        });
    }
    if mode != "ingress" {
        return Err(invalid(format!("invalid port mode '{mode}'")));
    }
    Err(invalid(UNSUPPORTED_TRANSPORT_INGRESS))
}

pub(crate) fn parse_extension_port(value: &str) -> Result<PortPublication, ComposeError> {
    if value.matches('@').count() > 1 {
        return Err(invalid("too many '@' symbols in port"));
    }
    let host_mode = value.ends_with("@host");
    if value.contains('@') && !host_mode {
        return Err(invalid(format!("invalid port mode in '{value}'")));
    }
    let value = value.strip_suffix("@host").unwrap_or(value);
    let parts = split_port_parts(value);
    let last = parts
        .last()
        .ok_or_else(|| invalid("port cannot be empty"))?;
    let (target, protocol) = last.rsplit_once('/').unwrap_or((last, "tcp"));
    let target = nonzero_port(parse_port(target)?, "container")?;
    if host_mode {
        if !matches!(protocol, "tcp" | "udp") {
            return Err(invalid(format!(
                "unsupported protocol '{protocol}' in host mode"
            )));
        }
        let (host, published) = match parts.as_slice() {
            [published, _] => (None, published.as_str()),
            [host, published, _] => (Some(host.as_str()), published.as_str()),
            _ => return Err(invalid(format!("invalid host port '{value}'"))),
        };
        return Ok(PortPublication::Host {
            bind: host_bind(host)?,
            published_port: nonzero_port(parse_port(published)?, "published")?,
            container_port: target,
            transport_protocol: transport_protocol(protocol)?,
        });
    }
    let (hostname, published) = match parts.as_slice() {
        [_] => ("", None),
        [first, _] => match first.parse::<u16>() {
            Ok(port) => ("", Some(nonzero_port(port, "load balancer")?)),
            Err(_) => (first.as_str(), None),
        },
        [hostname, published, _] => (
            hostname.as_str(),
            Some(nonzero_port(parse_port(published)?, "load balancer")?),
        ),
        _ => return Err(invalid(format!("invalid ingress port '{value}'"))),
    };
    match protocol {
        "http" | "https" => Ok(PortPublication::Ingress {
            hostname: ingress_hostname(hostname)?,
            load_balancer_port: published.unwrap_or_else(|| {
                NonZeroU16::new(if protocol == "https" { 443 } else { 80 }).expect("non-zero")
            }),
            container_port: target,
            http_protocol: if protocol == "https" {
                HttpProtocol::Https
            } else {
                HttpProtocol::Http
            },
        }),
        "tcp" | "udp" => Err(invalid(UNSUPPORTED_TRANSPORT_INGRESS)),
        protocol => Err(invalid(format!("unsupported protocol '{protocol}'"))),
    }
}

fn ingress_hostname(hostname: &str) -> Result<IngressHostname, ComposeError> {
    if hostname.is_empty() {
        Ok(IngressHostname::AssignFromClusterDomain)
    } else {
        IngressHostname::explicit(hostname).map_err(invalid)
    }
}

fn host_bind(host: Option<&str>) -> Result<HostBind, ComposeError> {
    match host {
        None | Some("") | Some("0.0.0.0") | Some("::") => Ok(HostBind::All),
        Some(host) if host.contains('/') => {
            let prefix = host
                .strip_prefix('[')
                .and_then(|host| host.split_once("]/"))
                .map_or_else(
                    || host.to_owned(),
                    |(address, bits)| format!("{address}/{bits}"),
                );
            Ok(HostBind::Prefix {
                prefix: prefix.parse().map_err(invalid)?,
            })
        }
        Some(host) => Ok(HostBind::Address {
            address: host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .map_err(invalid)?,
        }),
    }
}

fn split_port_parts(value: &str) -> Vec<String> {
    let mut parts = value.split(':').map(str::to_owned).collect::<Vec<_>>();
    if parts.len() > 3 {
        let trailing = parts.split_off(parts.len() - 2);
        parts = vec![parts.join(":")];
        parts.extend(trailing);
    }
    parts
}

fn parse_port(value: &str) -> Result<u16, ComposeError> {
    if value.contains('-') {
        return Err(invalid("published port ranges are not supported"));
    }
    value
        .parse()
        .map_err(|_| invalid(format!("invalid port '{value}'")))
}

fn nonzero_port(port: u16, kind: &str) -> Result<NonZeroU16, ComposeError> {
    NonZeroU16::new(port).ok_or_else(|| invalid(format!("{kind} port must be non-zero")))
}

fn transport_protocol(protocol: &str) -> Result<TransportProtocol, ComposeError> {
    match protocol {
        "tcp" => Ok(TransportProtocol::Tcp),
        "udp" => Ok(TransportProtocol::Udp),
        protocol => Err(invalid(format!("unsupported protocol '{protocol}'"))),
    }
}
