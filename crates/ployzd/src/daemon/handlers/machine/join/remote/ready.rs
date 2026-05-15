use ployz_api::{DaemonPayload, DaemonRequest, DaemonResponse, MeshReadyPayload};
use ployz_model::MachineMembership;
use ployz_nats::NatsNodeRpcClient;
use ployz_node_api::NodeResponse;
use ployz_node_runtime::{
    MeshReadinessNodeClient, MeshReadinessRpcOperation, NODE_READINESS_RPC_POLICY,
    NodeProbeNodeClient,
};
use tokio::time::{Duration, Instant, sleep, timeout};

use crate::daemon::node_rpc::{
    MESH_READY_PAYLOAD_KIND, NatsMeshReadinessRpcTransport, NatsNodeProbeRpcTransport,
    decode_daemon_node_payload,
};
use crate::daemon::ssh::SshOptions;

use super::super::super::types::RemoteReadyWaitPolicy;
use super::{node_rpc_remote_error, remote_rpc};

const REMOTE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_READY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const NATS_READY_POLL_INTERVAL: Duration = Duration::from_millis(100);
const REMOTE_READY_RPC_TIMEOUT: Duration = Duration::from_secs(10);

fn production_remote_ready_wait_policy() -> RemoteReadyWaitPolicy {
    RemoteReadyWaitPolicy::new(
        REMOTE_READY_TIMEOUT,
        REMOTE_READY_POLL_INTERVAL,
        REMOTE_READY_RPC_TIMEOUT,
    )
}

pub(in crate::daemon::handlers::machine::join) async fn wait_for_remote_ready(
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

pub(in crate::daemon::handlers::machine::join) async fn wait_for_nats_command_responder(
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

pub(in crate::daemon::handlers::machine::join) async fn wait_for_nats_ready(
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
