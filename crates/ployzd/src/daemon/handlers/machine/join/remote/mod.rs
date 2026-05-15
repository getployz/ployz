use ployz_api::{
    DaemonPayload, DaemonRequest, MachineSelfTransition, MeshSelfRecordPayload, StatusPayload,
};
use ployz_model::{MachineEvent, MachineId, MachineLifecycle, MachineMembership, PublicKey};
use ployz_nats::NatsNodeRpcClient;
use ployz_node_api::NodeResponse;
use ployz_node_runtime::{
    MachineLifecycleNodeClient, MeshReadinessNodeClient, MeshReadinessRpcOperation, NodeRpcError,
};
use ployz_store_api::{MachineMembershipStore, StoreDriver};
use tokio::time::{Duration, Instant, timeout};

use crate::daemon::node_rpc::{
    MESH_SELF_RECORD_PAYLOAD_KIND, NatsMachineLifecycleRpcTransport, NatsMeshReadinessRpcTransport,
    decode_daemon_node_payload,
};
use crate::daemon::ssh::SshOptions;

mod ready;
mod rpc;

const MACHINE_STATE_SYNC_TIMEOUT: Duration = Duration::from_secs(20);

pub(super) use ready::{
    wait_for_nats_command_responder, wait_for_nats_ready, wait_for_remote_ready,
};
pub(super) use rpc::{remote_response_error, remote_rpc, remote_rpc_expect_ok};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedSubnetState {
    Present,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExpectedMachineRecord {
    lifecycle: MachineLifecycle,
    subnet: ExpectedSubnetState,
}

impl ExpectedMachineRecord {
    pub(super) fn new(lifecycle: MachineLifecycle, subnet: ExpectedSubnetState) -> Self {
        Self { lifecycle, subnet }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RemoteDaemonIdentity {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
}

pub(super) async fn remote_daemon_identity(
    target: &str,
    ssh_options: &SshOptions,
) -> Result<RemoteDaemonIdentity, String> {
    let response = remote_rpc(target, DaemonRequest::Status, ssh_options).await?;
    if !response.is_ok() {
        return Err(remote_response_error(&response));
    }
    match response.payload() {
        Some(DaemonPayload::Status(StatusPayload {
            machine_id,
            public_key,
            ..
        })) => Ok(RemoteDaemonIdentity {
            machine_id: MachineId::new(machine_id),
            public_key,
        }),
        Some(payload) => Err(format!("unexpected status payload: {payload:?}")),
        None => Err("status response missing structured payload".to_string()),
    }
}

pub(super) async fn remote_self_record(
    target: &str,
    ssh_options: &SshOptions,
) -> Result<MachineMembership, String> {
    let response = remote_rpc(target, DaemonRequest::MeshSelfRecord, ssh_options).await?;
    if !response.is_ok() {
        return Err(remote_response_error(&response));
    }
    match response.payload() {
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload { record })) => Ok(record),
        Some(payload) => Err(format!("unexpected self-record payload: {payload:?}")),
        None => Err("self-record response missing structured payload".to_string()),
    }
}

pub(super) async fn nats_self_record(
    client: &NatsNodeRpcClient,
    machine: &MachineMembership,
) -> Result<MachineMembership, String> {
    let response = MeshReadinessNodeClient::new(NatsMeshReadinessRpcTransport::new(client.clone()))
        .self_record(&machine.id)
        .await
        .map_err(node_rpc_remote_error)?;
    nats_self_record_payload(response)
}

pub(super) async fn nats_transition_self(
    client: &NatsNodeRpcClient,
    machine: &MachineMembership,
    transition: MachineSelfTransition,
) -> Result<(), String> {
    MachineLifecycleNodeClient::new(NatsMachineLifecycleRpcTransport::new(client.clone()))
        .transition_self(&machine.id, transition)
        .await
        .map_err(|error| error.to_string())
}

fn nats_self_record_payload(response: NodeResponse) -> Result<MachineMembership, String> {
    let payload = decode_daemon_node_payload::<MeshSelfRecordPayload>(
        MeshReadinessRpcOperation::SelfRecord.operation_name(),
        response,
        MESH_SELF_RECORD_PAYLOAD_KIND,
    )
    .map_err(|error| error.to_string())?;
    Ok(payload.record)
}

fn node_rpc_remote_error(error: NodeRpcError) -> String {
    format!("remote daemon error [{}]: {}", error.code, error.message)
}

pub(super) async fn log_nats_enable_rollback(
    client: &NatsNodeRpcClient,
    machine: &MachineMembership,
    original_error: &str,
) {
    if let Err(rollback_error) = nats_transition_self(
        client,
        machine,
        MachineSelfTransition::Standby { force: true },
    )
    .await
    {
        tracing::warn!(
            machine = %machine.id,
            error = %rollback_error,
            original_error,
            "remote enable rollback failed"
        );
    }
}

pub(super) async fn wait_for_machine_record(
    store: &StoreDriver,
    machine_id: &MachineId,
    expected: ExpectedMachineRecord,
) -> Result<(), String> {
    let deadline = Instant::now() + MACHINE_STATE_SYNC_TIMEOUT;
    let (snapshot, mut events) = store
        .subscribe_machines()
        .await
        .map_err(|err| format!("subscribe to machine records: {err}"))?;

    if machine_record_matches(
        snapshot.iter().find(|record| record.id == *machine_id),
        expected,
    ) {
        return Ok(());
    }

    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(machine_record_timeout(store, machine_id, expected).await);
        };

        match timeout(remaining, events.recv()).await {
            Ok(Some(Ok(MachineEvent::Upsert(record)))) => {
                if record.id == *machine_id && machine_record_matches(Some(&record), expected) {
                    return Ok(());
                }
            }
            Ok(Some(Ok(MachineEvent::Removed { .. }))) => {}
            Ok(Some(Err(err))) => {
                return Err(format!("machine record subscription failed: {err}"));
            }
            Ok(None) => return Err("machine record subscription closed".into()),
            Err(_) => {
                return Err(machine_record_timeout(store, machine_id, expected).await);
            }
        }
    }
}

fn machine_record_matches(
    record: Option<&MachineMembership>,
    expected: ExpectedMachineRecord,
) -> bool {
    let Some(record) = record else {
        return false;
    };
    let subnet_matches = match expected.subnet {
        ExpectedSubnetState::Present => record.subnet.is_some(),
        ExpectedSubnetState::Absent => record.subnet.is_none(),
    };
    record.lifecycle == expected.lifecycle && subnet_matches
}

async fn machine_record_timeout(
    store: &StoreDriver,
    machine_id: &MachineId,
    expected: ExpectedMachineRecord,
) -> String {
    match super::super::list::find_machine_record(store, machine_id).await {
        Ok(Some(record)) => {
            let expected_subnet = expected.subnet.label();
            let actual_subnet = if record.subnet.is_some() {
                "present"
            } else {
                "absent"
            };
            format!(
                "timed out waiting for machine '{}' to reach lifecycle='{}' subnet={expected_subnet}; observed lifecycle='{}' subnet={actual_subnet}",
                machine_id, expected.lifecycle, record.lifecycle,
            )
        }
        Ok(None) => format!(
            "timed out waiting for machine '{}' to appear in observed machine records",
            machine_id
        ),
        Err(err) => format!(
            "timed out waiting for machine '{}' record and failed to inspect final state: {err}",
            machine_id
        ),
    }
}

impl ExpectedSubnetState {
    fn label(self) -> &'static str {
        match self {
            Self::Present => "present",
            Self::Absent => "absent",
        }
    }
}
