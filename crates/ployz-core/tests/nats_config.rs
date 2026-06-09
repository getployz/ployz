use std::path::PathBuf;

use ployz_core::ids::NodeId;
use ployz_core::nats_config::{
    NatsServerConfig, NatsServerConfigError, trusted_nats_for_first_node,
};

#[test]
fn single_node_nats_config_renders_loopback_jetstream() {
    let config =
        NatsServerConfig::single_node(node_id("core_1"), PathBuf::from("/var/lib/ployz/nats"))
            .expect("single-node nats config is valid");

    assert_eq!(config.host(), "127.0.0.1");
    assert_eq!(config.port(), 4222);
    assert_eq!(
        config.render(),
        "server_name: core_1\nhost: 127.0.0.1\nport: 4222\njetstream {\n  store_dir: \"/var/lib/ployz/nats\"\n}\n"
    );
    assert_eq!(
        config.sha256_digest().as_str(),
        "5be25a6dfbc6a4b45598f1d128dd2230e5109575018b8826e36d2883102f6ec2"
    );
    assert_eq!(
        trusted_nats_for_first_node(node_id("core_1")).config_sha256,
        config.sha256_digest()
    );
}

#[test]
fn single_node_nats_config_escapes_store_dir() {
    let config = NatsServerConfig::single_node(
        node_id("core_1"),
        PathBuf::from("/var/lib/ployz/nats \"quoted\" \\ path"),
    )
    .expect("single-node nats config is valid");

    assert!(
        config
            .render()
            .contains("store_dir: \"/var/lib/ployz/nats \\\"quoted\\\" \\\\ path\"")
    );
}

#[test]
fn single_node_nats_config_requires_absolute_store_dir() {
    assert_eq!(
        NatsServerConfig::single_node(node_id("core_1"), PathBuf::from("relative/nats")),
        Err(NatsServerConfigError::InvalidPath {
            field: "jetstream_store_dir",
            value: PathBuf::from("relative/nats"),
        })
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
