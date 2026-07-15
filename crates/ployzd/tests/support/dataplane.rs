use futures_util::StreamExt;
use ployz_core::ids::MachineId;
use ployz_core::network::{
    DataplaneProjectionRevisions, DataplaneProjectionTestimony, EbpfAttachmentStatus,
    EndpointBridgeStatus, MachineDataplaneStatus, NativeDataplaneProjectionStatus,
    WireGuardConfiguredMtu, WireGuardDetectedMtu, WireGuardHandshakeStatus, WireGuardInterfaceMtu,
    WireGuardMtuProbe, WireGuardPeerEndpointSubnet, WireGuardPeerStatus, WireGuardRttStatus,
    WireGuardStatus,
};
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployzd::intent::service::NatsIntentReader;
use ployzd::roles::machine::protocol::{
    MachineDataplaneStatusRpcOk, MachineDataplaneStatusRpcResponse,
};

pub async fn start_applied_status_responder(
    service_client: async_nats::Client,
    intent_client: async_nats::Client,
    machine_id: MachineId,
) -> tokio::task::JoinHandle<()> {
    let mut requests = service_client
        .subscribe(machine_service(
            &machine_id,
            MachineServiceEndpoint::DataplaneStatus,
        ))
        .await
        .expect("subscribe dataplane status service");
    service_client
        .flush()
        .await
        .expect("flush dataplane status service subscription");
    tokio::spawn(async move {
        let reader = NatsIntentReader::new(intent_client);
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else { continue };
            let projection = reader
                .intent()
                .await
                .expect("intent reads for status testimony")
                .dataplane_projection;
            let local = projection
                .target_members()
                .iter()
                .find(|member| member.machine_id == machine_id)
                .expect("machine is projected");
            let peers = projection
                .target_members()
                .iter()
                .filter(|peer| peer.machine_id != machine_id)
                .map(|peer| WireGuardPeerStatus {
                    public_key: peer.wireguard_public_key.clone(),
                    endpoint_subnet: WireGuardPeerEndpointSubnet::Valid {
                        subnet: peer.endpoint_subnet.clone(),
                    },
                    endpoint: peer.mesh_endpoints.first().copied(),
                    handshake: WireGuardHandshakeStatus::Ago { seconds: 1 },
                    rtt: WireGuardRttStatus::Unavailable {
                        message: "not probed".to_owned(),
                    },
                    rx_bytes: 0,
                    tx_bytes: 0,
                    mtu_probe: WireGuardMtuProbe::NotRequested,
                })
                .collect();
            let response = MachineDataplaneStatusRpcResponse::Ok(MachineDataplaneStatusRpcOk {
                machine_id: machine_id.clone(),
                value: MachineDataplaneStatus {
                    projection: NativeDataplaneProjectionStatus {
                        endpoint_bridge: EndpointBridgeStatus::Ready {
                            subnet: local.endpoint_subnet.clone(),
                        },
                        testimony: DataplaneProjectionTestimony::Applied {
                            revisions: DataplaneProjectionRevisions {
                                declared_revision: projection.declared_revision().clone(),
                                target_revision: projection.target_revision().clone(),
                            },
                        },
                    },
                    wireguard: WireGuardStatus {
                        interface: "ployz-wg0".to_owned(),
                        configured_mtu: WireGuardConfiguredMtu::Auto,
                        detected_mtu: WireGuardDetectedMtu::Detected { mtu: 1420 },
                        interface_mtu: WireGuardInterfaceMtu::Detected { mtu: 1420 },
                        peers,
                    },
                    ebpf_attachment: EbpfAttachmentStatus::Attached,
                },
            });
            let payload = serde_json::to_vec(&response).expect("status response serializes");
            let _ = service_client.publish(reply, payload.into()).await;
        }
    })
}
