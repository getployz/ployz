use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId};
use ployz_core::ops::{
    EventSequence, OperationLeaseExpiresAt, OperationOwnerLease, ReplayedOperationEvent,
};
use ployz_sdk_types::AcceptedOperation;
use ployzctl::commands::deploy::DetachedDeployOutput;
use ployzctl::commands::init::{FirstNodeGateway, FirstNodeInitOutput, first_node_process_set};
use ployzctl::commands::machine::{MachineAddOutput, MachineBootstrapUrl, MachineJoinToken};
use ployzctl::commands::ops::WatchOutput;
use ployzctl::commands::upgrade::{UpgradeComponent, UpgradeOutput};

#[test]
fn init_first_node_reports_supervised_product_roles() {
    let node_id = NodeId::try_new("node_1").expect("valid node id");
    let output = FirstNodeInitOutput {
        node_id: node_id.clone(),
        gateway: FirstNodeGateway::Skip,
    }
    .render();

    assert_eq!(
        output,
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node\n"
    );
    assert_eq!(
        first_node_process_set(&node_id, FirstNodeGateway::Skip).roles(),
        &[
            ployz_core::roles::DaemonProcessRole::Tunnel(ployz_core::roles::TunnelSide::Core),
            ployz_core::roles::DaemonProcessRole::Control,
            ployz_core::roles::DaemonProcessRole::Node(node_id),
        ]
    );
}

#[test]
fn init_first_node_can_include_gateway_role() {
    let output = FirstNodeInitOutput {
        node_id: NodeId::try_new("node_1").expect("valid node id"),
        gateway: FirstNodeGateway::Install,
    }
    .render();

    assert_eq!(
        output,
        "init first node node_1\nsupervise nats-server\nsupervise roles tunnel-core control node gateway\n"
    );
}

#[test]
fn deploy_detach_prints_operation_id_without_runtime_details() {
    let output = DetachedDeployOutput {
        accepted: accepted_operation("op_deploy"),
    }
    .render();

    assert_eq!(
        output,
        "operation op_deploy\nwatch ployzctl ops watch op_deploy\n"
    );
}

#[test]
fn ops_watch_renders_persisted_operation_events() {
    let output = WatchOutput {
        events: vec![
            replayed(
                1,
                ployz_core::ops::OperationEvent::DeploySubmitted {
                    operation_id: operation_id("op_123"),
                    target: deploy_request(),
                },
            ),
            replayed(
                2,
                ployz_core::ops::OperationEvent::DeployCompleted {
                    operation_id: operation_id("op_123"),
                },
            ),
        ],
    }
    .render();

    assert_eq!(output, "1 deploy.submitted\n2 deploy.completed\n");
}

#[test]
fn ops_watch_renders_no_output_when_no_events_are_replayed() {
    let output = WatchOutput { events: Vec::new() }.render();

    assert_eq!(output, "");
}

#[test]
fn machine_add_prints_bootstrap_command_without_nats_credentials() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    }
    .render();

    assert!(output.contains("operation op_machine"));
    assert!(output.contains("node node_2"));
    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh'"));
    assert!(output.contains("--join-token 'join_once_123'"));
    assert!(!output.contains("nats"));
    assert!(!output.contains("creds"));
}

#[test]
fn machine_add_debug_redacts_join_token() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
            .expect("valid bootstrap url"),
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    };

    let debug = format!("{output:?}");

    assert!(debug.contains("[redacted]"));
    assert!(!debug.contains("join_once_123"));
}

#[test]
fn machine_add_shell_quotes_join_material() {
    let output = MachineAddOutput {
        node_id: NodeId::try_new("node_2").expect("valid node id"),
        accepted: accepted_operation("op_machine"),
        bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh/bootstrap?x='quoted'")
            .expect("valid bootstrap url"),
        join_token: MachineJoinToken::try_new("join'quoted'").expect("valid join token"),
    }
    .render();

    assert!(output.contains("curl -fsSL -- 'https://get.ployz.sh/bootstrap?x='\\''quoted'\\'''"));
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
fn upgrade_ployzd_reports_rollout_operation() {
    let output = UpgradeOutput {
        component: UpgradeComponent::Ployzd,
        version: "0.2.0".to_owned(),
        accepted: accepted_operation("op_upgrade"),
    }
    .render();

    assert_eq!(
        output,
        "operation op_upgrade\nupgrade ployzd 0.2.0\nwatch ployzctl ops watch op_upgrade\n"
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

fn replayed(sequence: u64, event: ployz_core::ops::OperationEvent) -> ReplayedOperationEvent {
    ReplayedOperationEvent {
        sequence: event_sequence(sequence),
        event,
    }
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        service_id: ServiceId::try_new("svc_api").expect("valid service id"),
        target_revision: RevisionId::try_new("rev_2").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}
