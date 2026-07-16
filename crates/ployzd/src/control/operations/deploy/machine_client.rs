//! Deploy-policy mapping for typed Machine role RPC outcomes.

use ployz_core::ids::MachineId;
use ployz_core::image::{
    ImageEnsureOk, ImageEnsureRequest, ImageRemoveOk, ImageRemoveRequest, OciDigest,
};
use ployz_core::intent::VolumePinState;

use crate::control::role_client::machine::{
    MachineCallError, MachineImageEnsureError, MachineImageRemoveError, MachineImageResolveError,
    NatsMachineContainerRuntime,
};
use crate::roles::machine::MachineRuntimeUnavailableReason;
use crate::roles::machine::protocol::{
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerResolveImageDomainError, MachineContainerResolveImageRpcRequest,
    MachineContainerRestartDomainError, MachineContainerRestartRpcRequest,
    MachineContainerRunDomainError, MachineContainerRunHookDomainError,
    MachineContainerRunHookRpcOk, MachineContainerRunHookRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopDomainError, MachineContainerStopRpcRequest, MachineRunContainerOutcome,
};

use super::{
    MachineContainerRuntime, MachineContainerRuntimeError, MachineImageRemovalRuntime,
    MachineVolumeEnsureError, PreStartHookRuntimeError,
};

impl MachineContainerRuntime for NatsMachineContainerRuntime {
    async fn ensure_volume(
        &mut self,
        machine_id: &MachineId,
        volume: &VolumePinState,
    ) -> Result<(), MachineVolumeEnsureError> {
        NatsMachineContainerRuntime::ensure_volume(self, machine_id, volume).await
    }

    async fn resolve_image(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerResolveImageRpcRequest,
    ) -> Result<OciDigest, MachineImageResolveError> {
        self.request_resolve_image(machine_id, &request)
            .await
            .map(|ok| ok.digest)
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => MachineImageResolveError::Unavailable {
                    machine_id: machine_id.clone(),
                    reason,
                },
                MachineCallError::Domain(
                    MachineContainerResolveImageDomainError::ResolveFailed { message },
                ) => MachineImageResolveError::Rejected {
                    machine_id: machine_id.clone(),
                    message,
                },
            })
    }

    async fn ensure_image(
        &mut self,
        machine_id: &MachineId,
        request: ImageEnsureRequest,
    ) -> Result<ImageEnsureOk, MachineImageEnsureError> {
        self.request_ensure_image(machine_id, &request)
            .await
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => MachineImageEnsureError::Unavailable {
                    machine_id: machine_id.clone(),
                    reason,
                },
                MachineCallError::Domain(error) => MachineImageEnsureError::Domain {
                    machine_id: machine_id.clone(),
                    error,
                },
            })
    }

    async fn run_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunRpcRequest,
    ) -> Result<MachineRunContainerOutcome, MachineContainerRuntimeError> {
        self.request_container_run(machine_id, &request)
            .await
            .map(|ok| ok.outcome)
            .map_err(|error| map_run_error(machine_id, error))
    }

    async fn run_pre_start_hook(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunHookRpcRequest,
    ) -> Result<MachineContainerRunHookRpcOk, PreStartHookRuntimeError> {
        self.request_container_run_hook(machine_id, &request)
            .await
            .map_err(|error| map_hook_error(machine_id, error))
    }

    async fn remove_pre_start_hook(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> Result<(), PreStartHookRuntimeError> {
        self.request_container_remove(machine_id, &request)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => PreStartHookRuntimeError::Unavailable {
                    machine_id: machine_id.clone(),
                    reason,
                },
                MachineCallError::Domain(MachineContainerRemoveDomainError::RemoveFailed {
                    container_id,
                    message,
                    inspect_hint,
                }) => PreStartHookRuntimeError::CleanupFailed {
                    machine_id: machine_id.clone(),
                    container_id,
                    message,
                    inspect_hint,
                },
            })
    }

    async fn remove_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        self.request_container_remove(machine_id, &request)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => unavailable(machine_id, reason),
                MachineCallError::Domain(MachineContainerRemoveDomainError::RemoveFailed {
                    container_id,
                    message,
                    inspect_hint,
                }) => MachineContainerRuntimeError::RemoveContainerFailed {
                    machine_id: machine_id.clone(),
                    container_id,
                    message,
                    inspect_hint,
                },
            })
    }

    async fn restart_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRestartRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        self.request_container_restart(machine_id, &request)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => unavailable(machine_id, reason),
                MachineCallError::Domain(MachineContainerRestartDomainError::RestartFailed {
                    container_id,
                    message,
                    inspect_hint,
                }) => MachineContainerRuntimeError::RestartContainerFailed {
                    machine_id: machine_id.clone(),
                    container_id,
                    message,
                    inspect_hint,
                },
            })
    }

    async fn stop_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerStopRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        self.request_container_stop(machine_id, &request)
            .await
            .map(|_| ())
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => unavailable(machine_id, reason),
                MachineCallError::Domain(MachineContainerStopDomainError::StopFailed {
                    container_id,
                    message,
                    inspect_hint,
                }) => MachineContainerRuntimeError::StopContainerFailed {
                    machine_id: machine_id.clone(),
                    container_id,
                    message,
                    inspect_hint,
                },
            })
    }
}

fn map_run_error(
    machine_id: &MachineId,
    error: MachineCallError<MachineContainerRunDomainError>,
) -> MachineContainerRuntimeError {
    match error {
        MachineCallError::Unavailable(reason) => unavailable(machine_id, reason),
        MachineCallError::Domain(error) => match error {
            MachineContainerRunDomainError::ImagePullFailed {
                service_id,
                namespace_revision_entry_id,
                message,
            } => MachineContainerRuntimeError::ImagePullFailed {
                machine_id: machine_id.clone(),
                service_id,
                namespace_revision_entry_id,
                message,
            },
            MachineContainerRunDomainError::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            } => MachineContainerRuntimeError::OperationStepAmbiguous {
                machine_id: machine_id.clone(),
                operation_id,
                step_id,
                container_ids,
            },
            MachineContainerRunDomainError::CreatedContainerStartFailed {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::CreatedContainerStartFailed {
                machine_id: machine_id.clone(),
                container_id,
                message,
                inspect_hint,
            },
            MachineContainerRunDomainError::ExistingContainerStartFailed {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::ExistingContainerStartFailed {
                machine_id: machine_id.clone(),
                container_id,
                message,
                inspect_hint,
            },
            MachineContainerRunDomainError::OperationStepContainerNotStartable {
                container_id,
                message,
                inspect_hint,
            } => MachineContainerRuntimeError::OperationStepContainerNotStartable {
                machine_id: machine_id.clone(),
                container_id,
                message,
                inspect_hint,
            },
        },
    }
}

fn map_hook_error(
    machine_id: &MachineId,
    error: MachineCallError<MachineContainerRunHookDomainError>,
) -> PreStartHookRuntimeError {
    match error {
        MachineCallError::Unavailable(reason) => PreStartHookRuntimeError::Unavailable {
            machine_id: machine_id.clone(),
            reason,
        },
        MachineCallError::Domain(error) => match error {
            MachineContainerRunHookDomainError::OperationStepAmbiguous {
                operation_id,
                step_id,
                container_ids,
            } => PreStartHookRuntimeError::OperationStepAmbiguous {
                machine_id: machine_id.clone(),
                operation_id,
                step_id,
                container_ids,
            },
            MachineContainerRunHookDomainError::CreateFailed { message } => {
                PreStartHookRuntimeError::CreateFailed {
                    machine_id: machine_id.clone(),
                    message,
                }
            }
            MachineContainerRunHookDomainError::StartFailed {
                container_id,
                message,
                inspect_hint,
            } => PreStartHookRuntimeError::StartFailed {
                machine_id: machine_id.clone(),
                container_id,
                message,
                inspect_hint,
            },
            MachineContainerRunHookDomainError::WaitFailed {
                container_id,
                message,
                log_hint,
            } => PreStartHookRuntimeError::WaitFailed {
                machine_id: machine_id.clone(),
                container_id,
                message,
                log_hint,
            },
            MachineContainerRunHookDomainError::TimedOut {
                container_id,
                timeout_millis,
                message,
                inspect_hint,
            } => PreStartHookRuntimeError::TimedOut {
                machine_id: machine_id.clone(),
                container_id,
                timeout_millis,
                message,
                inspect_hint,
            },
        },
    }
}

impl MachineImageRemovalRuntime for NatsMachineContainerRuntime {
    async fn remove_image(
        &mut self,
        machine_id: &MachineId,
        request: ImageRemoveRequest,
    ) -> Result<ImageRemoveOk, MachineImageRemoveError> {
        self.request_remove_image(machine_id, &request)
            .await
            .map_err(|error| match error {
                MachineCallError::Unavailable(reason) => MachineImageRemoveError::Unavailable {
                    machine_id: machine_id.clone(),
                    reason,
                },
                MachineCallError::Domain(error) => MachineImageRemoveError::Domain {
                    machine_id: machine_id.clone(),
                    error,
                },
            })
    }
}

fn unavailable(
    machine_id: &MachineId,
    reason: MachineRuntimeUnavailableReason,
) -> MachineContainerRuntimeError {
    MachineContainerRuntimeError::Unavailable {
        machine_id: machine_id.clone(),
        reason,
    }
}
