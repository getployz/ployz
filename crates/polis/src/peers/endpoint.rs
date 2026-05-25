use super::{
    PeerError, PeerIdentity, PeerProbeDeadline, PeerProbeResult, PeerTicket, issue_ticket,
};

pub type PeerEndpoint = iroh::Endpoint;

pub async fn bind_peer_endpoint(identity: &PeerIdentity) -> PeerProbeResult<PeerEndpoint> {
    iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(identity.secret_key())
        .bind()
        .await
        .map_err(|error| PeerError::EndpointBind {
            message: error.to_string(),
        })
}

pub async fn issue_endpoint_ticket(
    endpoint: &PeerEndpoint,
    deadline: PeerProbeDeadline,
) -> PeerProbeResult<PeerTicket> {
    tokio::time::timeout(deadline.duration(), endpoint.online())
        .await
        .map_err(|_| PeerError::EndpointOnlineTimeout)?;
    Ok(issue_ticket(endpoint.addr()))
}
