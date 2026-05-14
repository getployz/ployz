use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineSelfTransition, MeshReadyPayload,
    MeshSelfRecordPayload, StatusPayload,
};
use ployz_model::{MachineEvent, MachineId, MachineLifecycle, MachineMembership, PublicKey};
use ployz_nats::NatsNodeRpcClient;
use ployz_node_api::NodeResponse;
use ployz_node_runtime::{
    MachineLifecycleNodeClient, MeshReadinessNodeClient, MeshReadinessRpcOperation,
    NODE_READINESS_RPC_POLICY, NodeProbeNodeClient, NodeRpcError,
};
use ployz_sdk::Transport;
use ployz_store_api::{MachineMembershipStore, StoreDriver};
use tokio::time::{Duration, Instant, sleep, timeout};

use crate::daemon::node_rpc::{
    MESH_READY_PAYLOAD_KIND, MESH_SELF_RECORD_PAYLOAD_KIND, NatsMachineLifecycleRpcTransport,
    NatsMeshReadinessRpcTransport, NatsNodeProbeRpcTransport, decode_daemon_node_payload,
};
use crate::daemon::ssh::{SshOptions, ssh_stdio_transport};

use super::super::types::RemoteReadyWaitPolicy;

const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const NATS_READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const REMOTE_READY_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const MACHINE_STATE_SYNC_TIMEOUT: Duration = Duration::from_secs(20);
const REMOTE_RPC_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployzctl\" rpc-stdio";

fn production_remote_ready_wait_policy() -> RemoteReadyWaitPolicy {
    RemoteReadyWaitPolicy::new(
        REMOTE_READY_TIMEOUT,
        REMOTE_READY_POLL_INTERVAL,
        REMOTE_READY_RPC_TIMEOUT,
    )
}

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

pub(super) async fn wait_for_remote_ready(
    target: &str,
    ssh_options: &SshOptions,
    wait_policy: Option<RemoteReadyWaitPolicy>,
) -> Result<(), String> {
    let policy = wait_policy.unwrap_or_else(production_remote_ready_wait_policy);
    let deadline = Instant::now() + policy.ready_timeout;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let last_error = match timeout(
            policy.ready_rpc_timeout,
            remote_rpc(
                target,
                DaemonRequest::MeshReady { json: false },
                ssh_options,
            ),
        )
        .await
        {
            Ok(Ok(response)) => match mesh_ready_payload(&response) {
                Ok(payload) => {
                    if remote_join_ready(&payload) {
                        tracing::debug!(%target, attempt, "remote mesh ready confirmed");
                        return Ok(());
                    }
                    tracing::debug!(%target, attempt, ?payload, "remote mesh not ready yet");
                    format!("mesh reported not ready yet: {}", response.message())
                }
                Err(err) => {
                    tracing::debug!(%target, attempt, error = %err, "remote readiness payload parse failed");
                    err
                }
            },
            Ok(Err(err)) => {
                tracing::debug!(%target, attempt, error = %err, "remote readiness rpc failed");
                err
            }
            Err(_) => {
                let err = format!(
                    "rpc readiness probe exceeded {:?}",
                    policy.ready_rpc_timeout
                );
                tracing::debug!(%target, attempt, error = %err, "remote readiness rpc timed out");
                err
            }
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for remote mesh readiness after {:?}: {last_error}",
                policy.ready_timeout,
            ));
        }

        sleep(policy.ready_poll_interval).await;
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

pub(super) async fn wait_for_nats_command_responder(
    client: &NatsNodeRpcClient,
    machine: &MachineMembership,
) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;
    let probe_client = NodeProbeNodeClient::new(NatsNodeProbeRpcTransport::new(client.clone()))
        .with_policy(NODE_READINESS_RPC_POLICY);

    loop {
        attempt += 1;
        let last_error = match timeout(REMOTE_READY_RPC_TIMEOUT, probe_client.ping(&machine.id))
            .await
        {
            Ok(Ok(())) => {
                tracing::debug!(machine = %machine.id, attempt, "NATS command responder confirmed");
                return Ok(());
            }
            Ok(Err(err)) => node_rpc_remote_error(err),
            Err(_) => format!(
                "NATS command responder probe exceeded {:?}",
                REMOTE_READY_RPC_TIMEOUT
            ),
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for NATS command responder after {:?}: {last_error}",
                REMOTE_READY_TIMEOUT,
            ));
        }

        sleep(NATS_READY_POLL_INTERVAL).await;
    }
}

fn mesh_ready_payload(response: &DaemonResponse) -> Result<MeshReadyPayload, String> {
    match &response.payload() {
        Some(DaemonPayload::MeshReady(payload)) => Ok(payload.clone()),
        Some(payload) => Err(format!("unexpected readiness payload: {payload:?}")),
        None => parse_remote_ready_payload(response.message()),
    }
}

fn nats_mesh_ready_payload(response: NodeResponse) -> Result<MeshReadyPayload, String> {
    decode_daemon_node_payload::<MeshReadyPayload>(
        MeshReadinessRpcOperation::Ready.operation_name(),
        response,
        MESH_READY_PAYLOAD_KIND,
    )
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

fn parse_remote_ready_payload(output: &str) -> Result<MeshReadyPayload, String> {
    if let Ok(payload) = serde_json::from_str::<MeshReadyPayload>(output) {
        return Ok(payload);
    }

    #[derive(serde::Deserialize)]
    struct RemoteReadyEnvelope {
        message: String,
    }

    let envelope = serde_json::from_str::<RemoteReadyEnvelope>(output)
        .map_err(|error| format!("failed to parse remote readiness envelope: {error}"))?;
    serde_json::from_str::<MeshReadyPayload>(&envelope.message)
        .map_err(|error| format!("failed to parse remote readiness message: {error}"))
}

fn remote_join_ready(payload: &MeshReadyPayload) -> bool {
    payload.ready || (payload.phase == "running" && payload.store_healthy && payload.sync_connected)
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

pub(super) async fn wait_for_nats_ready(
    client: &NatsNodeRpcClient,
    machine: &MachineMembership,
) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;
    let readiness_client =
        MeshReadinessNodeClient::new(NatsMeshReadinessRpcTransport::new(client.clone()))
            .with_policy(NODE_READINESS_RPC_POLICY);

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_RPC_TIMEOUT,
            readiness_client.ready(&machine.id, false),
        )
        .await
        {
            Ok(Ok(response)) => {
                let response_message = response.message().to_string();
                match nats_mesh_ready_payload(response) {
                    Ok(payload) => {
                        if remote_join_ready(&payload) {
                            tracing::debug!(machine = %machine.id, attempt, "NATS mesh ready confirmed");
                            return Ok(());
                        }
                        format!("mesh reported not ready yet: {response_message}")
                    }
                    Err(err) => err,
                }
            }
            Ok(Err(err)) => node_rpc_remote_error(err),
            Err(_) => format!(
                "NATS readiness probe exceeded {:?}",
                REMOTE_READY_RPC_TIMEOUT
            ),
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for NATS mesh readiness after {:?}: {last_error}",
                REMOTE_READY_TIMEOUT,
            ));
        }

        sleep(NATS_READY_POLL_INTERVAL).await;
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

pub(super) async fn remote_rpc(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<DaemonResponse, String> {
    let transport = ssh_stdio_transport(target, REMOTE_RPC_COMMAND, ssh_options);
    transport.request(request).await.map_err(|err| {
        format!(
            "remote rpc via '{}' failed: {err}",
            transport.command_display()
        )
    })
}

pub(super) async fn remote_rpc_expect_ok(
    target: &str,
    request: DaemonRequest,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let response = remote_rpc(target, request, ssh_options).await?;
    if response.is_ok() {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

pub(super) fn remote_response_error(response: &DaemonResponse) -> String {
    format!(
        "remote daemon error [{}]: {}",
        response.code(),
        response.message()
    )
}
