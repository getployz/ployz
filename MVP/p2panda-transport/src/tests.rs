use std::net::{Ipv4Addr, UdpSocket};

use p2panda_core_git::{SigningKey, Topic};

use crate::{PandaNetBindConfig, PandaNetNode, PandaNetNodeConfig};

#[tokio::test]
async fn owned_nodes_sync_one_opaque_body_with_explicit_bootstrap() {
    let topic: Topic = [42; 32].into();
    let receiver = PandaNetNode::spawn(node_config([1; 32], free_port(), Vec::new()))
        .await
        .expect("spawn receiver node");
    let receiver_info = receiver.node_info();
    let mut receiver_stream = receiver
        .open_stream(topic, true)
        .await
        .expect("open receiver stream");

    let mut sender = PandaNetNode::spawn(node_config([2; 32], free_port(), vec![receiver_info]))
        .await
        .expect("spawn sender node");
    sender
        .append_to_topic(&topic, b"transport-body")
        .await
        .expect("append sender body");
    let _sender_stream = sender
        .open_stream(topic, true)
        .await
        .expect("open sender stream");

    let body = receiver_stream
        .next_body()
        .await
        .expect("receiver gets body");
    assert_eq!(body, b"transport-body");
}

fn node_config(
    seed: [u8; 32],
    port: u16,
    bootstrap_nodes: Vec<crate::PandaNetNodeInfo>,
) -> PandaNetNodeConfig {
    PandaNetNodeConfig {
        network_id: [7; 32],
        signing_key: SigningKey::from_bytes(&seed),
        bind: PandaNetBindConfig {
            ipv4: Ipv4Addr::LOCALHOST,
            port_v4: port,
            ipv6: "::1".parse().expect("valid localhost IPv6"),
            port_v6: free_port(),
        },
        bootstrap_nodes,
    }
}

fn free_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind UDP port probe")
        .local_addr()
        .expect("read UDP port probe")
        .port()
}
