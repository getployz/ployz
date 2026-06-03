use ployz::operation::OperationLedgerPort;
use ployz_api::error::{ProtocolError, ProtocolErrorCode};
use ployz_api::frame::{OutputFrame, RequestFrame, ResponsePayload};
use ployz_api::operation::{
    METHOD_OPERATION_GET, METHOD_OPERATION_STREAM, METHOD_OPERATION_SUBMIT, OperationEventDto,
    OperationGetRequestDto, OperationIdDto, OperationLivenessDto, OperationRecordDto,
    OperationStatusResponseDto, OperationStreamRequestDto, OperationSubmissionDto,
};
use ployz_api::status::METHOD_STATUS;

use super::status::status_dto;
use crate::commands::{
    DaemonCommandError, DaemonCommandRequest, DaemonCommandResponse, DaemonCommandService,
    OperationStatusResponse,
};

pub async fn handle_protocol_request<L>(
    service: &DaemonCommandService<L>,
    request: RequestFrame,
) -> Vec<OutputFrame>
where
    L: OperationLedgerPort + Clone + Send + Sync + 'static,
{
    let id = request.id().clone();
    let result = match request.method() {
        METHOD_STATUS => service.handle(DaemonCommandRequest::Status).await,
        METHOD_OPERATION_SUBMIT => match decode_submit(&request) {
            Ok(submission) => {
                service
                    .handle(DaemonCommandRequest::OperationSubmit(submission))
                    .await
            }
            Err(error) => return vec![OutputFrame::error(Some(id), error)],
        },
        METHOD_OPERATION_GET => match decode_get(&request) {
            Ok(operation) => {
                service
                    .handle(DaemonCommandRequest::OperationGet(operation))
                    .await
            }
            Err(error) => return vec![OutputFrame::error(Some(id), error)],
        },
        METHOD_OPERATION_STREAM => match decode_stream(&request) {
            Ok((operation, cursor)) => {
                service
                    .handle(DaemonCommandRequest::OperationStream { operation, cursor })
                    .await
            }
            Err(error) => return vec![OutputFrame::error(Some(id), error)],
        },
        method => {
            return vec![OutputFrame::error(
                Some(id),
                ProtocolError::unsupported_method(method),
            )];
        }
    };

    match result {
        Ok(response) => encode_response(id, response),
        Err(error) => vec![OutputFrame::error(Some(id), protocol_error(error))],
    }
}

fn decode_submit(
    request: &RequestFrame,
) -> Result<ployz::operation::OperationSubmission, ProtocolError> {
    request
        .decode_payload::<OperationSubmissionDto>()
        .and_then(|payload| payload.to_domain())
}

fn decode_get(request: &RequestFrame) -> Result<ployz::operation::OperationId, ProtocolError> {
    request
        .decode_payload::<OperationGetRequestDto>()
        .and_then(|payload| payload.operation().to_domain())
}

fn decode_stream(
    request: &RequestFrame,
) -> Result<
    (
        ployz::operation::OperationId,
        Option<ployz::operation::OperationEventCursor>,
    ),
    ProtocolError,
> {
    request
        .decode_payload::<OperationStreamRequestDto>()
        .and_then(|payload| Ok((payload.operation().to_domain()?, payload.cursor()?)))
}

fn encode_response(
    id: ployz_api::frame::RequestId,
    response: DaemonCommandResponse,
) -> Vec<OutputFrame> {
    match response {
        DaemonCommandResponse::Status(status) => {
            vec![OutputFrame::response(
                id,
                ResponsePayload::Status(status_dto(&status)),
            )]
        }
        DaemonCommandResponse::OperationSubmitted(operation) => {
            vec![OutputFrame::response(
                id,
                ResponsePayload::OperationSubmitted {
                    operation: OperationIdDto::from(&operation),
                },
            )]
        }
        DaemonCommandResponse::OperationStatus(status) => {
            vec![OutputFrame::response(
                id,
                ResponsePayload::OperationStatus(operation_status_dto(&status)),
            )]
        }
        DaemonCommandResponse::OperationEvents(events) => {
            let events = events.iter().map(OperationEventDto::from_domain).collect();
            vec![OutputFrame::response(
                id,
                ResponsePayload::OperationEvents { events },
            )]
        }
    }
}

fn operation_status_dto(value: &OperationStatusResponse) -> OperationStatusResponseDto {
    OperationStatusResponseDto::new(
        OperationRecordDto::from_domain(value.record()),
        OperationLivenessDto::from_domain(value.liveness()),
    )
}

fn protocol_error(value: DaemonCommandError) -> ProtocolError {
    match value {
        DaemonCommandError::UnsupportedOperation { kind } => ProtocolError::new(
            ProtocolErrorCode::UnsupportedOperation,
            format!("operation kind {} is not supported", kind.as_str()),
        ),
        DaemonCommandError::OperationNotFound { operation } => ProtocolError::new(
            ProtocolErrorCode::OperationNotFound,
            format!("operation {} was not found", operation.as_str()),
        )
        .with_operation(operation.as_str()),
        DaemonCommandError::Operation { source } => ProtocolError::from(source),
    }
}
