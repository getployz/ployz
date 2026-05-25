use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use crate::IrohEndpointId;

use super::{PLOYZ_PEER_ALPN, PeerError, PeerProbeResult, PeerTicketPath};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerProbeDeadline(Duration);

impl PeerProbeDeadline {
    #[must_use]
    pub fn new(duration: Duration) -> Self {
        Self(duration)
    }

    #[must_use]
    pub fn duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerProbeReceipt {
    endpoint: IrohEndpointId,
    alpn: &'static [u8],
    observed_path: PeerTicketPath,
}

impl PeerProbeReceipt {
    #[must_use]
    pub fn new(endpoint: IrohEndpointId, observed_path: PeerTicketPath) -> Self {
        Self {
            endpoint,
            alpn: PLOYZ_PEER_ALPN,
            observed_path,
        }
    }

    #[must_use]
    pub fn endpoint(&self) -> &IrohEndpointId {
        &self.endpoint
    }

    #[must_use]
    pub fn observed_path(&self) -> PeerTicketPath {
        self.observed_path
    }

    #[must_use]
    pub fn alpn(&self) -> &'static [u8] {
        self.alpn
    }
}

pub trait PeerProbe {
    fn probe<'a>(
        &'a self,
        target: &'a IrohEndpointId,
        deadline: PeerProbeDeadline,
    ) -> impl Future<Output = PeerProbeResult<PeerProbeReceipt>> + 'a;
}

#[derive(Debug, Default)]
pub struct FakePeerProbe {
    reachable: BTreeSet<IrohEndpointId>,
    failures: BTreeMap<IrohEndpointId, String>,
}

impl FakePeerProbe {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn reachable(mut self, endpoint: IrohEndpointId) -> Self {
        self.reachable.insert(endpoint);
        self
    }

    #[must_use]
    pub fn failing(mut self, endpoint: IrohEndpointId, reason: impl Into<String>) -> Self {
        self.failures.insert(endpoint, reason.into());
        self
    }
}

impl PeerProbe for FakePeerProbe {
    async fn probe(
        &self,
        target: &IrohEndpointId,
        _deadline: PeerProbeDeadline,
    ) -> PeerProbeResult<PeerProbeReceipt> {
        if let Some(reason) = self.failures.get(target) {
            return Err(PeerError::ProbeFailed {
                endpoint: target.clone(),
                reason: reason.clone(),
            });
        }

        if self.reachable.contains(target) {
            return Ok(PeerProbeReceipt::new(
                target.clone(),
                PeerTicketPath::DiscoveryOnly,
            ));
        }

        Err(PeerError::ProbeFailed {
            endpoint: target.clone(),
            reason: "endpoint is not reachable".to_string(),
        })
    }
}
