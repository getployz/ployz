use std::path::Path;

use super::{
    PeerEndpoint, PeerProbeDeadline, PeerProbeResult, PeerRpcListener, PeerTicket,
    bind_peer_endpoint, issue_endpoint_ticket, load_or_create_identity,
};

pub struct PeerRuntime {
    endpoint: PeerEndpoint,
    listener: PeerRpcListener,
    ticket: PeerTicket,
}

impl PeerRuntime {
    pub async fn start(
        identity_path: &Path,
        boot_deadline: PeerProbeDeadline,
    ) -> PeerProbeResult<Self> {
        let identity = load_or_create_identity(identity_path)?;
        let endpoint = bind_peer_endpoint(&identity).await?;
        let ticket = match issue_endpoint_ticket(&endpoint, boot_deadline).await {
            Ok(ticket) => ticket,
            Err(error) => {
                endpoint.close().await;
                return Err(error);
            }
        };
        let listener = match PeerRpcListener::start(endpoint.clone()) {
            Ok(listener) => listener,
            Err(error) => {
                endpoint.close().await;
                return Err(error);
            }
        };

        Ok(Self {
            endpoint,
            listener,
            ticket,
        })
    }

    #[must_use]
    pub fn endpoint(&self) -> PeerEndpoint {
        self.endpoint.clone()
    }

    #[must_use]
    pub fn endpoint_id(&self) -> crate::IrohEndpointId {
        self.ticket.endpoint_id()
    }

    #[must_use]
    pub fn ticket(&self) -> &PeerTicket {
        &self.ticket
    }

    pub async fn shutdown(self, shutdown_deadline: PeerProbeDeadline) -> PeerProbeResult<()> {
        self.listener.shutdown(shutdown_deadline).await
    }
}
