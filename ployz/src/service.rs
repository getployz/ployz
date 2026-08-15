use std::{collections::HashMap, time::Duration};

use ployz_core::{
    ContainerAction, ContainerCreated, ContainerId, ContainerKind, CreateContainerRequest,
    CreateVolumeRequest, InspectContainerRequest, ListContainersRequest, ListMachinesRequest,
    LiveServices, MachineFailure, MachineId, MachineObservation, MachineSelector, MachineSuccess,
    MembershipObservation, PartialResult, RemoveContainerRequest, ResolvedServiceSpec, RpcError,
    RpcErrorCode, ServiceSelectorError, StartContainerRequest, StopContainerRequest,
    derive_live_services, op, select_service,
};
use serde_json::Value;
use thiserror::Error;
use tokio::task::JoinSet;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};

#[derive(Debug, Error)]
pub enum ServiceClientError {
    #[error(transparent)]
    Connect(#[from] ConnectError),
    #[error(transparent)]
    Selector(#[from] ServiceSelectorError),
}

pub struct LifecycleResult {
    pub observations: PartialResult<Vec<ployz_core::ContainerObservation>, RpcError>,
    pub outcomes: PartialResult<ContainerId, ContainerOperationFailure>,
}

#[derive(Clone, Debug)]
pub struct ContainerOperationFailure {
    pub container_id: ContainerId,
    pub error: RpcError,
}

pub(crate) async fn create_volume_on_machine(
    client: &Client,
    machine_id: &MachineId,
    request: CreateVolumeRequest,
) -> Result<(), RpcError> {
    client
        .invoke::<op::CreateVolume>(
            request,
            &MachineSelector::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
}

impl Client {
    pub async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError> {
        let machines = self
            .call::<op::ListMachines>(ListMachinesRequest {}, None)
            .await?;
        self.live_services_from(&machines.machines).await
    }

    pub(crate) async fn live_services_from(
        &self,
        machines: &[MachineObservation],
    ) -> Result<LiveServices<RpcError>, ConnectError> {
        let mut tasks = JoinSet::new();
        let mut omissions = Vec::new();
        for machine in machines {
            // TODO(UT-102): the entry Machine's observer-relative Membership Observation is the
            // current trust boundary; it can be stale and is not an authority or freshness proof.
            match machine.membership {
                MembershipObservation::Up | MembershipObservation::Suspect => {
                    tasks.spawn(list_on_machine(self.clone(), machine.machine.id.clone()));
                }
                MembershipObservation::Down
                | MembershipObservation::Unknown
                | MembershipObservation::Unrecognized(_) => {
                    omissions.push(machine.machine.id.clone());
                }
            }
        }
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions,
        };
        // TODO(UT-015, UT-017): retain target failures and omissions beside successful container
        // observations; an entry-relative answer remains partial evidence.
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(Ok(success)) => result.successes.push(success),
                Ok(Err(failure)) => result.failures.push(failure),
                Err(error) => return Err(ConnectError::Attempt(error.to_string())),
            }
        }
        Ok(derive_live_services(result))
    }

    pub async fn inspect_container(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
    ) -> Result<ployz_core::ContainerObservation, RpcError> {
        self.invoke::<op::InspectContainer>(
            InspectContainerRequest { container_id },
            &MachineSelector::from(&machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|details| details.container)
    }

    pub async fn create_container(
        &self,
        machine_id: MachineId,
        kind: ContainerKind,
        resolved_spec: ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        self.invoke::<op::CreateContainer>(
            CreateContainerRequest {
                kind,
                resolved_spec,
            },
            &MachineSelector::from(&machine_id),
            None,
        )
        .await
    }

    pub async fn change_container(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        change_container_rpc(
            self,
            &machine_id,
            &container_id,
            action,
            signal,
            grace_period_seconds,
        )
        .await
    }

    pub(crate) async fn remove_container(
        &self,
        machine_id: MachineId,
        container_id: ContainerId,
    ) -> Result<(), RpcError> {
        remove_container_rpc(self, &machine_id, &container_id).await
    }

    pub async fn change_service(
        &mut self,
        selector: &str,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> Result<LifecycleResult, ServiceClientError> {
        let live = self.live_services().await?;
        let outcomes = self
            .change_observed_service(
                select_service(&live.services, selector)?,
                action,
                signal,
                grace_period_seconds,
            )
            .await;
        Ok(LifecycleResult {
            observations: live.containers,
            outcomes,
        })
    }

    pub async fn change_observed_service(
        &self,
        service: &ployz_core::ServiceObservation,
        action: ContainerAction,
        signal: Option<String>,
        grace_period_seconds: Option<i32>,
    ) -> PartialResult<ContainerId, ContainerOperationFailure> {
        let mut tasks = JoinSet::new();
        let mut task_targets = HashMap::new();
        for container in service.containers_for(action) {
            let machine_id = container.machine_id.clone();
            let container_id = container.container_id.clone();
            let handle = tasks.spawn(change_on_machine(
                self.clone(),
                machine_id.clone(),
                container_id.clone(),
                action,
                signal.clone(),
                grace_period_seconds,
            ));
            task_targets.insert(handle.id(), (machine_id, container_id));
        }
        let mut outcomes = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        while let Some(joined) = tasks.join_next_with_id().await {
            match joined {
                Ok((id, Ok(success))) => {
                    task_targets.remove(&id);
                    outcomes.successes.push(success);
                }
                Ok((id, Err(failure))) => {
                    task_targets.remove(&id);
                    outcomes.failures.push(failure);
                }
                Err(error) => {
                    if let Some((machine_id, container_id)) = task_targets.remove(&error.id()) {
                        outcomes.failures.push(MachineFailure {
                            machine_id,
                            error: ContainerOperationFailure {
                                container_id,
                                error: RpcError {
                                    code: RpcErrorCode::Unavailable,
                                    message: error.to_string(),
                                    details: Value::Null,
                                },
                            },
                        });
                    }
                }
            }
        }
        outcomes
    }
}

async fn list_on_machine(
    client: Client,
    machine_id: MachineId,
) -> Result<MachineSuccess<Vec<ployz_core::ContainerObservation>>, MachineFailure<RpcError>> {
    client
        .invoke::<op::ListContainers>(
            ListContainersRequest {},
            &MachineSelector::from(&machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|list| MachineSuccess {
            machine_id: machine_id.clone(),
            value: list.containers,
        })
        .map_err(|error| MachineFailure { machine_id, error })
}

async fn change_on_machine(
    client: Client,
    machine_id: MachineId,
    container_id: ContainerId,
    action: ContainerAction,
    signal: Option<String>,
    grace_period_seconds: Option<i32>,
) -> Result<MachineSuccess<ContainerId>, MachineFailure<ContainerOperationFailure>> {
    match change_container_rpc(
        &client,
        &machine_id,
        &container_id,
        action,
        signal,
        grace_period_seconds,
    )
    .await
    {
        Ok(()) => Ok(MachineSuccess {
            machine_id,
            value: container_id,
        }),
        Err(error) => Err(MachineFailure {
            machine_id,
            error: ContainerOperationFailure {
                container_id,
                error,
            },
        }),
    }
}

async fn change_container_rpc(
    client: &Client,
    machine_id: &MachineId,
    container_id: &ContainerId,
    action: ContainerAction,
    signal: Option<String>,
    grace_period_seconds: Option<i32>,
) -> Result<(), RpcError> {
    let target = MachineSelector::from(machine_id);
    if matches!(action, ContainerAction::Stop | ContainerAction::Remove) {
        accept_stop_result(
            action,
            client
                .invoke::<op::StopContainer>(
                    StopContainerRequest {
                        container_id: container_id.clone(),
                        signal,
                        grace_period_seconds,
                    },
                    &target,
                    stop_rpc_timeout(grace_period_seconds),
                )
                .await
                .map(|_| ()),
        )?;
    }
    match action {
        ContainerAction::Start => client
            .invoke::<op::StartContainer>(
                StartContainerRequest {
                    container_id: container_id.clone(),
                },
                &target,
                Some(TARGET_RPC_TIMEOUT),
            )
            .await
            .map(|_| ()),
        ContainerAction::Stop => Ok(()),
        ContainerAction::Remove => remove_container_rpc(client, machine_id, container_id).await,
    }
}

async fn remove_container_rpc(
    client: &Client,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<(), RpcError> {
    client
        .invoke::<op::RemoveContainer>(
            RemoveContainerRequest {
                container_id: container_id.clone(),
                remove_volumes: true,
                force: false,
            },
            &MachineSelector::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
}

fn stop_rpc_timeout(grace_period_seconds: Option<i32>) -> Option<Duration> {
    match grace_period_seconds {
        Some(seconds) if seconds < 0 => None,
        Some(seconds) => Some(TARGET_RPC_TIMEOUT + Duration::from_secs(seconds as u64)),
        None => None,
    }
}

fn accept_stop_result(
    action: ContainerAction,
    result: Result<(), RpcError>,
) -> Result<(), RpcError> {
    match result {
        Err(error) if action == ContainerAction::Remove && error.code == RpcErrorCode::NotFound => {
            Ok(())
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unspecified_or_negative_stop_timeout_has_no_rpc_deadline() {
        assert_eq!(stop_rpc_timeout(Some(-1)), None);
        assert_eq!(stop_rpc_timeout(None), None);
        assert_eq!(
            stop_rpc_timeout(Some(5)),
            Some(TARGET_RPC_TIMEOUT + Duration::from_secs(5))
        );
    }

    #[test]
    fn remove_tolerates_a_missing_preliminary_stop_target() {
        let missing = RpcError {
            code: RpcErrorCode::NotFound,
            message: "gone".into(),
            details: Value::Null,
        };

        assert!(accept_stop_result(ContainerAction::Remove, Err(missing.clone())).is_ok());
        assert_eq!(
            accept_stop_result(ContainerAction::Stop, Err(missing.clone()))
                .unwrap_err()
                .code,
            RpcErrorCode::NotFound
        );
        assert_eq!(
            accept_stop_result(
                ContainerAction::Remove,
                Err(RpcError {
                    code: RpcErrorCode::Internal,
                    ..missing
                })
            )
            .unwrap_err()
            .code,
            RpcErrorCode::Internal
        );
    }
}
