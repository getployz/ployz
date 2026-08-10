//! One-hop forwarding to the advisory preferred controller.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use hyper::{Response, StatusCode};
use ployz_core::V2Route;
use ployz_core::corrosion::{Principal, is_preferred_controller};
use ployz_core::ids::{MachineName, PeerName};

use super::controller::ControllerStore;
use super::deploy_hosts::machine_socket_addr;
use super::server::{
    BoundedBodyError, HttpBody, corrosion_unavailable_response, json_response, read_bounded_body,
    refusal_response,
};
use super::store::AcceptedRoster;

const FORWARDED_PEER_HEADER: &str = "x-ployz-forwarded-peer";
const FORWARDED_JOIN_HEADER: &str = "x-ployz-forwarded-join";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);
const BODY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY_BYTES: usize = 1_048_576;

pub(super) struct AdmittedMutation {
    pub(super) principal: Principal,
    pub(super) request: hyper::Request<hyper::body::Incoming>,
}

pub(super) enum MutationRouting {
    Local(AdmittedMutation),
    Forwarded(Response<HttpBody>),
}

/// One-hop forwarding to the controller named by Corrosion.
pub(super) struct ControllerForwarder {
    local_machine_id: MachineName,
    api_port: u16,
    controller: Arc<ControllerStore>,
    client: reqwest::Client,
}

impl ControllerForwarder {
    pub(super) fn new(
        local_machine_id: MachineName,
        api_port: u16,
        controller: Arc<ControllerStore>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(FORWARD_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;
        Ok(Self {
            local_machine_id,
            api_port,
            controller,
            client,
        })
    }

    pub(super) async fn route(
        &self,
        route: &V2Route,
        roster: &AcceptedRoster,
        principal: Principal,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> MutationRouting {
        match principal {
            Principal::Machine { machine_id } => {
                self.accept_forwarded(route, roster, machine_id, request)
                    .await
            }
            Principal::Peer { peer_id } => self.route_peer(route, roster, peer_id, request).await,
            Principal::ApiToken { .. } => MutationRouting::Forwarded(refusal_response(
                ployz_core::ApiRefusal::UnsupportedRoute,
            )),
        }
    }

    async fn accept_forwarded(
        &self,
        route: &V2Route,
        roster: &AcceptedRoster,
        source_machine: MachineName,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> MutationRouting {
        if !accepts_machine(roster, &self.local_machine_id) {
            return unavailable();
        }
        let forwarded_join = matches!(route, V2Route::Join)
            && request
                .headers()
                .get(FORWARDED_JOIN_HEADER)
                .is_some_and(|value| value == "1");
        let peer_id = if forwarded_join {
            None
        } else {
            let Some(peer) = forwarded_peer(&request) else {
                return unsupported();
            };
            if !roster.peers.iter().any(|accepted| accepted.name == peer) {
                return unsupported();
            }
            Some(peer)
        };
        let current = match self.controller.current().await {
            Ok(Some(current)) => current,
            Ok(None) => return unavailable(),
            Err(error) => {
                tracing::warn!(%error, "could not read controller appointment");
                return unavailable();
            }
        };
        if !is_preferred_controller(&current, &self.local_machine_id) {
            return stale_controller();
        }
        MutationRouting::Local(AdmittedMutation {
            principal: peer_id.map_or(
                Principal::Machine {
                    machine_id: source_machine,
                },
                |peer_id| Principal::Peer { peer_id },
            ),
            request,
        })
    }

    /// Routes a token-bearing public join request to the appointed controller.
    pub(super) async fn route_join(
        &self,
        roster: &AcceptedRoster,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> MutationRouting {
        if !accepts_machine(roster, &self.local_machine_id) {
            return unavailable();
        }
        let current = match self.current().await {
            Ok(current) => current,
            Err(response) => return MutationRouting::Forwarded(response),
        };
        if current.preferred_machine_name == self.local_machine_id {
            return MutationRouting::Local(AdmittedMutation {
                principal: Principal::Machine {
                    machine_id: self.local_machine_id.clone(),
                },
                request,
            });
        }
        let target = roster
            .machines
            .iter()
            .find(|machine| machine.document.name == current.preferred_machine_name)
            .map(|machine| machine_socket_addr(&machine.document.transport, self.api_port));
        let response = match target {
            Some(target) => self.forward(target, &V2Route::Join, None, request).await,
            None => Err(ForwardError::Connect(
                "appointed machine is absent from the accepted roster".to_owned(),
            )),
        };
        match response {
            Ok(response) => MutationRouting::Forwarded(response),
            Err(ForwardError::Connect(detail)) => {
                tracing::warn!(%detail, machine_name = %current.preferred_machine_name, "preferred controller is unreachable during join");
                unavailable()
            }
            Err(ForwardError::Other(detail)) => {
                tracing::warn!(%detail, "controller join forwarding failed");
                unavailable()
            }
            Err(ForwardError::Body(error)) => {
                MutationRouting::Forwarded(body_error_response(error))
            }
        }
    }

    async fn route_peer(
        &self,
        route: &V2Route,
        roster: &AcceptedRoster,
        peer_id: PeerName,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> MutationRouting {
        if !accepts_machine(roster, &self.local_machine_id) {
            return unavailable();
        }
        let current = match self.current().await {
            Ok(current) => current,
            Err(response) => return MutationRouting::Forwarded(response),
        };
        if current.preferred_machine_name == self.local_machine_id {
            return MutationRouting::Local(AdmittedMutation {
                principal: Principal::Peer { peer_id },
                request,
            });
        }

        let target = roster
            .machines
            .iter()
            .find(|machine| machine.document.name == current.preferred_machine_name)
            .map(|machine| machine_socket_addr(&machine.document.transport, self.api_port));
        let response = match target {
            Some(target) => self.forward(target, route, Some(&peer_id), request).await,
            None => Err(ForwardError::Connect(
                "appointed machine is absent from the accepted roster".to_owned(),
            )),
        };
        match response {
            Ok(response) => MutationRouting::Forwarded(response),
            Err(ForwardError::Connect(detail)) => {
                tracing::warn!(%detail, machine_name = %current.preferred_machine_name, "preferred controller is unreachable");
                unavailable()
            }
            Err(ForwardError::Other(detail)) => {
                tracing::warn!(%detail, "controller forwarding failed without changing appointment");
                unavailable()
            }
            Err(ForwardError::Body(error)) => MutationRouting::Forwarded(match error {
                BoundedBodyError::TooLarge => super::mutations::simple_error(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "request_too_large",
                ),
                BoundedBodyError::Deadline => {
                    super::mutations::simple_error(StatusCode::REQUEST_TIMEOUT, "request_timeout")
                }
                BoundedBodyError::Read => {
                    super::mutations::simple_error(StatusCode::BAD_REQUEST, "invalid_request")
                }
            }),
        }
    }

    async fn current(
        &self,
    ) -> Result<ployz_core::corrosion::ControllerDocument, Response<HttpBody>> {
        match self.controller.current().await {
            Ok(Some(current)) => Ok(current),
            Ok(None) => Err(corrosion_unavailable_response()),
            Err(error) => {
                tracing::warn!(%error, "could not read controller appointment");
                Err(corrosion_unavailable_response())
            }
        }
    }

    async fn forward(
        &self,
        target: std::net::SocketAddr,
        route: &V2Route,
        peer_id: Option<&PeerName>,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> Result<Response<HttpBody>, ForwardError> {
        let body = read_bounded_body(request.into_body(), MAX_BODY_BYTES, BODY_TIMEOUT)
            .await
            .map_err(ForwardError::Body)?;
        let request = self
            .client
            .post(format!("http://{target}{}", route.path()))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        let request = match peer_id {
            Some(peer_id) => request.header(FORWARDED_PEER_HEADER, peer_id.as_str()),
            None => request.header(FORWARDED_JOIN_HEADER, "1"),
        };
        let response = request.send().await.map_err(|error| {
            if error.is_connect() && !error.is_timeout() {
                ForwardError::Connect(error.to_string())
            } else {
                ForwardError::Other(error.to_string())
            }
        })?;
        let status = StatusCode::from_u16(response.status().as_u16())
            .map_err(|error| ForwardError::Other(error.to_string()))?;
        Ok(json_response(status, response_body(response).await?))
    }
}

fn accepts_machine(roster: &AcceptedRoster, machine_id: &MachineName) -> bool {
    roster
        .machines
        .iter()
        .any(|machine| &machine.document.name == machine_id)
}

enum ForwardError {
    Connect(String),
    Other(String),
    Body(BoundedBodyError),
}

impl From<BoundedBodyError> for ForwardError {
    fn from(error: BoundedBodyError) -> Self {
        Self::Body(error)
    }
}

async fn response_body(response: reqwest::Response) -> Result<Vec<u8>, ForwardError> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ForwardError::Other(error.to_string()))?;
        let Some(total) = body.len().checked_add(chunk.len()) else {
            return Err(ForwardError::Other(
                "controller reply was too large".to_owned(),
            ));
        };
        if total > MAX_BODY_BYTES {
            return Err(ForwardError::Other(
                "controller reply was too large".to_owned(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn forwarded_peer<Body>(request: &hyper::Request<Body>) -> Option<PeerName> {
    request
        .headers()
        .get(FORWARDED_PEER_HEADER)?
        .to_str()
        .ok()
        .and_then(|value| PeerName::try_new(value.to_owned()).ok())
}

fn body_error_response(error: BoundedBodyError) -> Response<HttpBody> {
    match error {
        BoundedBodyError::TooLarge => {
            super::mutations::simple_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large")
        }
        BoundedBodyError::Deadline => {
            super::mutations::simple_error(StatusCode::REQUEST_TIMEOUT, "request_timeout")
        }
        BoundedBodyError::Read => {
            super::mutations::simple_error(StatusCode::BAD_REQUEST, "invalid_request")
        }
    }
}

fn unavailable() -> MutationRouting {
    MutationRouting::Forwarded(corrosion_unavailable_response())
}

fn unsupported() -> MutationRouting {
    MutationRouting::Forwarded(refusal_response(ployz_core::ApiRefusal::UnsupportedRoute))
}

fn stale_controller() -> MutationRouting {
    MutationRouting::Forwarded(json_response(
        StatusCode::CONFLICT,
        b"{\"kind\":\"stale_controller\"}".to_vec(),
    ))
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt as _;

    use super::*;

    #[test]
    fn forwarded_peer_requires_a_typed_name() {
        let peer_id = PeerName::try_new("peer-a").expect("peer name");
        let valid = hyper::Request::builder()
            .header(FORWARDED_PEER_HEADER, peer_id.as_str())
            .body(())
            .expect("request");

        assert_eq!(forwarded_peer(&valid), Some(peer_id));

        for request in [
            hyper::Request::builder().body(()).expect("request"),
            hyper::Request::builder()
                .header(FORWARDED_PEER_HEADER, "not/a/peer")
                .body(())
                .expect("request"),
        ] {
            assert_eq!(forwarded_peer(&request), None);
        }
    }

    #[tokio::test]
    async fn stale_appointment_returns_a_conflict() {
        let MutationRouting::Forwarded(response) = stale_controller() else {
            panic!("stale appointment must be an HTTP response");
        };

        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .expect("infallible response body")
                .to_bytes(),
            &b"{\"kind\":\"stale_controller\"}"[..]
        );
    }
}
