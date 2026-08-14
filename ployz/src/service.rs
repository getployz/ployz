use std::{collections::HashMap, future::Future, time::Duration};

use ployz_core::{
    ContainerAction, ContainerCreated, ContainerId, ContainerKind, CreateContainerRequest,
    InspectContainerRequest, LiveServices, MachineFailure, MachineId, MachineObservation,
    MachineRpcClient, MachineSuccess, MembershipObservation, OpaquePayload, PartialResult,
    RemoveContainerRequest, ResolvedServiceSpec, RpcError, RpcErrorCode, RpcRequest, RpcResponse,
    RpcResponseBody, ServiceSelectorError, StartContainerRequest, StopContainerRequest,
    derive_live_services, select_service,
};
use serde_json::Value;
use thiserror::Error;
use tokio::task::JoinSet;
use tonic::{Request, Response, transport::Channel};

use crate::connect::{Client, ConnectError};

const TARGET_RPC_TIMEOUT: Duration = Duration::from_secs(10);

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

impl Client {
    pub async fn live_services(&mut self) -> Result<LiveServices<RpcError>, ConnectError> {
        let machines = entry_machines(&mut self.rpc).await?;
        let mut tasks = JoinSet::new();
        let mut omissions = Vec::new();
        for machine in machines {
            // TODO(UT-102): the entry Machine's observer-relative Membership Observation is the
            // current trust boundary; it can be stale and is not an authority or freshness proof.
            match machine.membership {
                MembershipObservation::Up | MembershipObservation::Suspect => {
                    tasks.spawn(list_on_machine(self.rpc.clone(), machine.machine.id));
                }
                MembershipObservation::Down
                | MembershipObservation::Unknown
                | MembershipObservation::Unrecognized(_) => omissions.push(machine.machine.id),
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
        let mut rpc = self.rpc.clone();
        let request = routed_request(
            &machine_id,
            RpcRequest::inspect_container(InspectContainerRequest { container_id }),
        )?;
        target_response(timed_rpc(rpc.inspect_container(request)).await?)?
            .decode_container_details()
            .cloned()
            .map_err(codec_error)
    }

    pub async fn create_container(
        &self,
        machine_id: MachineId,
        kind: ContainerKind,
        resolved_spec: ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        let mut rpc = self.rpc.clone();
        let request = routed_request(
            &machine_id,
            RpcRequest::create_container(CreateContainerRequest {
                kind,
                resolved_spec,
            }),
        )?;
        target_response(timed_rpc(rpc.create_container(request)).await?)?
            .decode_container_created()
            .cloned()
            .map_err(codec_error)
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
            &mut self.rpc.clone(),
            &machine_id,
            &container_id,
            action,
            signal,
            grace_period_seconds,
        )
        .await
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
                self.rpc.clone(),
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

async fn entry_machines(
    rpc: &mut MachineRpcClient<Channel>,
) -> Result<Vec<MachineObservation>, ConnectError> {
    let response = rpc
        .list_machines(RpcRequest::list_machines().encode()?)
        .await?
        .into_inner()
        .decode_response()?;
    if let RpcResponseBody::Error(error) = &response.body {
        return Err(ConnectError::Remote(error.clone()));
    }
    Ok(response.decode_machine_list()?.to_vec())
}

async fn list_on_machine(
    mut rpc: MachineRpcClient<Channel>,
    machine_id: MachineId,
) -> Result<MachineSuccess<Vec<ployz_core::ContainerObservation>>, MachineFailure<RpcError>> {
    let request = routed_request(&machine_id, RpcRequest::list_containers()).map_err(|error| {
        MachineFailure {
            machine_id: machine_id.clone(),
            error,
        }
    })?;
    let result = async {
        let response = target_response(timed_rpc(rpc.list_containers(request)).await?)?;
        response
            .decode_container_list()
            .map(<[_]>::to_vec)
            .map_err(codec_error)
    }
    .await;
    result
        .map(|value| MachineSuccess {
            machine_id: machine_id.clone(),
            value,
        })
        .map_err(|error| MachineFailure { machine_id, error })
}

async fn change_on_machine(
    mut rpc: MachineRpcClient<Channel>,
    machine_id: MachineId,
    container_id: ContainerId,
    action: ContainerAction,
    signal: Option<String>,
    grace_period_seconds: Option<i32>,
) -> Result<MachineSuccess<ContainerId>, MachineFailure<ContainerOperationFailure>> {
    let result = change_container_rpc(
        &mut rpc,
        &machine_id,
        &container_id,
        action,
        signal,
        grace_period_seconds,
    )
    .await;
    match result {
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
    rpc: &mut MachineRpcClient<Channel>,
    machine_id: &MachineId,
    container_id: &ContainerId,
    action: ContainerAction,
    signal: Option<String>,
    grace_period_seconds: Option<i32>,
) -> Result<(), RpcError> {
    if matches!(action, ContainerAction::Stop | ContainerAction::Remove) {
        let request = routed_request(
            machine_id,
            RpcRequest::stop_container(StopContainerRequest {
                container_id: container_id.clone(),
                signal,
                grace_period_seconds,
            }),
        )?;
        let response = rpc_with_timeout(
            stop_rpc_timeout(grace_period_seconds),
            rpc.stop_container(request),
        )
        .await?;
        expect_changed(target_response(response)?)?;
    }
    let response = match action {
        ContainerAction::Start => {
            let request = routed_request(
                machine_id,
                RpcRequest::start_container(StartContainerRequest {
                    container_id: container_id.clone(),
                }),
            )?;
            timed_rpc(rpc.start_container(request)).await?
        }
        ContainerAction::Stop => return Ok(()),
        ContainerAction::Remove => {
            let request = routed_request(
                machine_id,
                RpcRequest::remove_container(RemoveContainerRequest {
                    container_id: container_id.clone(),
                    remove_volumes: true,
                    force: false,
                }),
            )?;
            timed_rpc(rpc.remove_container(request)).await?
        }
    };
    expect_changed(target_response(response)?)
}

fn routed_request(
    machine_id: &MachineId,
    request: RpcRequest,
) -> Result<Request<OpaquePayload>, RpcError> {
    let mut request = Request::new(request.encode().map_err(codec_error)?);
    request.metadata_mut().insert(
        "machine",
        machine_id.as_str().parse().map_err(|error| RpcError {
            code: RpcErrorCode::InvalidArgument,
            message: format!("invalid Machine routing metadata: {error}"),
            details: Value::Null,
        })?,
    );
    Ok(request)
}

fn target_response(payload: OpaquePayload) -> Result<RpcResponse, RpcError> {
    let response = payload.decode_response().map_err(codec_error)?;
    match response {
        RpcResponse {
            body: RpcResponseBody::Error(error),
            ..
        } => Err(error),
        response => Ok(response),
    }
}

fn expect_changed(response: RpcResponse) -> Result<(), RpcError> {
    response
        .decode_container_changed()
        .map(|_| ())
        .map_err(codec_error)
}

fn codec_error(error: ployz_core::CodecError) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: error.to_string(),
        details: Value::Null,
    }
}

async fn timed_rpc(
    future: impl Future<Output = Result<Response<OpaquePayload>, tonic::Status>>,
) -> Result<OpaquePayload, RpcError> {
    rpc_with_timeout(Some(TARGET_RPC_TIMEOUT), future).await
}

async fn rpc_with_timeout(
    timeout: Option<Duration>,
    future: impl Future<Output = Result<Response<OpaquePayload>, tonic::Status>>,
) -> Result<OpaquePayload, RpcError> {
    let response = match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .map_err(|_| RpcError {
                code: RpcErrorCode::Unavailable,
                message: "target Machine RPC timed out".into(),
                details: Value::Null,
            })?,
        None => future.await,
    };
    response
        .map(Response::into_inner)
        .map_err(|error| RpcError {
            code: RpcErrorCode::Unavailable,
            message: error.to_string(),
            details: Value::Null,
        })
}

fn stop_rpc_timeout(grace_period_seconds: Option<i32>) -> Option<Duration> {
    match grace_period_seconds {
        Some(seconds) if seconds < 0 => None,
        Some(seconds) => Some(TARGET_RPC_TIMEOUT + Duration::from_secs(seconds as u64)),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn target_timeout_becomes_a_typed_partial_failure() {
        let error = rpc_with_timeout(
            Some(Duration::from_millis(1)),
            std::future::pending::<Result<Response<OpaquePayload>, tonic::Status>>(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::Unavailable);
    }

    #[test]
    fn unspecified_or_negative_stop_timeout_has_no_rpc_deadline() {
        assert_eq!(stop_rpc_timeout(Some(-1)), None);
        assert_eq!(stop_rpc_timeout(None), None);
        assert_eq!(
            stop_rpc_timeout(Some(5)),
            Some(TARGET_RPC_TIMEOUT + Duration::from_secs(5))
        );
    }
}
