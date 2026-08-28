//! Client-owned barrier over replicated Container Observations.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{
    ContainerId, DescribeContractRequest, GET_CONTAINER_OBSERVATIONS_CAPABILITY,
    GetContainerObservationsRequest, MachineId, MachineTarget, RpcError, RpcErrorCode, op,
    service_containers, serving_replicas,
};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::Client;
use crate::connect::TARGET_RPC_TIMEOUT;

const BARRIER_TIMEOUT: Duration = Duration::from_secs(30);
const RPC_HOLD: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContainerObservationCondition {
    Serving,
    Dropped,
}

impl Client {
    pub(crate) async fn wait_for_container_observations(
        &self,
        container_ids: &[ContainerId],
        condition: ContainerObservationCondition,
        cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        if container_ids.is_empty() {
            return Ok(());
        }
        let deadline = Instant::now() + BARRIER_TIMEOUT;
        let mut client = self.clone();
        let machines = client.machines().await.map_err(RpcError::from)?;
        let probes = machines
            .into_iter()
            .filter(|machine| machine.membership.invites_rpc())
            .map(|machine| {
                let client = self.clone();
                async move {
                    let machine_id = machine.machine.id;
                    let result = client
                        .invoke::<op::DescribeContract>(
                            DescribeContractRequest {},
                            &MachineTarget::from(&machine_id),
                            Some(TARGET_RPC_TIMEOUT),
                        )
                        .await;
                    (machine_id, result)
                }
            });
        let probed = tokio::select! {
            () = cancellation.cancelled() => return Err(cancelled()),
            probed = join_all(probes) => probed,
        };
        let mut capable = probed
            .into_iter()
            .filter_map(|(machine_id, description)| {
                description
                    .ok()
                    .filter(|description| {
                        description.supports(GET_CONTAINER_OBSERVATIONS_CAPABILITY)
                    })
                    .map(|_| machine_id)
            })
            .collect::<Vec<_>>();
        let mut pending = container_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut wait = Duration::ZERO;

        while !pending.is_empty() && !capable.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                return Err(timed_out(&pending));
            }
            let timeout = std::cmp::min(deadline - now, TARGET_RPC_TIMEOUT);
            let requested = pending.iter().copied().collect::<Vec<_>>();
            let calls = capable.iter().copied().map(|machine_id| {
                let client = self.clone();
                let container_ids = requested.clone();
                async move {
                    let response = client
                        .invoke::<op::GetContainerObservations>(
                            GetContainerObservationsRequest {
                                container_ids,
                                wait_millis: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
                            },
                            &MachineTarget::from(&machine_id),
                            Some(timeout),
                        )
                        .await;
                    (machine_id, response)
                }
            });
            let responses = tokio::select! {
                () = cancellation.cancelled() => return Err(cancelled()),
                responses = join_all(calls) => responses,
            };
            let requested = requested.into_iter().collect::<BTreeSet<_>>();
            let mut skipped = BTreeSet::new();
            let mut serving_by_machine = BTreeMap::new();
            for (machine_id, response) in responses {
                match response {
                    Ok(response) => {
                        let actual = response.containers.keys().copied().collect::<BTreeSet<_>>();
                        if actual != requested {
                            return Err(invalid_response(machine_id));
                        }
                        let containers = service_containers(
                            response.containers.values().filter_map(Clone::clone),
                        );
                        serving_by_machine.insert(
                            machine_id,
                            serving_replicas(&containers)
                                .into_iter()
                                .map(|container| container.as_observation().container_id)
                                .collect::<BTreeSet<_>>(),
                        );
                    }
                    Err(error)
                        if matches!(
                            error.code,
                            RpcErrorCode::Unsupported
                                | RpcErrorCode::Unavailable
                                | RpcErrorCode::NotFound
                        ) =>
                    {
                        skipped.insert(machine_id);
                    }
                    Err(mut error) => {
                        error.message = format!("Machine {machine_id}: {}", error.message);
                        return Err(error);
                    }
                }
            }
            capable.retain(|machine_id| !skipped.contains(machine_id));
            pending.retain(|container_id| {
                serving_by_machine.values().any(|serving| match condition {
                    ContainerObservationCondition::Serving => !serving.contains(container_id),
                    ContainerObservationCondition::Dropped => serving.contains(container_id),
                })
            });
            wait = std::cmp::min(RPC_HOLD, deadline.saturating_duration_since(Instant::now()));
        }
        Ok(())
    }
}

fn cancelled() -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: "container observation wait cancelled".into(),
        details: Value::Null,
    }
}

fn timed_out(pending: &BTreeSet<ContainerId>) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: format!(
            "timed out waiting for replicated Container Observations: {}",
            pending
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        details: Value::Null,
    }
}

fn invalid_response(machine_id: MachineId) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: format!(
            "Machine {machine_id} returned an incomplete replicated Container Observation map"
        ),
        details: Value::Null,
    }
}
