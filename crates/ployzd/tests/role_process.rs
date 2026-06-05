use ployz_core::ids::NodeId;
use ployz_core::subjects::API_OPS_STATUS;
use ployzd::app::{ControlWork, DnsWork, GatewayWork, RoleProcessPlan, TunnelWork, plan_process};
use ployzd::role::{DaemonProcessRole, TunnelSide, parse_role_args};

#[test]
fn control_process_owns_api_and_nats_assurance() {
    let RoleProcessPlan::Control(plan) = plan_process(&DaemonProcessRole::Control) else {
        panic!("control role should produce a control process plan");
    };

    assert_eq!(
        plan.work,
        &[
            ControlWork::AssureNatsResources,
            ControlWork::ServeOperationApi
        ]
    );
    assert_eq!(service_names(&plan.service_catalog), vec!["plz-api"]);
    assert!(plan.service_catalog.has_endpoint_subject(API_OPS_STATUS));
}

#[test]
fn node_process_owns_node_rpc_and_observations_only() {
    let node_id = node_id("node_7");
    let RoleProcessPlan::Node(plan) = plan_process(&DaemonProcessRole::Node(node_id.clone()))
    else {
        panic!("node role should produce a node process plan");
    };

    assert_eq!(plan.node_id, node_id);
    assert_eq!(
        plan.work,
        &[
            ployzd::app::NodeWork::ServeNodeRpc,
            ployzd::app::NodeWork::PublishDockerObservations
        ]
    );
    assert_eq!(service_names(&plan.service_catalog), vec!["plz-node"]);
    assert!(!plan.service_catalog.has_endpoint_subject(API_OPS_STATUS));
}

#[test]
fn gateway_and_dns_are_watchers_not_command_surfaces() {
    let RoleProcessPlan::Gateway(gateway) = plan_process(&DaemonProcessRole::Gateway) else {
        panic!("gateway role should produce a gateway process plan");
    };
    let RoleProcessPlan::Dns(dns) = plan_process(&DaemonProcessRole::Dns) else {
        panic!("dns role should produce a dns process plan");
    };

    assert_eq!(
        gateway.work,
        &[
            GatewayWork::WatchRoutes,
            GatewayWork::WatchContainerHealth,
            GatewayWork::ServeLastKnownGoodRoutes
        ]
    );
    assert_eq!(
        dns.work,
        &[
            DnsWork::WatchServices,
            DnsWork::WatchNodeAddresses,
            DnsWork::ServeLastKnownGoodAnswers
        ]
    );
}

#[test]
fn tunnel_side_decides_byte_transport_work() {
    let RoleProcessPlan::Tunnel(edge) = plan_process(&DaemonProcessRole::Tunnel(TunnelSide::Edge))
    else {
        panic!("edge tunnel role should produce a tunnel process plan");
    };
    let RoleProcessPlan::Tunnel(core) = plan_process(&DaemonProcessRole::Tunnel(TunnelSide::Core))
    else {
        panic!("core tunnel role should produce a tunnel process plan");
    };

    assert_eq!(
        edge.work,
        &[TunnelWork::ExposeLoopbackNats, TunnelWork::OpenIrohToCore]
    );
    assert_eq!(
        core.work,
        &[
            TunnelWork::AcceptIrohFromEdges,
            TunnelWork::ForwardCoreNatsBytes
        ]
    );
}

#[test]
fn role_parser_accepts_the_supervisor_process_commands() {
    assert_eq!(
        parse_role_args(["control"].map(str::to_owned)),
        Ok(DaemonProcessRole::Control)
    );
    assert_eq!(
        parse_role_args(["node", "--id", "node_7"].map(str::to_owned)),
        Ok(DaemonProcessRole::Node(node_id("node_7")))
    );
    assert_eq!(
        parse_role_args(["gateway"].map(str::to_owned)),
        Ok(DaemonProcessRole::Gateway)
    );
    assert_eq!(
        parse_role_args(["dns"].map(str::to_owned)),
        Ok(DaemonProcessRole::Dns)
    );
    assert_eq!(
        parse_role_args(["tunnel", "--side", "edge"].map(str::to_owned)),
        Ok(DaemonProcessRole::Tunnel(TunnelSide::Edge))
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn service_names(catalog: &ployzd::services::DaemonServiceCatalog) -> Vec<&str> {
    catalog
        .services()
        .iter()
        .map(|service| service.name)
        .collect()
}
