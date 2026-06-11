//! Authenticated-connection contract against a real TLS + NKey-authorized
//! `nats-server`: valid credentials connect, invalid credentials are
//! rejected, subject permissions are enforced, and per-principal inbox
//! prefixes make reply sniffing impossible.

use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::ids::NodeId;
use ployz_core::permissions::{inbox_prefix, inbox_subscribe_scope};
use ployz_core::security::NatsPrincipal;
use ployz_core::state::{NodePublicIpObservation, NodePublicIpObservationKey};
use ployz_core::subjects::{NodeServiceEndpoint, node_service, node_service_scope};
use ployz_nats::connect::{
    NatsClientUrl, NatsConnectConfig, authenticated_connect_options, connect_authenticated,
    connect_with_timeout,
};
use ployz_nats::observations::{AsyncNatsObservationStore, KV_OBS_BUCKET};
use ployz_test_support::nats::SecuredTestNats;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const NO_DELIVERY_WINDOW: Duration = Duration::from_millis(500);

#[tokio::test]
async fn valid_seed_and_ca_connects_over_tls() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");

    let client = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller seed + cluster CA connects");

    assert!(
        client.server_info().tls_required,
        "fixture server must require TLS"
    );
    client
        .flush()
        .await
        .expect("authenticated client round-trips");
}

#[tokio::test]
async fn wrong_seed_is_rejected() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let unauthorized_seed =
        SecuredTestNats::fresh_unauthorized_seed().expect("fresh unauthorized seed");
    let config = fixture.config_with_seed(NatsPrincipal::Join, unauthorized_seed);

    let result = connect_authenticated(&config, CONNECT_TIMEOUT).await;

    assert!(
        result.is_err(),
        "a seed outside the authorized user set must not connect"
    );
}

#[tokio::test]
async fn plaintext_connect_to_tls_port_fails() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let plaintext_url = NatsClientUrl::try_new(format!("nats://127.0.0.1:{}", fixture.port()))
        .expect("valid plaintext URL");

    let result = connect_with_timeout(&plaintext_url, CONNECT_TIMEOUT).await;

    assert!(
        result.is_err(),
        "an anonymous plaintext client must not reach the secured port"
    );
}

#[tokio::test]
async fn node_publish_outside_allow_list_gets_permission_violation() {
    let node_id = NodeId::try_new("node-a").expect("valid node id");
    let fixture = SecuredTestNats::start_with_nodes(std::slice::from_ref(&node_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.node_config(&node_id) else {
        panic!("fixture mints a user for every requested node");
    };

    let (client, mut events) = connect_with_event_capture(&config).await;
    client
        .publish("plz.v1.obs.node.other-node.started", "evidence".into())
        .await
        .expect("publish is accepted client-side");
    client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn node_observation_writes_are_fenced_to_its_own_keys() {
    let node_id = NodeId::try_new("node-a").expect("valid node id");
    let other_node_id = NodeId::try_new("node-b").expect("valid node id");
    let fixture = SecuredTestNats::start_with_nodes(std::slice::from_ref(&node_id))
        .await
        .expect("secured fixture");
    let controller = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller connects");
    async_nats::jetstream::new(controller)
        .create_key_value(async_nats::jetstream::kv::Config {
            bucket: KV_OBS_BUCKET.to_owned(),
            ..Default::default()
        })
        .await
        .expect("controller creates the observation bucket");
    let Some(config) = fixture.node_config(&node_id) else {
        panic!("fixture mints a user for every requested node");
    };
    let (node_client, mut events) = connect_with_event_capture(&config).await;
    let observations =
        AsyncNatsObservationStore::from_jetstream(&async_nats::jetstream::new(node_client.clone()))
            .await
            .expect("node opens the observation store");

    // Writing this node's own observation key succeeds.
    observations
        .replace_node_public_ip(&NodePublicIpObservation {
            node_id: node_id.clone(),
            public_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 1)),
        })
        .await
        .expect("a node writes its own observation key");

    // Writing another node's observation key is denied server-side.
    let other_key = NodePublicIpObservationKey::from_node_id(&other_node_id);
    node_client
        .publish(
            format!("$KV.{KV_OBS_BUCKET}.{}", other_key.as_str()),
            "evidence".into(),
        )
        .await
        .expect("publish is accepted client-side");
    node_client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn join_cannot_sniff_other_principals_inboxes() {
    let node_id = NodeId::try_new("node-x").expect("valid node id");
    let fixture = SecuredTestNats::start_with_nodes(std::slice::from_ref(&node_id))
        .await
        .expect("secured fixture");

    // Join sniffer: subscribe the shared legacy inbox scope and other
    // principals' prefixes. The server must reject every one of them.
    let join_config = fixture.join_config();
    let (join_client, mut join_events) = connect_with_event_capture(&join_config).await;
    let node_principal = NatsPrincipal::Node {
        node_id: node_id.clone(),
    };
    let controller_scope = inbox_subscribe_scope(&NatsPrincipal::Controller);
    let node_scope = inbox_subscribe_scope(&node_principal);
    let mut sniff_shared = join_client
        .subscribe("_INBOX.>")
        .await
        .expect("subscribe call is accepted client-side");
    let mut sniff_controller = join_client
        .subscribe(controller_scope.clone())
        .await
        .expect("subscribe call is accepted client-side");
    let mut sniff_node = join_client
        .subscribe(node_scope.clone())
        .await
        .expect("subscribe call is accepted client-side");
    join_client.flush().await.expect("flush");

    for _ in ["_INBOX.>", controller_scope.as_str(), node_scope.as_str()] {
        let violation = next_permission_violation(&mut join_events).await;
        assert!(
            violation.contains("Subscription"),
            "expected a subscription violation, got: {violation}"
        );
    }

    // Node responder for the controller's request.
    let Some(node_config) = fixture.node_config(&node_id) else {
        panic!("fixture mints a user for every requested node");
    };
    let node_client = connect_authenticated(&node_config, CONNECT_TIMEOUT)
        .await
        .expect("node connects");
    let request_subject = node_service(&node_id, NodeServiceEndpoint::Inspect);
    let mut requests = node_client
        .subscribe(node_service_scope(&node_id))
        .await
        .expect("node subscribes its service scope");
    node_client.flush().await.expect("flush");
    let responder = node_client.clone();
    tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            responder.publish(reply, "pong".into()).await.ok();
            responder.flush().await.ok();
        }
    });

    // Drive a Controller request-reply while the sniffer is subscribed.
    let controller_client = connect_authenticated(&fixture.controller_config(), CONNECT_TIMEOUT)
        .await
        .expect("controller connects");
    let response = tokio::time::timeout(
        EVENT_TIMEOUT,
        controller_client.request(request_subject, "ping".into()),
    )
    .await
    .expect("request does not hang")
    .expect("controller receives the node's reply");
    assert_eq!(response.payload.as_ref(), b"pong");
    assert!(
        response
            .subject
            .as_str()
            .starts_with(&format!("{}.", inbox_prefix(&NatsPrincipal::Controller))),
        "reply must arrive on the controller's own inbox prefix, got {}",
        response.subject.as_str()
    );

    // The Join sniffer observably received none of that traffic.
    for (scope, sniffer) in [
        ("_INBOX.>", &mut sniff_shared),
        (controller_scope.as_str(), &mut sniff_controller),
        (node_scope.as_str(), &mut sniff_node),
    ] {
        let delivery = tokio::time::timeout(NO_DELIVERY_WINDOW, sniffer.next()).await;
        assert!(
            delivery.is_err(),
            "join sniffer on {scope} must not receive request-reply traffic"
        );
    }
}

/// Connects with the exact product option set plus an event capture channel
/// so tests can observe server-side permission violations.
async fn connect_with_event_capture(
    config: &NatsConnectConfig,
) -> (
    async_nats::Client,
    tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
    let client = authenticated_connect_options(config)
        .event_callback(move |event| {
            let events_tx = events_tx.clone();
            async move {
                events_tx.send(event).ok();
            }
        })
        .connect(config.url.as_str())
        .await
        .expect("authenticated connect with event capture");
    (client, events_rx)
}

async fn next_permission_violation(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) -> String {
    tokio::time::timeout(EVENT_TIMEOUT, async {
        loop {
            let Some(event) = events.recv().await else {
                panic!("event channel closed before a permission violation arrived");
            };
            if let async_nats::Event::ServerError(async_nats::ServerError::Other(message)) = event
                && message
                    .to_ascii_lowercase()
                    .contains("permissions violation")
            {
                return message;
            }
        }
    })
    .await
    .expect("server reports a permission violation")
}
