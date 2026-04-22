use std::net::{IpAddr, SocketAddr};

use ployz_api::{
    DaemonPayload, DaemonRequest, DaemonResponse, MeshReadyPayload, MeshSelfRecordPayload,
};
use ployz_sdk::Transport;
use ployz_store_api::StoreDriver;
use ployz_types::model::{
    JOIN_RESPONSE_PREFIX, JoinResponse, MachineId, MachineRecord, OverlayIp, Participation,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{Duration, Instant, sleep, timeout};

use crate::daemon::ssh::{SshOptions, ssh_stdio_transport};

const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const REMOTE_READY_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const PEER_RPC_TIMEOUT: Duration = Duration::from_secs(3);
const MACHINE_STATE_SYNC_TIMEOUT: Duration = Duration::from_secs(20);
const MACHINE_STATE_SYNC_POLL_INTERVAL: Duration = Duration::from_millis(500);
const REMOTE_RPC_COMMAND: &str = "set -eu; \"$HOME/.local/bin/ployz\" rpc-stdio";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ExpectedSubnetState {
    Present,
    Absent,
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
) -> Result<MachineRecord, String> {
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
    payload.ready || (payload.phase == "running" && payload.store_healthy && payload.heartbeat_started)
}

pub(super) async fn overlay_rpc(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<DaemonResponse, String> {
    let address = SocketAddr::new(IpAddr::V6(overlay_ip.0), peer_rpc_port);
    let stream = timeout(PEER_RPC_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc connect {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc connect {address}: {error}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(&request)
        .map_err(|error| format!("encode overlay rpc request: {error}"))?;
    line.push('\n');
    timeout(PEER_RPC_TIMEOUT, writer.write_all(line.as_bytes()))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc write {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc write {address}: {error}"))?;
    timeout(PEER_RPC_TIMEOUT, writer.shutdown())
        .await
        .map_err(|_| {
            format!(
                "overlay rpc shutdown {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc shutdown {address}: {error}"))?;

    let mut response_line = String::new();
    let mut reader = BufReader::new(reader);
    timeout(PEER_RPC_TIMEOUT, reader.read_line(&mut response_line))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc read {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc read {address}: {error}"))?;
    serde_json::from_str(&response_line)
        .map_err(|error| format!("decode overlay rpc response: {error}"))
}

pub(super) async fn overlay_rpc_expect_ok(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<(), String> {
    let response = overlay_rpc(overlay_ip, peer_rpc_port, request).await?;
    if response.ok {
        return Ok(());
    }
    Err(remote_response_error(&response))
}

async fn rollback_remote_enable(overlay_ip: OverlayIp, peer_rpc_port: u16) -> Result<(), String> {
    overlay_rpc_expect_ok(
        overlay_ip,
        peer_rpc_port,
        DaemonRequest::MeshStandby { force: true },
    )
    .await
}

pub(super) async fn log_remote_enable_rollback(
    machine: &MachineRecord,
    peer_rpc_port: u16,
    original_error: &str,
) {
    if let Err(rollback_error) = rollback_remote_enable(machine.overlay_ip, peer_rpc_port).await {
        tracing::warn!(
            machine = %machine.id,
            error = %rollback_error,
            original_error,
            "remote enable rollback failed"
        );
    }
}

pub(super) async fn overlay_self_record(
    machine: &MachineRecord,
    peer_rpc_port: u16,
) -> Result<MachineRecord, String> {
    let response = overlay_rpc(
        machine.overlay_ip,
        peer_rpc_port,
        DaemonRequest::MeshSelfRecord,
    )
    .await?;
    if !response.ok {
        return Err(remote_response_error(&response));
    }
    match response.payload {
        Some(DaemonPayload::MeshSelfRecord(MeshSelfRecordPayload { record, .. })) => Ok(record),
        Some(payload) => Err(format!("unexpected self-record payload: {payload:?}")),
        None => decode_joiner_record(&response.message),
    }
}

pub(super) async fn wait_for_overlay_ready(
    machine: &MachineRecord,
    peer_rpc_port: u16,
) -> Result<(), String> {
    let deadline = Instant::now() + REMOTE_READY_TIMEOUT;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        let last_error = match timeout(
            REMOTE_READY_RPC_TIMEOUT,
            overlay_rpc(
                machine.overlay_ip,
                peer_rpc_port,
                DaemonRequest::MeshReady { json: false },
            ),
        )
        .await
        {
            Ok(Ok(response)) => match mesh_ready_payload(&response) {
                Ok(payload) => {
                    if remote_join_ready(&payload) {
                        tracing::debug!(machine = %machine.id, attempt, "overlay mesh ready confirmed");
                        return Ok(());
                    }
                    format!("mesh reported not ready yet: {}", response.message)
                }
                Err(err) => err,
            },
            Ok(Err(err)) => err,
            Err(_) => format!(
                "overlay readiness probe exceeded {:?}",
                REMOTE_READY_RPC_TIMEOUT
            ),
        };

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for overlay mesh readiness after {:?}: {last_error}",
                REMOTE_READY_TIMEOUT,
            ));
        }

        sleep(REMOTE_READY_POLL_INTERVAL).await;
    }
}

pub(super) async fn wait_for_machine_projection(
    store: &StoreDriver,
    machine_id: &MachineId,
    expected_participation: Participation,
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
        if record.participation == expected_participation && subnet_matches {
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
                "timed out waiting for machine '{}' to reach participation='{}' subnet={expected_subnet}; observed participation='{}' subnet={actual_subnet}",
                machine_id, expected_participation, record.participation,
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

fn decode_joiner_record(output: &str) -> Result<MachineRecord, String> {
    let response_line = match output
        .lines()
        .find(|line| line.starts_with(JOIN_RESPONSE_PREFIX))
    {
        Some(line) => line,
        None => {
            return Err(format!(
                "self-record output missing {JOIN_RESPONSE_PREFIX} line\nhint: run `ployz mesh self-record` on the joiner and `ployz mesh accept <response>` on this machine"
            ));
        }
    };

    let join_response = JoinResponse::decode(response_line)
        .map_err(|err| format!("failed to decode join response: {err}"))?;
    Ok(join_response.into_seed_machine_record())
}
