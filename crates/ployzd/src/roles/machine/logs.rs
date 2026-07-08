use super::response::{failure_message, machine_domain_error, machine_success, runner_error};
use crate::roles::machine::protocol::{
    MachineLogsTailDomainError, MachineLogsTailResult, MachineLogsTailRpcOk,
    MachineLogsTailRpcRequest, MachineLogsTailRpcResponse,
};
use crate::roles::machine::runner::{
    MachineContainerRunner, MachineLogReader, MachineLogReaderError, MachineLogTail,
};
use ployz_core::ids::MachineId;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse, decode_json_request};

pub(crate) async fn handle_logs_tail<R, L>(
    machine_id: MachineId,
    ports: (R, L),
    request: NatsServiceRequest,
) -> NatsServiceResponse
where
    R: MachineContainerRunner,
    L: MachineLogReader,
{
    let (runner, log_reader) = ports;
    let request = match decode_json_request::<MachineLogsTailRpcRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };

    let existing = match runner.existing_managed_containers().await {
        Ok(existing) => existing,
        Err(error) => return runner_error(error),
    };
    if !existing
        .iter()
        .any(|container| container.container_id == request.container_id)
    {
        return machine_domain_error(MachineLogsTailRpcResponse::DomainError {
            machine_id,
            error: MachineLogsTailDomainError::NotFound {
                container_id: request.container_id,
            },
        });
    }

    match log_reader
        .tail_container_logs(&request.container_id, request.tail_lines)
        .await
    {
        Ok(MachineLogTail { text, truncated }) => {
            machine_success(MachineLogsTailRpcResponse::Ok(MachineLogsTailRpcOk {
                value: MachineLogsTailResult {
                    machine_id,
                    container_id: request.container_id,
                    text,
                    truncated,
                },
            }))
        }
        Err(MachineLogReaderError::NotFound { container_id }) => {
            machine_domain_error(MachineLogsTailRpcResponse::DomainError {
                machine_id,
                error: MachineLogsTailDomainError::NotFound { container_id },
            })
        }
        Err(MachineLogReaderError::ReadFailed {
            container_id,
            message,
        }) => machine_domain_error(MachineLogsTailRpcResponse::DomainError {
            machine_id,
            error: MachineLogsTailDomainError::ReadFailed {
                container_id,
                message: failure_message(message),
            },
        }),
    }
}
