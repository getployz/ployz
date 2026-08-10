//! Local-or-mesh dispatch for the three coarse deploy host effects.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use hyper::StatusCode;
use ployz_core::corrosion::MachineTransport;
use ployz_core::ids::{ClusterName, MachineName};
use ployz_core::{
    DeployInspectOutcome, DeployPrepareOutcome, DeployPrepareRequest, DeployRetireOutcome,
    DeployRetireRequest, V2Route,
};

use super::controller::ControllerStore;
use super::deploy_effects::DeployHostEffects;
use super::node_workflows::{NodeWorkflows, ROLLBACK_WAIT};
use super::simple_deploy::{DeployHostError, DeployHosts};
use super::store::read_accepted_roster;
use crate::corrosion::CorrosionClient;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const INSPECT_TIMEOUT: Duration = Duration::from_secs(20);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(250);
const RETIRE_TIMEOUT: Duration = Duration::from_secs(75);
const ROLLBACK_TIMEOUT: Duration = ROLLBACK_WAIT.saturating_add(Duration::from_secs(5));
const MAX_REPLY_BYTES: usize = 1_048_576;

pub(super) struct MeshDeployHosts {
    local_machine_id: MachineName,
    cluster_id: ClusterName,
    api_port: u16,
    corrosion: CorrosionClient,
    controller: Arc<ControllerStore>,
    local_effects: Arc<DeployHostEffects>,
    local_workflows: Arc<NodeWorkflows>,
    client: reqwest::Client,
}

impl MeshDeployHosts {
    pub(super) fn new(
        local_machine_id: MachineName,
        cluster_id: ClusterName,
        api_port: u16,
        corrosion: CorrosionClient,
        controller: Arc<ControllerStore>,
        local_effects: Arc<DeployHostEffects>,
        local_workflows: Arc<NodeWorkflows>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()?;
        Ok(Self {
            local_machine_id,
            cluster_id,
            api_port,
            corrosion,
            controller,
            local_effects,
            local_workflows,
            client,
        })
    }

    async fn require_local_controller(&self) -> Result<(), DeployHostError> {
        match self.controller.local_machine_is_preferred().await {
            Ok(true) => Ok(()),
            Ok(false) => Err(DeployHostError::StaleController),
            Err(_) => Err(DeployHostError::Failed),
        }
    }

    async fn target(&self, machine_id: &MachineName) -> Result<SocketAddr, DeployHostError> {
        let roster = read_accepted_roster(&self.corrosion, &self.cluster_id)
            .await
            .map_err(|_| DeployHostError::Failed)?;
        let machine = roster
            .machines
            .into_iter()
            .find(|machine| &machine.document.name == machine_id)
            .ok_or(DeployHostError::Failed)?;
        Ok(machine_socket_addr(
            &machine.document.transport,
            self.api_port,
        ))
    }

    async fn post<Request, Reply>(
        &self,
        machine_id: &MachineName,
        route: V2Route,
        request: &Request,
        timeout: Duration,
    ) -> Result<Reply, DeployHostError>
    where
        Request: serde::Serialize + ?Sized,
        Reply: serde::de::DeserializeOwned,
    {
        let target = self.target(machine_id).await?;
        let response = self
            .client
            .post(format!("http://{target}{}", route.path()))
            .timeout(timeout)
            .json(request)
            .send()
            .await
            .map_err(|_| DeployHostError::Failed)?;
        if response.status() == StatusCode::CONFLICT.as_u16() {
            return Err(DeployHostError::StaleController);
        }
        if response.status() != StatusCode::OK.as_u16() {
            return Err(DeployHostError::Failed);
        }
        decode_bounded(response).await
    }
}

#[async_trait]
impl DeployHosts for MeshDeployHosts {
    async fn inspect(
        &self,
        machine_id: &MachineName,
    ) -> Result<DeployInspectOutcome, DeployHostError> {
        if machine_id == &self.local_machine_id {
            self.require_local_controller().await?;
            return Ok(self.local_effects.inspect().await);
        }
        self.post(machine_id, V2Route::DeployInspect, &(), INSPECT_TIMEOUT)
            .await
    }

    async fn prepare(
        &self,
        machine_id: &MachineName,
        request: DeployPrepareRequest,
    ) -> Result<DeployPrepareOutcome, DeployHostError> {
        if machine_id == &self.local_machine_id {
            if request.controller_machine_name != self.local_machine_id {
                return Err(DeployHostError::Failed);
            }
            self.require_local_controller().await?;
            return Ok(self.local_workflows.prepare(request).await);
        }
        self.post(
            machine_id,
            V2Route::DeployPrepare,
            &request,
            PREPARE_TIMEOUT,
        )
        .await
    }

    async fn retire(
        &self,
        machine_id: &MachineName,
        request: DeployRetireRequest,
    ) -> Result<DeployRetireOutcome, DeployHostError> {
        if machine_id == &self.local_machine_id {
            if request.controller_machine_name != self.local_machine_id {
                return Err(DeployHostError::Failed);
            }
            if request.rollback_services.is_empty() {
                self.require_local_controller().await?;
            }
            return Ok(self.local_workflows.retire(request).await);
        }
        let timeout = if request.rollback_services.is_empty() {
            RETIRE_TIMEOUT
        } else {
            ROLLBACK_TIMEOUT
        };
        self.post(machine_id, V2Route::DeployRetire, &request, timeout)
            .await
    }
}

async fn decode_bounded<Reply>(response: reqwest::Response) -> Result<Reply, DeployHostError>
where
    Reply: serde::de::DeserializeOwned,
{
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| DeployHostError::Failed)?;
        let total = body
            .len()
            .checked_add(chunk.len())
            .ok_or(DeployHostError::Failed)?;
        if total > MAX_REPLY_BYTES {
            return Err(DeployHostError::Failed);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| DeployHostError::Failed)
}

pub(super) fn machine_socket_addr(transport: &MachineTransport, api_port: u16) -> SocketAddr {
    let ip = match transport {
        MachineTransport::Wireguard { addr_v6, .. } => IpAddr::V6(*addr_v6),
        MachineTransport::Tailscale { ip, .. } => IpAddr::V4(*ip),
    };
    SocketAddr::new(ip, api_port)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};

    use super::*;

    #[test]
    fn machine_address_uses_the_shared_api_port_for_both_meshes() {
        let wireguard = MachineTransport::Wireguard {
            addr_v6: "fd00::12".parse::<Ipv6Addr>().expect("IPv6"),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.12.0/24").expect("subnet"),
            pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("key"),
            endpoint: None,
        };
        let tailscale = MachineTransport::Tailscale {
            ip: Ipv4Addr::new(100, 64, 0, 12),
            subnet_v4: MachineEndpointSubnet::try_new("10.210.12.0/24").expect("subnet"),
        };
        assert_eq!(machine_socket_addr(&wireguard, 8080).port(), 8080);
        assert_eq!(machine_socket_addr(&tailscale, 8080).ip(), tailscale_ip());
    }

    fn tailscale_ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(100, 64, 0, 12))
    }
}
