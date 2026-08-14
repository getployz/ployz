use std::{collections::BTreeSet, num::NonZeroU32};

use ployz_core::{
    CREATE_CONTAINER_CAPABILITY, CapabilityName, CodecError, ConfigMount, ConfigSpec,
    ContainerCreated, ContainerKind, ContainerPath, ContainerResources,
    ContainerRuntimeObservation, ContractDescription, CreateContainerRequest,
    DESCRIBE_CONTRACT_CAPABILITY, FanoutFailure, FanoutOutcome, FanoutResponse, FramingError,
    HealthObservation, ImageSummary, LIST_IMAGES_CAPABILITY, MachineFailure, MachineId,
    MachineImages, MachineName, MachinePath, MachineRpc, MachineRpcClient, MachineRpcServer,
    MachineSelector, MachineSuccess, MachineTokenRequest, MachineUpdate, NameMatches,
    OpaquePayload, PROTOCOL_MAJOR, PartialResult, Placement, PreDeployHook, PublicIpDiscovery,
    PublicIpUpdate, PullPolicy, RESET_MACHINE_CAPABILITY, RemoveLocalMachineRequest,
    RemoveMachineRequest, RequestedServiceSpec, ResolvedServiceSpec, ResponseKind, RpcError,
    RpcErrorCode, RpcRequest, RpcRequestBody, RpcResponse, RpcResponseBody, ServiceContainerSpec,
    ServiceId, ServiceMode, ServiceMount, ServiceName, ServiceVolume, ServiceVolumeReference,
    UpdateConfig, UpdateOrder, VolumeSource, encode_grpc_frame, grpc_frames,
};
use prost::Message;
use serde_json::{Value, json};

const MACHINE_ID: &str = "0123456789abcdef0123456789abcdef";
const OTHER_MACHINE_ID: &str = "fedcba9876543210fedcba9876543210";

#[test]
fn identities_validate_and_serialize_as_their_wire_strings() {
    let machine = MachineId::parse(MACHINE_ID).unwrap();
    assert_eq!(
        serde_json::to_string(&machine).unwrap(),
        format!("\"{MACHINE_ID}\"")
    );
    assert_eq!(
        serde_json::from_str::<MachineId>(&format!("\"{MACHINE_ID}\"")).unwrap(),
        machine
    );

    assert!(MachineId::parse("0123456789ABCDEF0123456789ABCDEF").is_err());
    assert!(ServiceId::parse("too-short").is_err());
    assert!(ServiceName::parse("Uppercase").is_err());
    assert!(ServiceName::parse("valid-service").is_ok());
}

#[test]
fn duplicate_name_matches_remain_ambiguous() {
    let first = ServiceId::parse("11111111111111111111111111111111").unwrap();
    let second = ServiceId::parse("22222222222222222222222222222222").unwrap();

    assert_eq!(
        NameMatches::from_matches(vec![first.clone(), second.clone()]),
        NameMatches::Ambiguous(vec![first, second])
    );
}

#[test]
fn partial_results_keep_successes_failures_and_omissions_together() {
    let result = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
            value: "running".to_owned(),
        }],
        failures: vec![MachineFailure {
            machine_id: MachineId::parse(OTHER_MACHINE_ID).unwrap(),
            error: "unreachable".to_owned(),
        }],
        omissions: vec![MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()],
    };

    assert!(!result.all_targets_succeeded());
    let round_trip: PartialResult<String, String> =
        serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
    assert_eq!(round_trip, result);
}

#[test]
fn unknown_observation_variants_preserve_the_raw_value() {
    let future = json!({
        "state": "hibernating",
        "wake_at": "tomorrow",
        "vendor": { "reason": 7 }
    });
    let observation: ContainerRuntimeObservation = serde_json::from_value(future.clone()).unwrap();

    assert_eq!(
        observation,
        ContainerRuntimeObservation::Unknown {
            raw: future.clone()
        }
    );
    assert_eq!(serde_json::to_value(observation).unwrap(), future);

    let health: HealthObservation = serde_json::from_str("\"degraded\"").unwrap();
    assert_eq!(health, HealthObservation::Unrecognized("degraded".into()));

    let known_with_addition: ContainerRuntimeObservation = serde_json::from_value(json!({
        "state": "running",
        "health": "healthy",
        "engine_detail": "accepted and ignored"
    }))
    .unwrap();
    assert_eq!(
        known_with_addition,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy
        }
    );
}

#[test]
fn additive_fields_are_accepted_on_requests_and_responses() {
    let request = OpaquePayload::new(
        serde_json::to_vec(&json!({
            "protocol_major": 1,
            "command": "describe_contract",
            "payload": {},
            "future_request_metadata": true
        }))
        .unwrap(),
    );
    assert_eq!(
        request.decode_request().unwrap(),
        RpcRequest::describe_contract()
    );

    let response: RpcResponse = serde_json::from_value(json!({
        "protocol_major": 1,
        "kind": "future_response",
        "payload": { "answer": 42 },
        "future_response_metadata": true
    }))
    .unwrap();
    assert_eq!(
        response.kind(),
        ResponseKind::Unknown("future_response".into())
    );
    assert!(matches!(
        response.body,
        RpcResponseBody::Unknown { ref payload, .. }
            if payload == &json!({ "answer": 42 })
    ));
}

#[test]
fn unknown_commands_are_rejected_with_a_typed_error() {
    let payload = OpaquePayload::new(
        br#"{"protocol_major":1,"command":"future_mutation","payload":{}}"#.to_vec(),
    );

    assert!(matches!(
        payload.decode_request(),
        Err(CodecError::UnsupportedCommand(command)) if command == "future_mutation"
    ));
}

#[test]
fn incompatible_protocol_majors_are_rejected_explicitly() {
    let payload = OpaquePayload::new(
        br#"{"protocol_major":2,"command":"describe_contract","payload":{}}"#.to_vec(),
    );

    assert!(matches!(
        payload.decode_request(),
        Err(CodecError::UnsupportedProtocolMajor {
            requested: 2,
            supported: PROTOCOL_MAJOR
        })
    ));

    let response = OpaquePayload::new(
        br#"{"protocol_major":2,"kind":"future_response","payload":{}}"#.to_vec(),
    );
    assert!(matches!(
        response.decode_response(),
        Err(CodecError::UnsupportedProtocolMajor {
            requested: 2,
            supported: PROTOCOL_MAJOR
        })
    ));
}

#[test]
fn per_machine_capability_fixture_is_stable_and_additive() {
    let description = ContractDescription {
        machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "0.1.0".into(),
        capabilities: BTreeSet::from([
            CapabilityName::parse("ployz.containers.inspect.v1").unwrap(),
            CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY).unwrap(),
        ]),
    };

    assert!(description.supports(DESCRIBE_CONTRACT_CAPABILITY));
    assert!(!description.supports("ployz.containers.replace.v2"));
    assert_eq!(
        serde_json::to_string(&description).unwrap(),
        concat!(
            "{\"machine_id\":\"0123456789abcdef0123456789abcdef\",",
            "\"protocol_major\":1,\"daemon_version\":\"0.1.0\",",
            "\"capabilities\":[\"ployz.containers.inspect.v1\",",
            "\"ployz.rpc.describe-contract.v1\"]}"
        )
    );

    let with_future_field: ContractDescription = serde_json::from_value(json!({
        "machine_id": MACHINE_ID,
        "protocol_major": 1,
        "daemon_version": "0.2.0",
        "capabilities": [DESCRIBE_CONTRACT_CAPABILITY, "vendor.future.contract.v1"],
        "build_revision": "future"
    }))
    .unwrap();
    assert!(with_future_field.supports("vendor.future.contract.v1"));
    assert!(CapabilityName::parse("future_contract").is_err());
}

#[test]
fn json_payload_round_trips_through_the_opaque_prost_envelope() {
    let request = RpcRequest::describe_contract().encode().unwrap();
    let framed = request.encode_to_vec();
    let decoded = OpaquePayload::decode(framed.as_slice()).unwrap();

    assert_eq!(
        decoded.decode_request().unwrap(),
        RpcRequest::describe_contract()
    );

    let response = RpcResponse {
        protocol_major: PROTOCOL_MAJOR,
        body: RpcResponseBody::Unknown {
            kind: "ployz.future.result.v1".into(),
            payload: Value::Array(vec![json!(1), json!(2)]),
        },
    };
    let response_payload = response.encode().unwrap();
    assert_eq!(
        response_payload.decode_json::<RpcResponse>().unwrap(),
        response
    );
}

#[test]
fn image_list_contract_keeps_machine_local_store_and_platforms() {
    let request = RpcRequest::list_images(Some("example.test/api:1.*".into()));
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);

    let images = MachineImages {
        containerd_store: true,
        images: vec![ImageSummary {
            id: "sha256:abcdef".into(),
            repo_tags: vec!["example.test/api:1.2".into()],
            created: 17,
            size: 42,
            containers: 1,
            platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
        }],
    };
    let response = RpcResponse::machine_images(images.clone());
    assert_eq!(
        response
            .encode()
            .unwrap()
            .decode_response()
            .unwrap()
            .decode_machine_images()
            .unwrap(),
        &images
    );
    assert_eq!(LIST_IMAGES_CAPABILITY, "ployz.image.list.v1");
}

#[test]
fn fanout_frames_preserve_opaque_messages_and_type_target_failures() {
    let machine_id = MachineId::parse(MACHINE_ID).unwrap();
    let machine_name = MachineName::parse("machine-a").unwrap();
    let original = encode_grpc_frame(br#"{\"future\":true}"#);
    let success = FanoutResponse::success(&machine_id, &machine_name, original.clone()).unwrap();
    let encoded = success.encode_grpc_frame();

    let frames = grpc_frames(&encoded).unwrap();
    let [outer] = frames.as_slice() else {
        panic!("expected one outer gRPC frame")
    };
    let decoded = FanoutResponse::decode_grpc_frame(outer).unwrap();
    assert_eq!(decoded.machine_id().unwrap(), machine_id);
    assert_eq!(decoded.machine_name().unwrap(), machine_name);
    assert!(matches!(
        decoded.outcome,
        Some(FanoutOutcome::FramedPayload(ref payload)) if payload == &original
    ));

    let failure = FanoutFailure {
        code: tonic::Code::Unavailable as u32,
        message: "connection refused".into(),
        details: vec![1, 2, 3],
    };
    let decoded = FanoutResponse::decode_grpc_frame(
        &FanoutResponse::failure(&machine_id, &machine_name, failure.clone()).encode_grpc_frame(),
    )
    .unwrap();
    assert_eq!(decoded.outcome, Some(FanoutOutcome::Failure(failure)));

    assert_eq!(grpc_frames(&[0, 0]), Err(FramingError::TruncatedHeader));
    assert_eq!(
        grpc_frames(&[0, 0, 0, 0, 4, 1, 2, 3]),
        Err(FramingError::TruncatedMessage {
            declared: 4,
            available: 3,
        })
    );
}

#[test]
fn stream_frames_round_trip_binary_exec_payloads_and_control_kinds() {
    use ployz_core::{ExecConfig, ExecOptions, ExecRequestFrame, ExecResponseFrame};

    let container_id = ployz_core::ContainerId::parse("a".repeat(64)).unwrap();
    let config = ExecRequestFrame::Config(ExecConfig {
        container_id,
        options: ExecOptions {
            command: vec!["printf".into(), "\\377".into()],
            attach_stdin: true,
            attach_stdout: true,
            attach_stderr: true,
            tty: false,
            detach: false,
        },
    });
    assert_eq!(
        ExecRequestFrame::decode(&config.encode().unwrap()).unwrap(),
        config
    );

    for request in [
        ExecRequestFrame::Stdin(Vec::new()),
        ExecRequestFrame::Stdin(vec![0, 0xff, b'\n']),
        ExecRequestFrame::Resize {
            width: 132,
            height: 43,
        },
    ] {
        assert_eq!(
            ExecRequestFrame::decode(&request.encode().unwrap()).unwrap(),
            request
        );
    }
    for response in [
        ExecResponseFrame::ExecId("exec-1".into()),
        ExecResponseFrame::Stdout(vec![0, 0xff]),
        ExecResponseFrame::Stderr(Vec::new()),
        ExecResponseFrame::Exit(42),
    ] {
        assert_eq!(
            ExecResponseFrame::decode(&response.encode().unwrap()).unwrap(),
            response
        );
    }
}

#[test]
fn streaming_requests_keep_typed_control_options_outside_raw_frames() {
    use ployz_core::{ContainerLogsRequest, LogsOptions, MachineLogService, MachineLogsRequest};

    let options = LogsOptions {
        follow: true,
        tail: -1,
        since: "2m30s".into(),
        until: "2026-08-14T09:00:00Z".into(),
    };
    for request in [
        RpcRequest::container_logs(ContainerLogsRequest {
            container_id: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
            options: options.clone(),
        }),
        RpcRequest::machine_logs(MachineLogsRequest {
            service: MachineLogService::Ployz,
            options,
        }),
    ] {
        assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);
    }
}

#[test]
fn log_frames_keep_identity_outside_untouched_message_bytes() {
    use ployz_core::{LogEntry, LogMetadata, LogOrigin, LogStream};

    let entry = LogEntry {
        metadata: LogMetadata {
            origin: LogOrigin::Service {
                service_id: ployz_core::ServiceId::parse("1".repeat(32)).unwrap(),
                service_name: ployz_core::ServiceName::parse("api").unwrap(),
                container_id: ployz_core::ContainerId::parse("b".repeat(64)).unwrap(),
                hook: Some("pre-deploy".into()),
            },
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
            machine_name: MachineName::parse("machine-a").unwrap(),
        },
        stream: LogStream::Stderr,
        timestamp_unix_nanos: 1_765_000_000_123_456_789,
        message: vec![0, 0xff, b'\n'],
        error: None,
    };
    let encoded = entry.encode().unwrap();
    let decoded = LogEntry::decode(&encoded).unwrap();

    assert_eq!(decoded, entry);
    assert_eq!(decoded.message, vec![0, 0xff, b'\n']);
}

#[test]
fn malformed_stream_frames_return_protocol_errors() {
    use ployz_core::{ExecRequestFrame, StreamProtocolError};

    assert_eq!(
        ExecRequestFrame::decode(&OpaquePayload::new(vec![1, 0, 0])),
        Err(StreamProtocolError::TruncatedHeader)
    );
    assert_eq!(
        ExecRequestFrame::decode(&OpaquePayload::new(vec![0xfe, 0, 0, 0, 0])),
        Err(StreamProtocolError::UnknownKind(0xfe))
    );
    assert_eq!(
        ExecRequestFrame::decode(&OpaquePayload::new(vec![2, 0, 0, 0, 3, 1, 2])),
        Err(StreamProtocolError::LengthMismatch {
            declared: 3,
            actual: 2,
        })
    );
}

#[test]
fn typed_errors_preserve_future_error_codes() {
    let error: RpcError = serde_json::from_value(json!({
        "code": "rate_limited",
        "message": "try later",
        "details": { "retry_after_seconds": 2 },
        "future_metadata": true
    }))
    .unwrap();

    assert_eq!(error.code, RpcErrorCode::Unknown("rate_limited".into()));
    let response = RpcResponse::error(error);
    assert_eq!(response.kind(), ResponseKind::Error);
    assert!(matches!(
        response.body,
        RpcResponseBody::Error(RpcError { ref details, .. })
            if details["retry_after_seconds"] == 2
    ));
}

#[test]
fn reset_command_and_acknowledgement_have_a_stable_capability() {
    let request = RpcRequest::reset().encode().unwrap();
    assert_eq!(request.decode_request().unwrap(), RpcRequest::reset());

    let response = RpcResponse::reset_accepted();
    assert_eq!(response.kind(), ResponseKind::ResetAccepted);
    assert!(response.decode_reset_accepted().is_ok());
    assert_eq!(RESET_MACHINE_CAPABILITY, "ployz.machine.reset.v1");
}

#[test]
fn volume_and_container_commands_keep_machine_local_inputs_exact() {
    use std::collections::BTreeMap;

    use ployz_core::{
        CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName, InspectVolumeRequest,
        ListVolumesRequest, RemoveVolumeRequest,
    };

    assert!(DockerVolumeName::parse("").is_err());
    let name = DockerVolumeName::parse("data").unwrap();
    let create = CreateVolumeRequest {
        name: name.clone(),
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("purpose".into(), "database".into())]),
    };
    assert_eq!(
        RpcRequest::create_volume(create.clone())
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::CreateVolume(create)
    );
    assert_eq!(
        RpcRequest::list_volumes()
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::ListVolumes(ListVolumesRequest {})
    );
    assert_eq!(
        RpcRequest::inspect_volume(name.clone())
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::InspectVolume(InspectVolumeRequest { name: name.clone() })
    );
    assert_eq!(
        RpcRequest::remove_volume(name.clone(), true)
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::RemoveVolume(RemoveVolumeRequest {
            name: name.clone(),
            force: true,
        })
    );

    let volume = DockerVolume {
        id: DockerVolumeId {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
            name,
        },
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("purpose".into(), "database".into())]),
    };
    assert_eq!(
        RpcResponse::volume_list(vec![volume.clone()])
            .decode_volume_list()
            .unwrap(),
        &[volume]
    );
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": "11111111111111111111111111111111",
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
    }))
    .unwrap();
    let request = RpcRequest::create_container(CreateContainerRequest {
        kind: ContainerKind::ServiceContainer,
        resolved_spec: spec.clone(),
    });
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);

    let created = ContainerCreated {
        container_id: ployz_core::ContainerId::parse("a".repeat(64)).unwrap(),
        display_name: "api-abcd".into(),
    };
    let response = RpcResponse::container_created(created.clone());
    assert_eq!(response.decode_container_created().unwrap(), &created);
    assert_eq!(CREATE_CONTAINER_CAPABILITY, "ployz.container.create.v1");
}

#[test]
fn machine_administration_requests_round_trip_as_typed_payloads() {
    let requests = [
        RpcRequest::machine_token(MachineTokenRequest {
            public_ip: PublicIpDiscovery::Override("203.0.113.7".parse().unwrap()),
            ..Default::default()
        }),
        RpcRequest::update_machine(MachineUpdate {
            name: Some(MachineName::parse("renamed").unwrap()),
            public_ip: PublicIpUpdate::Remove,
            advertised_endpoints: None,
        }),
        RpcRequest::remove_local_machine(RemoveLocalMachineRequest::default()),
        RpcRequest::remove_machine(RemoveMachineRequest {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        }),
        RpcRequest::inspect_wireguard(),
    ];
    for request in requests {
        assert_eq!(
            request.clone().encode().unwrap().decode_request().unwrap(),
            request
        );
    }
}

#[test]
fn requested_and_resolved_specs_and_mounts_round_trip() {
    let container = ServiceContainerSpec {
        image: "ghcr.io/example/api:sha".into(),
        command: vec!["serve".into()],
        entrypoint: Vec::new(),
        environment: Default::default(),
        cap_add: vec!["NET_ADMIN".into()],
        cap_drop: Vec::new(),
        healthcheck: None,
        pull_policy: PullPolicy::Missing,
        init: None,
        user: None,
        working_directory: Some(ContainerPath::parse("/srv/app").unwrap()),
        tty: false,
        open_stdin: false,
        privileged: false,
        pid_mode: None,
        log_driver: None,
        resources: ContainerResources {
            memory_bytes: Some(256 * 1024 * 1024),
            ..Default::default()
        },
        stop_grace_period_millis: Some(10_000),
        sysctls: Default::default(),
        config_mounts: vec![ConfigMount {
            config_name: "settings".into(),
            target: Some(ContainerPath::parse("/etc/api/settings.toml").unwrap()),
            uid: Some(1000),
            gid: Some(1000),
            mode: Some(0o440),
        }],
    };
    let reference = ServiceVolumeReference::parse("data").unwrap();
    let volume = ServiceVolume {
        reference: reference.clone(),
        source: VolumeSource::Bind {
            machine_path: MachinePath::parse("/srv/api").unwrap(),
            create_machine_path: true,
            propagation: None,
            recursive: None,
        },
    };
    let mount = ServiceMount {
        volume: reference,
        target: ContainerPath::parse("/var/lib/api").unwrap(),
        read_only: false,
    };
    let requested = RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode: ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        },
        container: container.clone(),
        placement: Placement {
            machines: vec![MachineSelector::parse("edge").unwrap()],
        },
        ports: Vec::new(),
        volumes: vec![volume.clone()],
        mounts: vec![mount.clone()],
        configs: vec![ConfigSpec {
            name: "settings".into(),
            content: b"port = 8080".to_vec(),
        }],
        pre_deploy: Some(PreDeployHook {
            command: vec!["migrate".into()],
            environment: Default::default(),
            privileged: None,
            timeout_millis: Some(30_000),
            user: None,
        }),
        caddy_config: Some("reverse_proxy localhost:8080".into()),
        update: UpdateConfig {
            order: None,
            monitor_millis: Some(5_000),
        },
    };
    let resolved = ResolvedServiceSpec {
        service_id: ServiceId::parse("11111111111111111111111111111111").unwrap(),
        name: requested.name.clone(),
        mode: requested.mode.clone(),
        container,
        placement: requested.placement.clone(),
        ports: Vec::new(),
        volumes: vec![volume],
        mounts: vec![mount],
        configs: requested.configs.clone(),
        pre_deploy: requested.pre_deploy.clone(),
        caddy_config: requested.caddy_config.clone(),
        update: ployz_core::ResolvedUpdateConfig {
            order: UpdateOrder::StartFirst,
            monitor_millis: Some(5_000),
        },
    };

    let requested_json = serde_json::to_value(&requested).unwrap();
    assert_eq!(
        serde_json::from_value::<RequestedServiceSpec>(requested_json.clone()).unwrap(),
        requested
    );
    let mut older_requested_json = requested_json;
    older_requested_json
        .as_object_mut()
        .unwrap()
        .remove("update");
    assert_eq!(
        serde_json::from_value::<RequestedServiceSpec>(older_requested_json)
            .unwrap()
            .update,
        UpdateConfig::default()
    );
    let resolved_json = serde_json::to_value(&resolved).unwrap();
    assert_eq!(
        serde_json::from_value::<ResolvedServiceSpec>(resolved_json).unwrap(),
        resolved
    );
}

struct FixtureMachineRpc;

#[tonic::async_trait]
impl MachineRpc for FixtureMachineRpc {
    type ExecStream = tonic::codegen::tokio_stream::Empty<Result<OpaquePayload, tonic::Status>>;
    type ContainerLogsStream =
        tonic::codegen::tokio_stream::Empty<Result<OpaquePayload, tonic::Status>>;
    type MachineLogsStream =
        tonic::codegen::tokio_stream::Empty<Result<OpaquePayload, tonic::Status>>;

    async fn describe_contract(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn inspect(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn machine_token(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn initialize(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn register(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn join(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn list_machines(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn list_containers(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn create_volume(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn inspect_container(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn list_volumes(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn create_container(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn inspect_volume(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn start_container(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn remove_volume(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn stop_container(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn remove_container(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn exec(
        &self,
        _request: tonic::Request<tonic::Streaming<OpaquePayload>>,
    ) -> Result<tonic::Response<Self::ExecStream>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn container_logs(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<Self::ContainerLogsStream>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn machine_logs(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<Self::MachineLogsStream>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn reset(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn update_machine(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn remove_local_machine(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn remove_machine(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn inspect_wireguard(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }

    async fn list_images(
        &self,
        _request: tonic::Request<OpaquePayload>,
    ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
        unreachable!("compile-time service fixture")
    }
}

#[test]
fn tonic_generates_both_sides_of_the_machine_rpc_service() {
    let _server = MachineRpcServer::new(FixtureMachineRpc);
    let _client: Option<MachineRpcClient<tonic::transport::Channel>> = None;
}
