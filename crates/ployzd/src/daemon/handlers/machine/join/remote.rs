use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MachineTransitionGoal, MeshReadyPayload,
    MeshSelfRecordPayload, StatusPayload,
};
use ployz_nats::coord::rpc::{NatsNodeRpcClient, NodeCommandSubject};
use ployz_sdk::Transport;
use ployz_store_api::StoreDriver;
use ployz_types::model::{
    JOIN_RESPONSE_PREFIX, JoinResponse, MachineId, MachineLifecycle, MachineMembership, PublicKey,
};
use tokio::time::{Duration, Instant, sleep, timeout};

use crate::daemon::ssh::{SshOptions, ssh_stdio_transport};

const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REMOTE_READY_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const MACHINE_STATE_SYNC_TIMEOUT: Duration = Duration::from_secs(20);
const MACHINE_STATE_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(500);
const REMOTE_RPC_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployzctl\" rpc-stdio";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedSubnetState {
    Present,
    Absent,
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
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    match response.payload {
        Some(DaemonPayload::Status(StatusPayload {
            machine_id,
            public_key,
            ..
        })) => Ok(RemoteDaemonIdentity {
            machine_id: MachineId(machine_id),
            public_key,
        }),
        Some(payload) => Err(format!("unexpected status payload: {payload:?}")),
        None => Err("status response missing structured payload".to_string()),
    }
}

pub(super) async fn wait_for_remote_ready(
    target: &str,
    ssh_options: &SshOptions,
) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_RPC_TIMEOUT,
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
                    format!("mesh reported not ready yet: {}", response.message)
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
                    REMOTE_READY_RPC_TIMEOUT
                );
                tracing::debug!(%target, attempt, error = %err, "remote readiness rpc timed out");
                err
            }
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for remote mesh readiness after {:?}: {last_error}",
                REMOTE_READY_TIMEOUT,
            ));
        }

        sleep(REMOTE_READY_POLL_INTERVAL).await;
    }
}

pub(super) async fn remote_self_record(
    target: &str,
    ssh_options: &SshOptions,
) -> Result<MachineMembership, String> {
    let response = remote_rpc(target, DaemonRequest::MeshSelfRecord, ssh_options).await?;
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    match response.payload {
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload { record, .. })) => Ok(record),
        Some(payload) => Err(format!("unexpected self-record payload: {payload:?}")),
        None => decode_joiner_record(&response.message),
    }
}

pub(super) async fn nats_self_record(
    client: &NatsNodeRpcClient,
    machine: &MachineMembership,
) -> Result<MachineMembership, String> {
    let response = client
        .request(
            NodeCommandSubject::mesh_self_record(&machine.id),
            &DaemonRequest::MeshSelfRecord,
        )
        .await
        .map_err(|error| error.to_string())?;
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    match response.payload {
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload { record, .. })) => Ok(record),
        Some(payload) => Err(format!("unexpected self-record payload: {payload:?}")),
        None => decode_joiner_record(&response.message),
    }
}

pub(super) async fn nats_rpc_expect_ok(
    client: &NatsNodeRpcClient,
    subject: NodeCommandSubject,
    request: DaemonRequest,
) -> Result<(), String> {
    let response = client
        .request(subject, &request)
        .await
        .map_err(|error| error.to_string())?;
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

fn mesh_ready_payload(response: &DaemonResponse) -> Result<MeshReadyPayload, String> {
    match &response.payload {
        Some(DaemonPayload::MeshReady(payload)) => Ok(payload.clone()),
        Some(payload) => Err(format!("unexpected readiness payload: {payload:?}")),
        None => parse_remote_ready_payload(&response.message),
    }
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
    let request = DaemonRequest::MachineTransitionSelf {
        goal: MachineTransitionGoal::Standby,
        assigned_subnet: None,
        force: true,
    };
    if let Err(rollback_error) = nats_rpc_expect_ok(
        client,
        NodeCommandSubject::machine_transition_self(&machine.id),
        request,
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

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_RPC_TIMEOUT,
            client.request(
                NodeCommandSubject::mesh_ready(&machine.id),
                &DaemonRequest::MeshReady { json: false },
            ),
        )
        .await
        {
            Ok(Ok(response)) => match mesh_ready_payload(&response) {
                Ok(payload) => {
                    if remote_join_ready(&payload) {
                        tracing::debug!(machine = %machine.id, attempt, "NATS mesh ready confirmed");
                        return Ok(());
                    }
                    format!("mesh reported not ready yet: {}", response.message)
                }
                Err(err) => err,
            },
            Ok(Err(err)) => err.to_string(),
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

        sleep(REMOTE_READY_POLL_INTERVAL).await;
    }
}

pub(super) async fn wait_for_machine_projection(
    store: &StoreDriver,
    machine_id: &MachineId,
    expected_lifecycle: MachineLifecycle,
    expected_subnet: ExpectedSubnetState,
) -> Result<(), String> {
    let deadline = Instant::now() + MACHINE_STATE_SYNC_TIMEOUT;

    loop {
        let Some(record) = super::super::list::find_machine_record(store, machine_id).await? else {
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for machine '{}' to appear in local store",
                    machine_id
                ));
            }
            sleep(MACHINE_STATE_SYNC_POLL_INTERVAL).await;
            continue;
        };

        let subnet_matches = match expected_subnet {
            ExpectedSubnetState::Present => record.subnet.is_some(),
            ExpectedSubnetState::Absent => record.subnet.is_none(),
        };
        if record.lifecycle == expected_lifecycle && subnet_matches {
            return Ok(());
        }

        if Instant::now() >= deadline {
            let expected_subnet = match expected_subnet {
                ExpectedSubnetState::Present => "present",
                ExpectedSubnetState::Absent => "absent",
            };
            let actual_subnet = if record.subnet.is_some() {
                "present"
            } else {
                "absent"
            };
            return Err(format!(
                "timed out waiting for machine '{}' to reach lifecycle='{}' subnet={expected_subnet}; observed lifecycle='{}' subnet={actual_subnet}",
                machine_id, expected_lifecycle, record.lifecycle,
            ));
        }

        sleep(MACHINE_STATE_SYNC_POLL_INTERVAL).await;
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
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

pub(super) fn remote_response_error(response: &DaemonResponse) -> String {
    format!(
        "remote daemon error [{}]: {}",
        response.code, response.message
    )
}

fn decode_joiner_record(output: &str) -> Result<MachineMembership, String> {
    let response_line = match output
        .lines()
        .find(|line| line.starts_with(JOIN_RESPONSE_PREFIX))
    {
        Some(line) => line,
        None => {
            return Err(format!(
                "self-record output missing {JOIN_RESPONSE_PREFIX} line\nhint: run `ployzctl mesh self-record` on the joiner and `ployzctl mesh accept <response>` on this machine"
            ));
        }
    };

    let join_response = JoinResponse::decode(response_line)
        .map_err(|err| format!("failed to decode join response: {err}"))?;
    Ok(join_response.into_seed_machine_membership())
}
