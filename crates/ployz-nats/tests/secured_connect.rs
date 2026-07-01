//! Authenticated-connection contract against a real TLS + NKey-authorized
//! `nats-server`: valid credentials connect, invalid credentials are
//! rejected, subject permissions are enforced, and per-principal inbox
//! prefixes make reply sniffing impossible.

use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::ids::MachineId;
use ployz_core::nats_config::MintedNatsUser;
use ployz_core::permissions::{inbox_prefix, inbox_subscribe_scope};
use ployz_core::security::NatsPrincipal;
use ployz_core::state::{MachinePublicIpObservation, MachinePublicIpObservationKey};
use ployz_core::subjects::{
    API_INIT_FIRST_MACHINE_ACTIVATE, MachineServiceEndpoint, machine_service, machine_service_scope,
};
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
async fn extra_cloud_user_public_key_can_connect_as_user() {
    let cloud_user = MintedNatsUser::generate().expect("cloud nkey mints");
    let fixture =
        SecuredTestNats::start_with_machines_and_extra_users(&[], &[cloud_user.public.clone()])
            .await
            .expect("secured fixture");

    let client = connect_authenticated(
        &fixture.config_with_seed(NatsPrincipal::User, cloud_user.seed),
        CONNECT_TIMEOUT,
    )
    .await
    .expect("external cloud user key connects as User principal");

    client.flush().await.expect("cloud user round-trips");
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
async fn controller_can_publish_to_its_own_inbox_without_permission_violation() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (controller, mut events) = connect_with_event_capture(&fixture.controller_config()).await;

    controller
        .publish(
            format!("{}.test", inbox_prefix(&NatsPrincipal::Controller)),
            "service reply".into(),
        )
        .await
        .expect("publish accepted client-side");
    controller.flush().await.expect("flush");

    assert_no_permission_violation(&mut events).await;
}

#[tokio::test]
async fn machine_publish_outside_allow_list_gets_permission_violation() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };

    let (client, mut events) = connect_with_event_capture(&config).await;
    client
        .publish(
            "plz.v1.obs.machine.other-machine.started",
            "evidence".into(),
        )
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
async fn machine_can_publish_to_its_own_inbox_without_permission_violation() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine, mut events) = connect_with_event_capture(&config).await;

    machine
        .publish(
            format!(
                "{}.test",
                inbox_prefix(&NatsPrincipal::Machine {
                    machine_id: machine_id.clone(),
                })
            ),
            "service reply".into(),
        )
        .await
        .expect("publish accepted client-side");
    machine.flush().await.expect("flush");

    assert_no_permission_violation(&mut events).await;
}

#[tokio::test]
async fn machine_observation_writes_are_fenced_to_its_own_keys() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let other_machine_id = MachineId::try_new("machine-b").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
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
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine_client, mut events) = connect_with_event_capture(&config).await;
    let observations = AsyncNatsObservationStore::from_jetstream(&async_nats::jetstream::new(
        machine_client.clone(),
    ))
    .await
    .expect("machine opens the observation store");

    // Writing this machine's own observation key succeeds.
    observations
        .replace_machine_public_ip(&MachinePublicIpObservation {
            machine_id: machine_id.clone(),
            public_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, 1)),
        })
        .await
        .expect("a machine writes its own observation key");

    // Writing another machine's observation key is denied server-side.
    let other_key = MachinePublicIpObservationKey::from_machine_id(&other_machine_id);
    machine_client
        .publish(
            format!("$KV.{KV_OBS_BUCKET}.{}", other_key.as_str()),
            "evidence".into(),
        )
        .await
        .expect("publish is accepted client-side");
    machine_client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn controller_can_serve_and_call_operation_api_subjects() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let server_config = fixture.controller_config();
    let caller_config = fixture.controller_config();
    let server_client = connect_authenticated(&server_config, CONNECT_TIMEOUT)
        .await
        .expect("controller service connects");
    let caller_client = connect_authenticated(&caller_config, CONNECT_TIMEOUT)
        .await
        .expect("controller caller connects");
    let mut requests = server_client
        .subscribe(API_INIT_FIRST_MACHINE_ACTIVATE)
        .await
        .expect("controller subscribes API endpoint subject");
    server_client.flush().await.expect("flush");
    let responder = server_client.clone();
    tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            responder.publish(reply, "activated".into()).await.ok();
            responder.flush().await.ok();
        }
    });

    let response = tokio::time::timeout(
        EVENT_TIMEOUT,
        caller_client.request(API_INIT_FIRST_MACHINE_ACTIVATE, "activate".into()),
    )
    .await
    .expect("request does not hang")
    .expect("controller receives the API response");

    assert_eq!(response.payload.as_ref(), b"activated");
}

#[tokio::test]
async fn machine_can_serve_machine_rpc_and_service_discovery_subjects() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine_client, mut events) = connect_with_event_capture(&config).await;

    let _machine_rpc = machine_client
        .subscribe(machine_service_scope(&machine_id))
        .await
        .expect("machine subscribes its machine service scope");
    let _service_ping = machine_client
        .subscribe("$SRV.PING")
        .await
        .expect("machine subscribes service ping discovery");
    let _service_info = machine_client
        .subscribe("$SRV.INFO.plz-machine")
        .await
        .expect("machine subscribes service info discovery");
    machine_client.flush().await.expect("flush");

    assert_no_permission_violation(&mut events).await;
}

#[tokio::test]
async fn join_cannot_publish_general_operation_api_subjects() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (join_client, mut events) = connect_with_event_capture(&fixture.join_config()).await;
    join_client
        .publish(API_INIT_FIRST_MACHINE_ACTIVATE, "activate".into())
        .await
        .expect("publish is accepted client-side");
    join_client.flush().await.expect("flush");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Publish"),
        "expected a publish violation, got: {violation}"
    );
}

#[tokio::test]
async fn join_cannot_sniff_other_principals_inboxes() {
    let machine_id = MachineId::try_new("machine-x").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");

    // Join sniffer: subscribe the shared legacy inbox scope and other
    // principals' prefixes. The server must reject every one of them.
    let join_config = fixture.join_config();
    let (join_client, mut join_events) = connect_with_event_capture(&join_config).await;
    let machine_principal = NatsPrincipal::Machine {
        machine_id: machine_id.clone(),
    };
    let controller_scope = inbox_subscribe_scope(&NatsPrincipal::Controller);
    let machine_scope = inbox_subscribe_scope(&machine_principal);
    let mut sniff_shared = join_client
        .subscribe("_INBOX.>")
        .await
        .expect("subscribe call is accepted client-side");
    let mut sniff_controller = join_client
        .subscribe(controller_scope.clone())
        .await
        .expect("subscribe call is accepted client-side");
    let mut sniff_machine = join_client
        .subscribe(machine_scope.clone())
        .await
        .expect("subscribe call is accepted client-side");
    join_client.flush().await.expect("flush");

    for _ in [
        "_INBOX.>",
        controller_scope.as_str(),
        machine_scope.as_str(),
    ] {
        let violation = next_permission_violation(&mut join_events).await;
        assert!(
            violation.contains("Subscription"),
            "expected a subscription violation, got: {violation}"
        );
    }

    // Machine responder for the controller's request.
    let Some(machine_config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let machine_client = connect_authenticated(&machine_config, CONNECT_TIMEOUT)
        .await
        .expect("machine connects");
    let request_subject = machine_service(&machine_id, MachineServiceEndpoint::Inspect);
    let mut requests = machine_client
        .subscribe(machine_service_scope(&machine_id))
        .await
        .expect("machine subscribes its service scope");
    machine_client.flush().await.expect("flush");
    let responder = machine_client.clone();
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
    .expect("controller receives the machine's reply");
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
        (machine_scope.as_str(), &mut sniff_machine),
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

async fn assert_no_permission_violation(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<async_nats::Event>,
) {
    let observed = tokio::time::timeout(NO_DELIVERY_WINDOW, async {
        loop {
            let Some(event) = events.recv().await else {
                return None;
            };
            if let async_nats::Event::ServerError(async_nats::ServerError::Other(message)) = event
                && message
                    .to_ascii_lowercase()
                    .contains("permissions violation")
            {
                return Some(message);
            }
        }
    })
    .await;

    match observed {
        Err(_) | Ok(None) => {}
        Ok(Some(message)) => panic!("unexpected permission violation: {message}"),
    }
}
