use std::path::PathBuf;

use ployz_core::ids::NodeId;
use ployz_core::nats_config::{NatsServerConfig, NatsServerConfigError};

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
