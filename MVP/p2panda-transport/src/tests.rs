use crate::{PandaNetNode, PandaNetNodeConfig, PandaNetTopic};

#[tokio::test]
async fn owned_nodes_sync_one_opaque_body_with_explicit_bootstrap() {
    let topic = PandaNetTopic::new([42; 32]);
    let receiver = PandaNetNode::spawn(
        PandaNetNodeConfig::localhost_ephemeral([7; 32], [1; 32], Vec::new())
            .expect("receiver config"),
    )
    .await
    .expect("spawn receiver node");
    let receiver_info = receiver.node_info();
    let mut receiver_stream = receiver
        .open_stream(topic, true)
        .await
        .expect("open receiver stream");

    let mut sender = PandaNetNode::spawn(
        PandaNetNodeConfig::localhost_ephemeral([7; 32], [2; 32], vec![receiver_info])
            .expect("sender config"),
    )
    .await
    .expect("spawn sender node");
    sender
        .append_to_topic(topic, b"transport-body")
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
