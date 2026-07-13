//! Authenticated-connection contract against a real TLS + NKey-authorized
//! `nats-server`: valid credentials connect, invalid credentials are
//! rejected, subject permissions are enforced, and per-principal inbox
//! prefixes make reply sniffing impossible.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use futures_util::StreamExt;
use ployz_core::ids::MachineId;
use ployz_core::nats_config::MintedNatsUser;
use ployz_core::permissions::{inbox_prefix, inbox_subscribe_scope};
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{
    INTENT_CHANGED, INTENT_GET, MachineServiceEndpoint, OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
    PENDING_MACHINE_JOINS_CHANGED, gateway_status, gateway_status_scope, machine_container_facts,
    machine_facts, machine_facts_scope, machine_service, machine_service_command_scope,
    machine_service_query_scope,
};
use ployz_nats::connect::{
    NatsConnectConfig, authenticated_connect_options, connect_authenticated,
};
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
async fn extra_cloud_operator_public_key_can_connect_as_operator() {
    let cloud_user = MintedNatsUser::generate().expect("cloud nkey mints");
    let fixture = SecuredTestNats::start_with_machines_and_extra_users(
        &[],
        std::slice::from_ref(&cloud_user.public),
    )
    .await
    .expect("secured fixture");

    let client = connect_authenticated(
        &fixture.config_with_seed(NatsPrincipal::Operator, cloud_user.seed),
        CONNECT_TIMEOUT,
    )
    .await
    .expect("external cloud operator key connects as Operator principal");

    client.flush().await.expect("cloud operator round-trips");
}

#[tokio::test]
async fn operator_can_read_intent_and_subscribe_runtime_broadcasts_only() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let (operator, mut events) = connect_with_event_capture(&fixture.user_config()).await;

    let _machine_facts = operator
        .subscribe(machine_facts_scope())
        .await
        .expect("operator subscribes machine testimony");
    let _gateway_status = operator
        .subscribe(gateway_status_scope())
        .await
        .expect("operator subscribes gateway testimony");
    let _intent_changed = operator
        .subscribe(INTENT_CHANGED)
        .await
        .expect("operator subscribes intent invalidation");
    operator
        .publish(INTENT_GET, "read intent".into())
        .await
        .expect("operator publishes intent read request");
    operator.flush().await.expect("flush allowed subjects");
    assert_no_permission_violation(&mut events).await;

    let _unrelated_signal = operator
        .subscribe(PENDING_MACHINE_JOINS_CHANGED)
        .await
        .expect("subscribe call is accepted client-side");
    operator.flush().await.expect("flush denied subject");

    let violation = next_permission_violation(&mut events).await;
    assert!(
        violation.contains("Subscription"),
        "expected a subscription violation, got: {violation}"
    );
}

#[tokio::test]
async fn plaintext_connect_to_tls_port_fails() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let mut plaintext = TcpStream::connect(("127.0.0.1", fixture.port()))
        .expect("plaintext TCP reaches the listener");
    plaintext
        .set_read_timeout(Some(NO_DELIVERY_WINDOW))
        .expect("plaintext read timeout configures");
    plaintext
        .set_write_timeout(Some(NO_DELIVERY_WINDOW))
        .expect("plaintext write timeout configures");

    let mut info_line = String::new();
    BufReader::new(&plaintext)
        .read_line(&mut info_line)
        .expect("NATS listener sends its INFO prelude");
    let info = info_line
        .strip_prefix("INFO ")
        .expect("NATS prelude starts with INFO");
    let info: serde_json::Value =
        serde_json::from_str(info.trim()).expect("NATS INFO payload is JSON");
    assert_eq!(
        info.get("tls_required").and_then(|value| value.as_bool()),
        Some(true)
    );

    plaintext
        .write_all(b"CONNECT {\"verbose\":false}\r\nPING\r\n")
        .expect("plaintext NATS handshake writes");
    let mut response = [0; 4];
    let read = plaintext.read_exact(&mut response);
    assert!(
        read.is_err() || response != *b"PONG",
        "TLS-required listener accepted a plaintext NATS session"
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
async fn machine_fact_writes_are_fenced_to_its_own_subjects() {
    let machine_id = MachineId::try_new("machine-a").expect("valid machine id");
    let other_machine_id = MachineId::try_new("machine-b").expect("valid machine id");
    let fixture = SecuredTestNats::start_with_machines(std::slice::from_ref(&machine_id))
        .await
        .expect("secured fixture");
    let Some(config) = fixture.machine_config(&machine_id) else {
        panic!("fixture mints a user for every requested machine");
    };
    let (machine_client, mut events) = connect_with_event_capture(&config).await;

    machine_client
        .publish(machine_facts(&machine_id), "facts".into())
        .await
        .expect("machine publishes its own facts");
    machine_client
        .publish(machine_container_facts(&machine_id), "delta".into())
        .await
        .expect("machine publishes its own fact delta");
    machine_client
        .publish(gateway_status(&machine_id), "status".into())
        .await
        .expect("machine publishes its own gateway status");
    machine_client.flush().await.expect("flush");
    assert_no_permission_violation(&mut events).await;

    machine_client
        .publish(machine_container_facts(&other_machine_id), "delta".into())
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
async fn controller_can_serve_operator_rpc_subjects() {
    let fixture = SecuredTestNats::start().await.expect("secured fixture");
    let server_config = fixture.controller_config();
    let caller_config = fixture.user_config();
    let server_client = connect_authenticated(&server_config, CONNECT_TIMEOUT)
        .await
        .expect("controller service connects");
    let caller_client = connect_authenticated(&caller_config, CONNECT_TIMEOUT)
        .await
        .expect("operator caller connects");
    let mut requests = server_client
        .subscribe(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE)
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
        caller_client.request(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE, "activate".into()),
    )
    .await
    .expect("request does not hang")
    .expect("operator receives the API response");

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
        .subscribe(machine_service_query_scope(&machine_id))
        .await
        .expect("machine subscribes its machine query service scope");
    let _machine_command_rpc = machine_client
        .subscribe(machine_service_command_scope(&machine_id))
        .await
        .expect("machine subscribes its machine command service scope");
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
        .publish(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE, "activate".into())
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
        .subscribe(machine_service_query_scope(&machine_id))
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
    let observed = tokio::time::timeout(NO_DELIVERY_WINDOW, async {
        tokio::select! {
            message = sniff_shared.next() => ("_INBOX.>", message),
            message = sniff_controller.next() => (controller_scope.as_str(), message),
            message = sniff_machine.next() => (machine_scope.as_str(), message),
        }
    })
    .await;
    match observed {
        Err(_) => {}
        Ok((scope, Some(message))) => panic!(
            "join sniffer on {scope} received request-reply traffic on {}",
            message.subject
        ),
        Ok((scope, None)) => {
            panic!("join sniffer on {scope} closed before the observation window elapsed")
        }
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
            let event = events.recv().await?;
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
