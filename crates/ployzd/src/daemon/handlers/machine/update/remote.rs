use std::time::Duration;

use ployz_api::{MachineOperationPayload, StatusPayload};
use ployz_model::{MachineId, MachineMembership};
use ployz_node_runtime::{
    MACHINE_OPERATION_RPC_POLICY, MachineOperationNodeClient, MachineOperationRpcOperation,
    MachineOperationRpcTransport, MachineUpdateNodeClient, NODE_STATUS_RPC_POLICY,
    NodeProbeNodeClient, NodeProbeRpcOperation, NodeProbeRpcTransport,
};
use tokio::time::{Instant, sleep};

use crate::daemon::DaemonState;
use crate::daemon::handlers::machine::list::find_machine_record;
use crate::daemon::node_rpc::{
    MACHINE_OPERATION_PAYLOAD_KIND, NatsMachineOperationRpcTransport,
    NatsMachineUpdateRpcTransport, NatsNodeProbeRpcTransport, STATUS_PAYLOAD_KIND,
    decode_daemon_node_payload,
};

use super::update_row;

const UPDATE_READINESS_TIMEOUT: Duration = Duration::from_secs(120);
const UPDATE_READINESS_INTERVAL: Duration = Duration::from_secs(1);

impl DaemonState {
    pub(super) async fn update_remote_machine(
        &self,
        operation_id: &str,
        target: &str,
        version: &str,
    ) -> Result<ployz_api::MachineUpdateRow, ployz_api::MachineUpdateRow> {
        let machine_id =
            MachineId::try_new(target).map_err(|error| ployz_api::MachineUpdateRow {
                id: target.to_string(),
                version: version.to_string(),
                message: error,
            })?;
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => {
                return Err(update_row(&machine_id, version, response.message()));
            }
        };
        let record = match find_machine_record(&active.mesh.store, &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err(update_row(
                    &machine_id,
                    version,
                    format!("machine '{target}' not found"),
                ));
            }
            Err(error) => {
                return Err(update_row(
                    &machine_id,
                    version,
                    format!("failed to read machines: {error}"),
                ));
            }
        };
        let client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return Err(update_row(&machine_id, version, error)),
        };
        let update_client =
            MachineUpdateNodeClient::new(NatsMachineUpdateRpcTransport::new(client.clone()));
        let operation_client =
            MachineOperationNodeClient::new(NatsMachineOperationRpcTransport::new(client.clone()))
                .with_policy(MACHINE_OPERATION_RPC_POLICY);
        let probe_client = NodeProbeNodeClient::new(NatsNodeProbeRpcTransport::new(client))
            .with_policy(NODE_STATUS_RPC_POLICY);

        if let Err(error) = update_client
            .prepare_update(&record.id, operation_id, version)
            .await
        {
            return Err(update_row(
                &machine_id,
                version,
                format!("prepare rejected: {error}"),
            ));
        }

        if let Err(error) = update_client
            .execute_update(&record.id, operation_id, version)
            .await
        {
            return Err(update_row(
                &machine_id,
                version,
                format!("execute rejected: {error}"),
            ));
        }

        match wait_for_remote_update(
            &operation_client,
            &probe_client,
            record,
            operation_id,
            version,
        )
        .await
        {
            Ok(message) => Ok(update_row(&machine_id, version, message)),
            Err(error) => Err(update_row(&machine_id, version, error)),
        }
    }
}

async fn wait_for_remote_update<O, P>(
    operation_client: &MachineOperationNodeClient<O>,
    probe_client: &NodeProbeNodeClient<P>,
    record: MachineMembership,
    operation_id: &str,
    version: &str,
) -> Result<String, String>
where
    O: MachineOperationRpcTransport,
    P: NodeProbeRpcTransport,
{
    sleep(Duration::from_secs(2)).await;
    let deadline = Instant::now() + UPDATE_READINESS_TIMEOUT;
    let mut last_error = String::from("remote update operation did not complete");
    while Instant::now() < deadline {
        match operation_client.get(&record.id, operation_id).await {
            Ok(response) => {
                let payload = match decode_daemon_node_payload::<MachineOperationPayload>(
                    MachineOperationRpcOperation::Get.operation_name(),
                    response,
                    MACHINE_OPERATION_PAYLOAD_KIND,
                ) {
                    Ok(payload) => payload,
                    Err(error) => {
                        last_error = error.to_string();
                        sleep(UPDATE_READINESS_INTERVAL).await;
                        continue;
                    }
                };
                match payload.operation.status.as_str() {
                    "succeeded" => {
                        if version == "latest" {
                            return Ok("remote update operation succeeded".into());
                        }
                        verify_remote_version(probe_client, &record, version).await?;
                        return Ok(format!("ready with version {version}"));
                    }
                    "failed" | "interrupted" => {
                        return Err(payload.operation.last_error.unwrap_or_else(|| {
                            format!("remote update {}", payload.operation.status)
                        }));
                    }
                    "running" => {
                        last_error = format!("remote update stage {}", payload.operation.stage);
                    }
                    other => {
                        last_error = format!("remote update has unknown status '{other}'");
                    }
                }
            }
            Err(error) => {
                last_error = error.to_string();
            }
        }
        sleep(UPDATE_READINESS_INTERVAL).await;
    }
    Err(last_error)
}

async fn verify_remote_version<P>(
    client: &NodeProbeNodeClient<P>,
    record: &MachineMembership,
    version: &str,
) -> Result<(), String>
where
    P: NodeProbeRpcTransport,
{
    let response = client
        .status(&record.id)
        .await
        .map_err(|error| format!("remote status failed [{}]: {}", error.code, error.message))?;
    let status = decode_daemon_node_payload::<StatusPayload>(
        NodeProbeRpcOperation::Status.operation_name(),
        response,
        STATUS_PAYLOAD_KIND,
    )
    .map_err(|error| error.to_string())?;
    if status.version == version {
        Ok(())
    } else {
        Err(format!(
            "remote reports version {}, expected {}",
            status.version, version
        ))
    }
}
