//! Dumb HTTP-facing gateway boundary.

use crate::gateway::GatewayUpstream;
use crate::gateway_runtime::{GatewayRouteSelectionError, GatewayRouteTable};
use ployz_core::ops::{RouteHostname, RouteHostnameError, RoutePort, RouteTarget};

pub fn select_http_upstream(
    routes: &GatewayRouteTable,
    authority: &str,
    listener_port: RoutePort,
) -> Result<GatewayUpstream, GatewayHttpRouteError> {
    let target = route_target_from_authority(authority, listener_port)
        .map_err(GatewayHttpRouteError::InvalidTarget)?;
    routes
        .select_upstream(&target)
        .map_err(GatewayHttpRouteError::RouteSelection)
}

pub fn route_target_from_authority(
    authority: &str,
    listener_port: RoutePort,
) -> Result<RouteTarget, HttpRouteTargetError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(HttpRouteTargetError::EmptyHost);
    }
    if authority.contains('@') {
        return Err(HttpRouteTargetError::UserInfo);
    }

    let (hostname, port) = host_and_port(authority, listener_port)?;
    let hostname = RouteHostname::try_new(hostname)
        .map_err(|source| HttpRouteTargetError::InvalidHostname { source })?;

    Ok(RouteTarget::try_new(hostname, port))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayHttpRouteError {
    InvalidTarget(HttpRouteTargetError),
    RouteSelection(GatewayRouteSelectionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRouteTargetError {
    EmptyHost,
    UserInfo,
    MissingHost,
    MissingPort,
    InvalidHostname { source: RouteHostnameError },
    InvalidPort,
    UnsupportedIpv6,
}

fn host_and_port(
    authority: &str,
    listener_port: RoutePort,
) -> Result<(&str, RoutePort), HttpRouteTargetError> {
    let Some((hostname, port)) = authority.rsplit_once(':') else {
        return Ok((authority, listener_port));
    };
    if hostname.contains(':') {
        return Err(HttpRouteTargetError::UnsupportedIpv6);
    }
    if hostname.is_empty() {
        return Err(HttpRouteTargetError::MissingHost);
    }
    if port.is_empty() {
        return Err(HttpRouteTargetError::MissingPort);
    }
    let port = port
        .parse::<u16>()
        .ok()
        .and_then(|value| RoutePort::try_new(value).ok())
        .ok_or(HttpRouteTargetError::InvalidPort)?;

    Ok((hostname, port))
}
