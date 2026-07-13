use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::dataplane::{
    DataplaneProjection, DataplaneProjectionComponent, DataplaneProjectionFailure,
    DataplaneProjectionRevisions, DataplaneProjectionTestimony, EndpointBridgeStatus,
    MachineEndpointSubnet, NativeDataplaneProjectionStatus, PloyzNativeMeshComponent,
    WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError, WireGuardPeer,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use ployz_core::subjects::INTENT_CHANGED;
use ployz_nats::service_runtime::NatsClient;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::intent::service::NatsIntentReader;
use crate::roles::machine::runner::{MachineContainerRunner, MachineContainerRunnerError};
use crate::roles::machine::service::MachinePloyzNativeMeshPreparer;

const PROJECTION_READ_TIMEOUT: Duration = Duration::from_secs(30);
const PROJECTION_RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub(crate) struct MachineProjectionState(Arc<Mutex<NativeDataplaneProjectionStatus>>);

impl MachineProjectionState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(
            NativeDataplaneProjectionStatus::unavailable(failure_message(
                "dataplane projection has not been fetched",
            )),
        )))
    }

    #[must_use]
    pub(crate) fn snapshot(&self) -> NativeDataplaneProjectionStatus {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn record_fetch_failure(&self, message: String) {
        let mut status = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let last_applied_revisions = last_applied_revisions(&status.testimony);
        status.testimony = DataplaneProjectionTestimony::Unusable {
            attempted_revisions: None,
            last_applied_revisions,
            failure: DataplaneProjectionFailure::FetchFailed {
                message: failure_message(message),
            },
        };
    }

    fn record_failure(
        &self,
        attempted_revisions: DataplaneProjectionRevisions,
        endpoint_bridge: EndpointBridgeStatus,
        failure: DataplaneProjectionFailure,
    ) {
        let mut status = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let last_applied_revisions = last_applied_revisions(&status.testimony);
        *status = NativeDataplaneProjectionStatus {
            endpoint_bridge,
            testimony: DataplaneProjectionTestimony::Unusable {
                attempted_revisions: Some(attempted_revisions),
                last_applied_revisions,
                failure,
            },
        };
    }

    fn record_applied(
        &self,
        revisions: DataplaneProjectionRevisions,
        subnet: MachineEndpointSubnet,
    ) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = NativeDataplaneProjectionStatus {
            endpoint_bridge: EndpointBridgeStatus::Ready { subnet },
            testimony: DataplaneProjectionTestimony::Applied { revisions },
        };
    }
}

fn last_applied_revisions(
    testimony: &DataplaneProjectionTestimony,
) -> Option<DataplaneProjectionRevisions> {
    match testimony {
        DataplaneProjectionTestimony::Applied { revisions } => Some(revisions.clone()),
        DataplaneProjectionTestimony::Unusable {
            last_applied_revisions,
            ..
        } => last_applied_revisions.clone(),
    }
}

pub(crate) struct RunningProjectionTask {
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl RunningProjectionTask {
    pub(crate) async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

pub(crate) fn start_projection_task<R, P>(
    client: NatsClient,
    machine_id: MachineId,
    runner: R,
    preparer: P,
    state: MachineProjectionState,
) -> RunningProjectionTask
where
    R: MachineContainerRunner + Send + Sync + 'static,
    P: MachinePloyzNativeMeshPreparer + Send + Sync + 'static,
{
    let (shutdown, mut shutdown_rx) = oneshot::channel();
    let projection_reader =
        NatsIntentReader::new(client.clone()).with_request_timeout(PROJECTION_READ_TIMEOUT);
    let task = tokio::spawn(async move {
        loop {
            let mut changed = match client.subscribe(INTENT_CHANGED).await {
                Ok(changed) => changed,
                Err(error) => {
                    state.record_fetch_failure(format!(
                        "subscribe to dataplane projection invalidations: {error}"
                    ));
                    tokio::select! {
                        () = tokio::time::sleep(PROJECTION_RESUBSCRIBE_DELAY) => continue,
                        _ = &mut shutdown_rx => return,
                    }
                }
            };
            converge_once(&projection_reader, &machine_id, &runner, &preparer, &state).await;
            loop {
                tokio::select! {
                    message = changed.next() => {
                        let Some(_) = message else { break };
                        converge_once(
                            &projection_reader,
                            &machine_id,
                            &runner,
                            &preparer,
                            &state,
                        )
                        .await;
                    }
                    _ = &mut shutdown_rx => return,
                }
            }
            tokio::select! {
                () = tokio::time::sleep(PROJECTION_RESUBSCRIBE_DELAY) => {}
                _ = &mut shutdown_rx => return,
            }
        }
    });
    RunningProjectionTask { shutdown, task }
}

async fn converge_once<R, P>(
    projection_reader: &NatsIntentReader,
    machine_id: &MachineId,
    runner: &R,
    preparer: &P,
    state: &MachineProjectionState,
) where
    R: MachineContainerRunner,
    P: MachinePloyzNativeMeshPreparer,
{
    let projection = match projection_reader.dataplane_projection().await {
        Ok(projection) => projection,
        Err(error) => {
            state.record_fetch_failure(error.to_string());
            return;
        }
    };
    let revisions = DataplaneProjectionRevisions {
        declared_revision: projection.declared_revision.clone(),
        target_revision: projection.target_revision.clone(),
    };
    if !projection
        .target_members
        .iter()
        .any(|member| &member.machine_id == machine_id)
    {
        state.record_failure(
            revisions,
            EndpointBridgeStatus::Unavailable {
                message: failure_message("local machine is missing from dataplane projection"),
            },
            DataplaneProjectionFailure::LocalMemberMissing,
        );
        return;
    }
    let rendered = match render_projection(projection, machine_id) {
        Ok(rendered) => rendered,
        Err(message) => {
            state.record_failure(
                revisions,
                EndpointBridgeStatus::Unavailable {
                    message: message.clone(),
                },
                DataplaneProjectionFailure::InvalidView { message },
            );
            return;
        }
    };

    if let Err(error) = runner
        .ensure_projection_endpoint_network(&rendered.local_subnet)
        .await
    {
        let (endpoint_bridge, failure) = endpoint_network_failure(error);
        state.record_failure(rendered.revisions, endpoint_bridge, failure);
        return;
    }
    let ready = match preparer
        .prepare_ployz_native_mesh(&rendered.endpoint_routes, &rendered.peers)
        .await
    {
        Ok(ready) => ready,
        Err(error) => {
            state.record_failure(
                rendered.revisions,
                EndpointBridgeStatus::Ready {
                    subnet: rendered.local_subnet,
                },
                prepare_failure(error),
            );
            return;
        }
    };
    if ready.wireguard.public_key != rendered.local_public_key {
        state.record_failure(
            rendered.revisions,
            EndpointBridgeStatus::Ready {
                subnet: rendered.local_subnet,
            },
            DataplaneProjectionFailure::InvalidView {
                message: failure_message(
                    "local WireGuard public key does not match dataplane projection",
                ),
            },
        );
        return;
    }
    state.record_applied(rendered.revisions, rendered.local_subnet);
}

struct RenderedProjection {
    revisions: DataplaneProjectionRevisions,
    local_subnet: MachineEndpointSubnet,
    local_public_key: ployz_core::dataplane::WireGuardPublicKey,
    endpoint_routes: Vec<WireGuardEbpfEndpointRoute>,
    peers: Vec<WireGuardPeer>,
}

fn render_projection(
    projection: DataplaneProjection,
    machine_id: &MachineId,
) -> Result<RenderedProjection, FailureMessage> {
    validate_projection(&projection)?;
    let Some(local) = projection
        .target_members
        .iter()
        .find(|member| &member.machine_id == machine_id)
    else {
        return Err(failure_message(format!(
            "machine {} is missing from dataplane projection",
            machine_id.as_str()
        )));
    };
    let endpoint_routes = projection
        .target_members
        .iter()
        .map(|member| WireGuardEbpfEndpointRoute {
            machine_id: member.machine_id.clone(),
            endpoint_subnet: member.endpoint_subnet.as_string(),
        })
        .collect();
    let peers = projection
        .target_members
        .iter()
        .filter(|member| &member.machine_id != machine_id)
        .map(|member| {
            let Some(active_endpoint) = member.mesh_endpoints.first().copied() else {
                return Err(failure_message(format!(
                    "dataplane peer {} has no mesh endpoint",
                    member.machine_id.as_str()
                )));
            };
            Ok(WireGuardPeer {
                machine_id: member.machine_id.clone(),
                endpoint_subnet: member.endpoint_subnet.as_string(),
                active_endpoint,
                candidate_endpoints: member.mesh_endpoints.clone(),
                public_key: member.wireguard_public_key.clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RenderedProjection {
        revisions: DataplaneProjectionRevisions {
            declared_revision: projection.declared_revision,
            target_revision: projection.target_revision,
        },
        local_subnet: local.endpoint_subnet.clone(),
        local_public_key: local.wireguard_public_key.clone(),
        endpoint_routes,
        peers,
    })
}

fn validate_projection(projection: &DataplaneProjection) -> Result<(), FailureMessage> {
    let staged = projection
        .target_members
        .iter()
        .filter(|target| {
            !projection
                .declared_members
                .iter()
                .any(|declared| declared.machine_id == target.machine_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let staged = match staged.as_slice() {
        [] => None,
        [member] => Some(member.clone()),
        _ => {
            return Err(failure_message(
                "dataplane projection has multiple staged machines",
            ));
        }
    };
    let rebuilt = DataplaneProjection::try_new(projection.declared_members.clone(), staged)
        .map_err(|error| failure_message(error.to_string()))?;
    if &rebuilt != projection {
        return Err(failure_message(
            "dataplane projection revisions or target membership are inconsistent",
        ));
    }
    Ok(())
}

fn endpoint_network_failure(
    error: MachineContainerRunnerError,
) -> (EndpointBridgeStatus, DataplaneProjectionFailure) {
    if let MachineContainerRunnerError::EndpointNetworkSubnetMismatch { expected, observed } = error
    {
        return (
            EndpointBridgeStatus::SubnetMismatch {
                expected: expected.clone(),
                observed: observed.clone(),
            },
            DataplaneProjectionFailure::EndpointBridgeSubnetMismatch { expected, observed },
        );
    }
    let message = failure_message(format!("{error:?}"));
    (
        EndpointBridgeStatus::Unavailable {
            message: message.clone(),
        },
        DataplaneProjectionFailure::ApplyFailed {
            component: DataplaneProjectionComponent::EndpointBridge,
            message,
        },
    )
}

fn prepare_failure(error: WireGuardEbpfPrepareError) -> DataplaneProjectionFailure {
    match error {
        WireGuardEbpfPrepareError::Unavailable {
            component, message, ..
        } => DataplaneProjectionFailure::ApplyFailed {
            component: match component {
                PloyzNativeMeshComponent::WireGuard => DataplaneProjectionComponent::WireGuard,
                PloyzNativeMeshComponent::EbpfForwarding => DataplaneProjectionComponent::Ebpf,
            },
            message,
        },
        WireGuardEbpfPrepareError::InvalidReport { message } => {
            DataplaneProjectionFailure::InvalidView { message }
        }
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    let message = message.into();
    FailureMessage::try_new(if message.is_empty() {
        "dataplane projection failed".to_owned()
    } else {
        message
    })
    .expect("projection failure message is non-empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::{DataplaneProjectionMember, WireGuardPublicKey};
    use std::net::SocketAddr;

    fn member(id: &str, subnet: &str, endpoint: Option<&str>) -> DataplaneProjectionMember {
        DataplaneProjectionMember {
            machine_id: MachineId::try_new(id).expect("machine id"),
            endpoint_subnet: MachineEndpointSubnet::try_new(subnet).expect("subnet"),
            mesh_endpoints: endpoint
                .map(|value| vec![value.parse::<SocketAddr>().expect("endpoint")])
                .unwrap_or_default(),
            wireguard_public_key: WireGuardPublicKey::try_new(format!("key-{id}"))
                .expect("public key"),
        }
    }

    #[test]
    fn render_uses_complete_target_and_excludes_local_peer() {
        let local = member("machine_a", "10.198.1.0/24", None);
        let peer = member("machine_b", "10.198.2.0/24", Some("192.0.2.2:51820"));
        let projection = DataplaneProjection::try_new(vec![local.clone()], Some(peer.clone()))
            .expect("projection");

        let rendered = render_projection(projection, &local.machine_id).expect("render");

        assert_eq!(rendered.endpoint_routes.len(), 2);
        assert_eq!(rendered.peers.len(), 1);
        let [rendered_peer] = rendered.peers.as_slice() else {
            panic!("expected one rendered peer");
        };
        assert_eq!(rendered_peer.machine_id, peer.machine_id);
        assert_eq!(rendered.local_subnet, local.endpoint_subnet);
    }

    #[test]
    fn render_rejects_peer_without_endpoint() {
        let local = member("machine_a", "10.198.1.0/24", None);
        let peer = member("machine_b", "10.198.2.0/24", None);
        let projection =
            DataplaneProjection::try_new(vec![local.clone()], Some(peer)).expect("projection");

        assert!(render_projection(projection, &local.machine_id).is_err());
    }

    #[test]
    fn projection_state_preserves_last_applied_revision_after_failure() {
        let state = MachineProjectionState::new();
        let projection =
            DataplaneProjection::try_new(vec![member("machine_a", "10.198.1.0/24", None)], None)
                .expect("projection");
        let revisions = DataplaneProjectionRevisions {
            declared_revision: projection.declared_revision,
            target_revision: projection.target_revision,
        };
        let subnet = MachineEndpointSubnet::try_new("10.198.1.0/24").expect("subnet");
        state.record_applied(revisions.clone(), subnet);

        state.record_fetch_failure("core unavailable".to_owned());

        assert!(matches!(
            state.snapshot().testimony,
            DataplaneProjectionTestimony::Unusable {
                last_applied_revisions: Some(last),
                ..
            } if last == revisions
        ));
    }

    #[test]
    fn subnet_mismatch_is_typed_and_preserves_both_values() {
        let expected = MachineEndpointSubnet::try_new("10.198.1.0/24").expect("expected");
        let observed = MachineEndpointSubnet::try_new("10.198.2.0/24").expect("observed");

        let (bridge, failure) =
            endpoint_network_failure(MachineContainerRunnerError::EndpointNetworkSubnetMismatch {
                expected: expected.clone(),
                observed: observed.clone(),
            });

        assert_eq!(
            bridge,
            EndpointBridgeStatus::SubnetMismatch {
                expected: expected.clone(),
                observed: observed.clone(),
            }
        );
        assert_eq!(
            failure,
            DataplaneProjectionFailure::EndpointBridgeSubnetMismatch { expected, observed }
        );
    }

    #[test]
    fn projection_state_reports_exact_attempted_revision_and_typed_failure() {
        let state = MachineProjectionState::new();
        let projection =
            DataplaneProjection::try_new(vec![member("machine_a", "10.198.1.0/24", None)], None)
                .expect("projection");
        let revisions = DataplaneProjectionRevisions {
            declared_revision: projection.declared_revision,
            target_revision: projection.target_revision,
        };
        state.record_failure(
            revisions.clone(),
            EndpointBridgeStatus::Missing,
            DataplaneProjectionFailure::EndpointBridgeMissing,
        );

        assert!(matches!(
            state.snapshot().testimony,
            DataplaneProjectionTestimony::Unusable {
                attempted_revisions: Some(attempted),
                failure: DataplaneProjectionFailure::EndpointBridgeMissing,
                ..
            } if attempted == revisions
        ));
    }
}
