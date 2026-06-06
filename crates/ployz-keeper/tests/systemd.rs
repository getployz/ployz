use std::path::PathBuf;

use ployz_core::ids::NodeId;
use ployz_core::roles::{DaemonProcessRole, TunnelSide};
use ployz_keeper::artifacts::{
    ArtifactSource, ArtifactVersion, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::systemd::{PloyzdRoleUnit, role_unit_name};

#[test]
fn role_units_render_the_supervised_ployzd_commands() {
    let node = DaemonProcessRole::Node(node_id("node_7"));
    let tunnel = DaemonProcessRole::Tunnel(TunnelSide::Edge);

    assert_eq!(role_unit_name(&node), "ployzd-node-node_7.service");
    assert_eq!(node.command_args(), ["node", "--id", "node_7"]);
    assert_eq!(role_unit_name(&tunnel), "ployzd-tunnel-edge.service");
    assert_eq!(tunnel.command_args(), ["tunnel", "--side", "edge"]);

    let node_unit = PloyzdRoleUnit::new(node, &ployzd_artifact()).expect("node unit is valid");
    assert_eq!(node_unit.unit_name(), "ployzd-node-node_7.service");
    assert_eq!(
        node_unit.render(),
        "[Unit]\nDescription=Ployz node\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart=/usr/local/bin/ployzd node --id node_7\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );

    let tunnel_unit =
        PloyzdRoleUnit::new(tunnel, &ployzd_artifact()).expect("tunnel unit is valid");
    assert_eq!(tunnel_unit.unit_name(), "ployzd-tunnel-edge.service");
    assert_eq!(
        tunnel_unit.render(),
        "[Unit]\nDescription=Ployz tunnel-edge\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart=/usr/local/bin/ployzd tunnel --side edge\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

#[test]
fn role_units_quote_paths_that_need_systemd_escaping() {
    let spaced_path_artifact = PloyzdArtifactTarget::new(
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/opt/Ployz Tools/ployzd"),
    )
    .expect("artifact install paths are not systemd policy");
    let percent_path_artifact = PloyzdArtifactTarget::new(
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/opt/ployz%tools/ployzd"),
    )
    .expect("artifact install paths are not systemd policy");

    assert_eq!(
        PloyzdRoleUnit::new(DaemonProcessRole::Gateway, &spaced_path_artifact)
            .expect("spaced path can be quoted")
            .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart=\"/opt/Ployz Tools/ployzd\" gateway\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
    assert_eq!(
        PloyzdRoleUnit::new(DaemonProcessRole::Gateway, &percent_path_artifact)
            .expect("percent path can be escaped")
            .render(),
        "[Unit]\nDescription=Ployz gateway\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nType=simple\nExecStart=/opt/ployz%%tools/ployzd gateway\nRestart=always\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n"
    );
}

fn ployzd_artifact() -> PloyzdArtifactTarget {
    PloyzdArtifactTarget::new(
        version("0.1.0"),
        source("https://example.invalid/ployzd"),
        digest(PLOYZD_DIGEST),
        PathBuf::from("/usr/local/bin/ployzd"),
    )
    .expect("valid ployzd artifact")
}

fn version(value: &str) -> ArtifactVersion {
    ArtifactVersion::try_new(value).expect("valid artifact version")
}

fn source(value: &str) -> ArtifactSource {
    ArtifactSource::try_new(value).expect("valid artifact source")
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid artifact digest")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

const PLOYZD_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
