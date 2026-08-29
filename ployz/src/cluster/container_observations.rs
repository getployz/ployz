//! Client-owned barrier over replicated Container Observations.

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{
    ContainerId, DescribeContractRequest, GET_CONTAINER_OBSERVATIONS_CAPABILITY,
    GetContainerObservationsRequest, MachineId, MachineTarget, RpcError, RpcErrorCode, op,
    service_containers,
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
        let mut pending = container_ids.iter().copied().collect::<BTreeSet<_>>();
        let mut client = self.clone();
        let machines = within_deadline(deadline, cancellation, &pending, async {
            client.machines().await.map_err(RpcError::from)
        })
        .await?;
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
        let probed = within_deadline(deadline, cancellation, &pending, async {
            Ok(join_all(probes).await)
        })
        .await?;
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
            let responses = within_deadline(deadline, cancellation, &pending, async {
                Ok(join_all(calls).await)
            })
            .await?;
            let requested = requested.into_iter().collect::<BTreeSet<_>>();
            let mut round = Vec::new();
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
                        round.push((
                            machine_id,
                            Ok(containers
                                .iter()
                                .filter(|container| container.traffic_address().is_some())
                                .map(|container| container.as_observation().container_id)
                                .collect::<BTreeSet<_>>()),
                        ));
                    }
                    Err(error) => round.push((machine_id, Err(error))),
                }
            }
            let ok_votes = apply_round(&mut capable, &mut pending, condition, round)?;
            wait = std::cmp::min(RPC_HOLD, deadline.saturating_duration_since(Instant::now()));
            if !pending.is_empty() && !capable.is_empty() && ok_votes == 0 && !wait.is_zero() {
                tokio::select! {
                    () = cancellation.cancelled() => return Err(cancelled()),
                    () = tokio::time::sleep(wait) => {}
                }
            }
        }
        Ok(())
    }
}

fn apply_round(
    capable: &mut Vec<MachineId>,
    pending: &mut BTreeSet<ContainerId>,
    condition: ContainerObservationCondition,
    responses: Vec<(MachineId, Result<BTreeSet<ContainerId>, RpcError>)>,
) -> Result<usize, RpcError> {
    let mut dropped = BTreeSet::new();
    let mut serving_by_machine = BTreeMap::new();
    let mut ok_votes = 0;
    for (machine_id, response) in responses {
        match response {
            Ok(serving) => {
                ok_votes += 1;
                serving_by_machine.insert(machine_id, serving);
            }
            Err(error) if error.code == RpcErrorCode::Unsupported => {
                dropped.insert(machine_id);
            }
            Err(error)
                if matches!(
                    error.code,
                    RpcErrorCode::Unavailable | RpcErrorCode::NotFound
                ) => {}
            Err(mut error) => {
                error.message = format!("Machine {machine_id}: {}", error.message);
                return Err(error);
            }
        }
    }
    capable.retain(|machine_id| !dropped.contains(machine_id));
    pending.retain(|container_id| {
        capable
            .iter()
            .any(|machine_id| match serving_by_machine.get(machine_id) {
                None => true,
                Some(serving) => match condition {
                    ContainerObservationCondition::Serving => !serving.contains(container_id),
                    ContainerObservationCondition::Dropped => serving.contains(container_id),
                },
            })
    });
    Ok(ok_votes)
}

async fn within_deadline<T>(
    deadline: Instant,
    cancellation: &CancellationToken,
    pending: &BTreeSet<ContainerId>,
    work: impl Future<Output = Result<T, RpcError>>,
) -> Result<T, RpcError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(cancelled()),
        () = tokio::time::sleep_until(deadline) => Err(timed_out(pending)),
        result = work => result,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(hex: char) -> MachineId {
        MachineId::parse(hex.to_string().repeat(32)).unwrap()
    }

    fn container(hex: char) -> ContainerId {
        ContainerId::parse(hex.to_string().repeat(64)).unwrap()
    }

    fn error(code: RpcErrorCode) -> RpcError {
        RpcError {
            code,
            message: "transient".into(),
            details: Value::Null,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn hung_machine_list_returns_timed_out_at_the_barrier_deadline() {
        let pending = BTreeSet::from([container('a')]);
        let cancellation = CancellationToken::new();
        let deadline = Instant::now() + BARRIER_TIMEOUT;
        let wait = within_deadline(
            deadline,
            &cancellation,
            &pending,
            std::future::pending::<Result<(), RpcError>>(),
        );
        tokio::pin!(wait);
        tokio::select! {
            result = &mut wait => {
                let error = result.expect_err("hung work must time out");
                assert_eq!(error.code, RpcErrorCode::Unavailable);
                assert!(
                    error
                        .message
                        .contains("timed out waiting for replicated Container Observations"),
                    "{}",
                    error.message
                );
            }
            () = tokio::time::sleep(BARRIER_TIMEOUT + Duration::from_millis(1)) => {
                panic!("observation barrier outlived its deadline");
            }
        }
    }

    #[tokio::test]
    async fn cancelled_machine_list_returns_before_the_barrier_deadline() {
        let pending = BTreeSet::from([container('a')]);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = within_deadline(
            Instant::now() + BARRIER_TIMEOUT,
            &cancellation,
            &pending,
            std::future::pending::<Result<(), RpcError>>(),
        )
        .await
        .expect_err("cancelled work must return");
        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(error.message, "container observation wait cancelled");
    }

    #[test]
    fn unavailable_and_not_found_keep_a_capable_machine_and_the_pending_container() {
        for code in [RpcErrorCode::Unavailable, RpcErrorCode::NotFound] {
            let machine = machine('1');
            let container = container('a');
            let mut capable = vec![machine];
            let mut pending = BTreeSet::from([container]);

            apply_round(
                &mut capable,
                &mut pending,
                ContainerObservationCondition::Serving,
                vec![(machine, Err(error(code.clone())))],
            )
            .unwrap();

            assert_eq!(capable, vec![machine], "{code:?}");
            assert_eq!(pending, BTreeSet::from([container]), "{code:?}");
        }
    }

    #[test]
    fn serving_on_one_machine_does_not_clear_pending_while_another_is_unavailable() {
        let first = machine('1');
        let second = machine('2');
        let container = container('a');
        let mut capable = vec![first, second];
        let mut pending = BTreeSet::from([container]);

        apply_round(
            &mut capable,
            &mut pending,
            ContainerObservationCondition::Serving,
            vec![
                (first, Ok(BTreeSet::from([container]))),
                (second, Err(error(RpcErrorCode::Unavailable))),
            ],
        )
        .unwrap();

        assert_eq!(capable, vec![first, second]);
        assert_eq!(pending, BTreeSet::from([container]));
    }

    #[test]
    fn all_capable_serving_votes_clear_pending() {
        let machine = machine('1');
        let container = container('a');
        let mut capable = vec![machine];
        let mut pending = BTreeSet::from([container]);

        apply_round(
            &mut capable,
            &mut pending,
            ContainerObservationCondition::Serving,
            vec![(machine, Ok(BTreeSet::from([container])))],
        )
        .unwrap();

        assert_eq!(capable, vec![machine]);
        assert!(pending.is_empty());
    }

    #[test]
    fn unsupported_drops_the_machine_from_capable() {
        let machine = machine('1');
        let container = container('a');
        let mut capable = vec![machine];
        let mut pending = BTreeSet::from([container]);

        apply_round(
            &mut capable,
            &mut pending,
            ContainerObservationCondition::Serving,
            vec![(machine, Err(error(RpcErrorCode::Unsupported)))],
        )
        .unwrap();

        assert!(capable.is_empty());
        assert!(pending.is_empty());
    }
}
