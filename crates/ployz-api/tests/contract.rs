use ployz_api::frame::{OutputFrame, RequestFrame, RequestId, ResponsePayload};
use ployz_api::operation::{
    METHOD_OPERATION_SUBMIT, OperationEventDto, OperationIdDto, OperationSubmissionDto,
};
use ployz_api::status::{
    CorrosionCleanupDto, CorrosionPhaseDto, METHOD_STATUS, PeerPhaseDto, SchemaPhaseDto,
    StartupReportDto, StatusResponseDto,
};
use serde_json::json;

#[test]
fn status_request_and_response_preserve_frame_shape() {
    let request = RequestFrame::new(
        RequestId::parse("req-1").expect("request id"),
        METHOD_STATUS,
        json!({}),
    );
    let line = serde_json::to_string(&request).expect("request json");
    let decoded = RequestFrame::decode(&line).expect("decoded request");

    assert_eq!(decoded.id().as_str(), "req-1");
    assert_eq!(decoded.method(), "status");

    let status = StatusResponseDto::new(
        true,
        true,
        Some("endpoint-1".to_owned()),
        StartupReportDto::new(
            CorrosionPhaseDto::Ready,
            CorrosionCleanupDto::NotRun,
            SchemaPhaseDto::Ready,
            PeerPhaseDto::Ready,
            "127.0.0.1:8081",
            "state/peer.key",
        ),
        None,
    );
    let output = OutputFrame::response(request.id().clone(), ResponsePayload::Status(status));
    let encoded = output.encode().expect("response json");
    let value: serde_json::Value = serde_json::from_str(&encoded).expect("response value");

    assert_eq!(value["type"], "response");
    assert_eq!(value["result"]["kind"], "status");
    assert_eq!(value["result"]["command_service_ready"], true);
    assert_eq!(value["result"]["startup"]["peer_phase"]["kind"], "ready");
}

#[test]
fn operation_submit_payload_decodes_to_domain_submission() {
    let request = RequestFrame::new(
        RequestId::parse("req-2").expect("request id"),
        METHOD_OPERATION_SUBMIT,
        json!({
            "operation": "op-1",
            "kind": "fake.echo",
            "scope": "cluster",
            "idempotency": "idem-1",
            "principal": "cloud",
            "payload_fingerprint": "sha256:test"
        }),
    );

    let payload: OperationSubmissionDto = request.decode_payload().expect("submit payload");
    let domain = payload.to_domain().expect("domain submission");

    assert_eq!(domain.operation().as_str(), "op-1");
    assert_eq!(domain.kind().as_str(), "fake.echo");
    assert_eq!(domain.payload_fingerprint().as_str(), "sha256:test");
}

#[test]
fn malformed_json_is_a_structured_parse_error() {
    let error = RequestFrame::decode("{ nope").expect_err("parse error");

    assert_eq!(error.code(), ployz_api::ProtocolErrorCode::Parse);
}

#[test]
fn blank_request_id_is_rejected_on_decode() {
    let error = RequestFrame::decode(r#"{"id":" ","method":"status","payload":{}}"#)
        .expect_err("malformed id");

    assert_eq!(error.code(), ployz_api::ProtocolErrorCode::MalformedPayload);
}

#[test]
fn contract_contains_operation_event_cursor_and_terminal_fields() {
    let operation = ployz::operation::OperationId::parse("op-1").expect("operation id");
    let terminal = ployz::operation::OperationTerminal::failed(
        ployz::operation::OperationFailureCode::parse("runtime.timeout").expect("failure code"),
    );
    let event = ployz::operation::OperationEvent::new(
        operation,
        ployz::operation::OperationEventKind::terminal(terminal),
        std::time::UNIX_EPOCH,
    );
    let dto = OperationEventDto::from_domain(&event);
    let output = OutputFrame::response(
        RequestId::parse("req-3").expect("request id"),
        ResponsePayload::OperationEvents { events: vec![dto] },
    )
    .encode()
    .expect("event json");
    let value: serde_json::Value = serde_json::from_str(&output).expect("event value");

    assert_eq!(value["type"], "response");
    assert_eq!(value["result"]["kind"], "operation_events");
    assert_eq!(value["result"]["events"][0]["cursor"]["operation"], "op-1");
    assert_eq!(value["result"]["events"][0]["kind"]["kind"], "terminal");
    assert_eq!(
        value["result"]["events"][0]["kind"]["terminal"]["kind"],
        "failed"
    );
    assert_eq!(
        value["result"]["events"][0]["kind"]["terminal"]["code"],
        "runtime.timeout"
    );
}

#[test]
fn operation_id_dto_rejects_empty_domain_value() {
    let dto = OperationIdDto::new(" ");

    assert_eq!(
        dto.to_domain().expect_err("malformed").code(),
        ployz_api::ProtocolErrorCode::MalformedPayload
    );
}
