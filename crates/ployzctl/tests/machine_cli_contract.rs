use ployz_core::ids::{NodeId, OperationId, OperationOwnerId};
use ployz_core::install::{
    AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
    MachineJoinArtifact, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinPloyzdArtifact, MachineJoinTrustedNats,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsServerName};
use ployz_core::ops::{EventSequence, OperationLeaseExpiresAt, OperationOwnerLease};
use ployz_core::state::{
    ActiveMachineState, GatewayServingStatus, GatewayStatusObservation, NodePublicIpObservation,
};
use ployz_sdk_types::{AcceptedOperation, MachineSnapshot};
use ployzctl::commands::machine::{
    MachineAddOutput, MachineBootstrapUrl, MachineInspectOutput, MachineJoinRuntimeNatsUrl,
    MachineJoinToken, MachineListOutput, MachineName,
};

#[test]
fn machine_add_prints_bootstrap_command_without_nats_credentials() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    }
    .render();

    assert!(output.contains("operation op_machine"));
    assert!(output.contains("node node_2"));
    assert!(output.contains("join-token join_once_123"));
    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains(" | PLOYZ_NATS_URL='nats://127.0.0.1:7422' PLOYZ_NATS_CA_B64="));
    assert!(output.contains(&format!(
        "PLOYZ_JOIN_NKEY_SEED='{}' sh -s -- ",
        TEST_JOIN_SEED
    )));
    assert!(output.contains("--join-token 'join_once_123'"));
    assert!(!output.contains("creds"));
}

#[test]
fn machine_add_prints_runtime_nats_url_from_accepted_response() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7423"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains("join-token join_once_123"));
    assert!(output.contains(" | PLOYZ_NATS_URL='nats://127.0.0.1:7423' PLOYZ_NATS_CA_B64="));
    assert!(output.contains("--join-token 'join_once_123'"));
}

#[test]
fn machine_add_debug_redacts_join_token() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        join_seed: test_join_seed(),
    };

    let debug = format!("{output:?}");

    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains("join_once_123"));
    assert!(debug.contains("127.0.0.1"));
}

#[test]
fn machine_add_shell_quotes_join_material() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh/bootstrap?x='quoted'")
            .expect("valid bootstrap url"),
        join_bundle: machine_join_bundle("nats://127.0.0.1:7422"),
        join_token: MachineJoinToken::try_new("join'quoted'").expect("valid join token"),
        join_seed: test_join_seed(),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh/bootstrap?x='\\''quoted'\\'''"));
    assert!(output.contains("PLOYZ_NATS_URL='nats://127.0.0.1:7422'"));
    assert!(output.contains("--join-token 'join'\\''quoted'\\'''"));
}

#[test]
fn machine_add_rejects_bootstrap_url_that_curl_could_treat_as_an_option() {
    assert!(MachineBootstrapUrl::try_new("--help").is_err());
    assert!(MachineBootstrapUrl::try_new("http://get.ployz.sh").is_err());
}

#[test]
fn machine_add_rejects_join_tokens_with_shell_invisible_characters() {
    assert!(MachineJoinToken::try_new("").is_err());
    assert!(MachineJoinToken::try_new("join token").is_err());
    assert!(MachineJoinToken::try_new("join\ntoken").is_err());
}

#[test]
fn machine_list_renders_machine_summaries() {
    let output = MachineListOutput {
        machines: vec![machine_snapshot(
            "node_1",
            Some(GatewayServingStatus::Current),
        )],
    }
    .render();

    assert_eq!(
        output,
        "node_1 edge_1 public-ip 203.0.113.10 gateway current 127.0.0.1:8080 routes 2 containers 3\n"
    );
}

#[test]
fn machine_list_renders_no_output_without_machines() {
    let output = MachineListOutput {
        machines: Vec::new(),
    }
    .render();

    assert_eq!(output, "");
}

#[test]
fn machine_inspect_renders_machine_detail() {
    let output = MachineInspectOutput::new(machine_snapshot(
        "node_1",
        Some(GatewayServingStatus::LastKnownGood),
    ))
    .render();

    assert_eq!(
        output,
        "node node_1\nname edge_1\nactivated-by op_machine\npublic-ip 203.0.113.10\ngateway last-known-good 127.0.0.1:8080 routes 2\ncontainers 3\n"
    );
}

#[test]
fn machine_inspect_renders_missing_observations_as_unknown() {
    let mut machine = machine_snapshot("node_1", None);
    machine.public_ip = None;

    let output = MachineInspectOutput::new(machine).render();

    assert_eq!(
        output,
        "node node_1\nname edge_1\nactivated-by op_machine\npublic-ip unknown\ngateway none\ncontainers 3\n"
    );
}

fn accepted_operation(operation_id: &str) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: self::operation_id(operation_id),
        watch_subject: format!("plz.v1.op.{operation_id}.>"),
        start_sequence: event_sequence(1),
        owner_lease: OperationOwnerLease::new(
            self::operation_id(operation_id),
            OperationOwnerId::try_new("control").expect("valid owner id"),
            OperationLeaseExpiresAt::try_new(120).expect("valid lease expiry"),
        ),
    }
}

fn machine_snapshot(node_id: &str, gateway: Option<GatewayServingStatus>) -> MachineSnapshot {
    let node_id = self::node_id(node_id);
    MachineSnapshot {
        active: ActiveMachineState {
            node_id: node_id.clone(),
            name: MachineName::try_new("edge_1").expect("valid machine name"),
            activated_by: operation_id("op_machine"),
        },
        public_ip: Some(NodePublicIpObservation {
            node_id: node_id.clone(),
            public_ip: "203.0.113.10".parse().expect("valid public ip"),
        }),
        gateway: gateway.map(|serving| GatewayStatusObservation {
            node_id,
            listen_addr: "127.0.0.1:8080".parse().expect("valid listen addr"),
            serving,
            route_count: 2,
        }),
        observed_container_count: 3,
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

const TEST_JOIN_SEED: &str = "SUAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn test_join_seed() -> ployz_core::nats_config::NatsUserSeed {
    ployz_core::nats_config::NatsUserSeed::try_new(TEST_JOIN_SEED).expect("test seed is valid")
}

fn machine_join_bundle(runtime_nats_url: &str) -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(runtime_nats_url)
                .expect("valid runtime NATS URL"),
            trusted_nats: MachineJoinTrustedNats {
                server_name: NatsServerName::try_new("server_1").expect("valid NATS server name"),
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid CA pem"),
            },
            ployzd: MachineJoinPloyzdArtifact {
                version: version("0.1.0"),
                source: source("/tmp/ployzd"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/bin/ployzd"),
            },
            ebpf_bytecode: MachineJoinArtifact {
                version: version("0.1.0"),
                source: source("/tmp/ployz-ebpf-tc"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"),
            },
            ebpf_ctl: MachineJoinArtifact {
                version: version("0.1.0"),
                source: source("/tmp/ployz-ebpf-ctl"),
                sha256: digest("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                install_path: absolute_path("/usr/local/bin/ployz-ebpf-ctl"),
            },
        },
    }
}

fn version(value: &str) -> InstallArtifactVersion {
    InstallArtifactVersion::try_new(value).expect("valid artifact version")
}

fn source(value: &str) -> InstallArtifactSource {
    InstallArtifactSource::try_new(value).expect("valid artifact source")
}

fn digest(value: &str) -> InstallSha256Digest {
    InstallSha256Digest::try_new(value).expect("valid digest")
}

fn absolute_path(value: &str) -> AbsoluteInstallPath {
    AbsoluteInstallPath::try_new(value).expect("valid absolute path")
}
