use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use ployz_core::MachineId;

use super::*;
use crate::{
    machine::LocalMachineStore,
    observe::ship::{
        crash::Outbox,
        fixtures::{crash, held, store},
    },
};

const KAT_MACHINE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn seeded(byte: u8) -> ManagementSecret {
    serde_json::from_value(serde_json::to_value([byte; 32]).unwrap()).unwrap()
}

fn pusher(url: &Url) -> Pusher {
    Pusher {
        url: url.clone(),
        machine_id: MachineId::parse(KAT_MACHINE).unwrap(),
        secret: seeded(7),
    }
}

#[test]
fn backoff_doubles_to_its_cap_honors_retry_after_and_resets() {
    let mut backoff = Doubling::between(BACKOFF_FLOOR, BACKOFF_CAP);
    let waits: Vec<Duration> = (0..12).map(|_| retry_delay(&mut backoff, None)).collect();
    for (n, wait) in waits.iter().enumerate() {
        let ceiling = (BACKOFF_FLOOR * 2_u32.pow(u32::try_from(n).unwrap())).min(BACKOFF_CAP);
        assert!(
            *wait >= ceiling / 2 && *wait <= ceiling,
            "wait {n} was {wait:?}"
        );
    }
    assert!(waits.last().is_some_and(|wait| *wait >= BACKOFF_CAP / 2));
    assert_eq!(
        retry_delay(&mut backoff, Some(Duration::from_secs(7))),
        Duration::from_secs(7)
    );
    assert_eq!(
        retry_delay(&mut backoff, Some(Duration::from_secs(3600))),
        Duration::from_secs(3600)
    );
    assert_eq!(
        retry_delay(&mut backoff, Some(Duration::from_secs(86_400))),
        RETRY_AFTER_CAP
    );
    backoff.reset();
    assert!(retry_delay(&mut backoff, None) <= BACKOFF_FLOOR);
}

type Reply = (StatusCode, Option<&'static str>);

#[derive(Clone, Default)]
struct Cloud {
    replies: Arc<Mutex<Vec<Reply>>>,
    bodies: Arc<Mutex<Vec<serde_json::Value>>>,
    requests: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>,
}

impl Cloud {
    async fn serve(replies: Vec<Reply>) -> (Self, Url) {
        let cloud = Self {
            replies: Arc::new(Mutex::new(replies.into_iter().rev().collect())),
            bodies: Arc::default(),
            requests: Arc::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/push", post(Self::push))
            .with_state(cloud.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (cloud, format!("http://{address}/push").parse().unwrap())
    }

    async fn push(
        State(cloud): State<Self>,
        headers: HeaderMap,
        body: Bytes,
    ) -> axum::response::Response {
        cloud
            .bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&body).unwrap());
        cloud.requests.lock().unwrap().push((headers, body));
        let (status, extra) = cloud.replies.lock().unwrap().pop().unwrap();
        if status == StatusCode::OK {
            let json = [(CONTENT_TYPE, "application/json")];
            return (status, json, extra.unwrap_or_default()).into_response();
        }
        let mut response = status.into_response();
        if let Some(seconds) = extra {
            response
                .headers_mut()
                .insert(RETRY_AFTER, seconds.parse().unwrap());
        }
        response
    }

    fn bodies(&self) -> Vec<serde_json::Value> {
        self.bodies.lock().unwrap().clone()
    }

    fn requests(&self) -> Vec<(HeaderMap, Bytes)> {
        self.requests.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn entries_leave_the_outbox_only_when_cloud_acks_them() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    outbox.push(crash(9)).unwrap();
    let (cloud, url) = Cloud::serve(vec![
        (StatusCode::SERVICE_UNAVAILABLE, None),
        (StatusCode::UNAUTHORIZED, None),
        (StatusCode::NOT_FOUND, None),
        (StatusCode::NO_CONTENT, None),
    ])
    .await;
    let shipper = Shipper::new(outbox.clone(), move || Some(pusher(&url))).unwrap();

    for _ in 0..3 {
        assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
        assert_eq!(held(&outbox), [5, 9]);
    }
    assert_eq!(shipper.ship_once().await, Shipped::Progress);
    assert_eq!(held(&outbox), [] as [u64; 0]);
    assert_eq!(shipper.ship_once().await, Shipped::Idle);

    let bodies = cloud.bodies();
    assert_eq!(bodies.len(), 4);
    assert_eq!(
        bodies.last(),
        Some(&serde_json::json!({
            "stream": "crash",
            "seq": "9",
            "dropped": 0,
            "events": [
                { "seq": "5", "marker": serde_json::to_value(crash(5)).unwrap() },
                { "seq": "9", "marker": serde_json::to_value(crash(9)).unwrap() },
            ],
        }))
    );
}

#[tokio::test]
async fn cloud_turning_the_machine_away_warns_once_every_few_minutes() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let (_cloud, url) = Cloud::serve(vec![
        (StatusCode::UNAUTHORIZED, None),
        (StatusCode::NOT_FOUND, None),
        (StatusCode::UNAUTHORIZED, None),
    ])
    .await;
    let shipper = Shipper::new(outbox.clone(), move || Some(pusher(&url))).unwrap();
    let (journal, _journal) = crate::diag::test_support::capture();

    shipper.ship_once().await;
    shipper.ship_once().await;
    let warned = shipper.unheard_warned.lock().unwrap().unwrap();
    *shipper.unheard_warned.lock().unwrap() = warned.checked_sub(UNHEARD_WARN_EVERY);
    shipper.ship_once().await;

    let journal = journal.text();
    let warnings: Vec<&str> = journal
        .lines()
        .filter(|line| line.contains("WARN"))
        .collect();
    assert_eq!(warnings.len(), 2, "{journal}");
    assert!(
        warnings.iter().all(|line| line.contains("status=401")),
        "{journal}"
    );
    assert_eq!(held(&outbox), [5]);
}

#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn a_batch_cloud_refuses_is_dropped_and_counted_and_the_next_one_ships() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    let (cloud, url) = Cloud::serve(vec![
        (StatusCode::IM_A_TEAPOT, None),
        (StatusCode::BAD_REQUEST, None),
        (StatusCode::PAYLOAD_TOO_LARGE, None),
        (StatusCode::UNPROCESSABLE_ENTITY, None),
        (StatusCode::NO_CONTENT, None),
    ])
    .await;
    let shipper = Shipper::new(outbox.clone(), move || Some(pusher(&url))).unwrap();

    outbox.push(crash(5)).unwrap();
    assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
    assert_eq!(held(&outbox), [5]);
    for ts in 6..=8 {
        outbox.push(crash(ts)).unwrap();
        assert_eq!(shipper.ship_once().await, Shipped::Progress);
        assert_eq!(held(&outbox), [] as [u64; 0]);
    }
    outbox.push(crash(9)).unwrap();
    assert_eq!(shipper.ship_once().await, Shipped::Progress);
    assert_eq!(held(&outbox), [] as [u64; 0]);

    let bodies = cloud.bodies();
    assert_eq!(bodies.len(), 5);
    assert_eq!(bodies[4]["dropped"], 4);
    assert_eq!(bodies[4]["events"][0]["seq"], "9");
}

#[tokio::test]
async fn rate_limits_and_outages_keep_the_batch_and_pass_on_retry_after() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let (_cloud, url) = Cloud::serve(vec![
        (StatusCode::TOO_MANY_REQUESTS, Some("12")),
        (StatusCode::TOO_MANY_REQUESTS, None),
        (StatusCode::BAD_GATEWAY, Some("3")),
    ])
    .await;
    let shipper = Shipper::new(outbox.clone(), move || Some(pusher(&url))).unwrap();

    assert_eq!(
        shipper.ship_once().await,
        Shipped::Retry {
            after: Some(Duration::from_secs(12))
        }
    );
    assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
    assert_eq!(
        shipper.ship_once().await,
        Shipped::Retry {
            after: Some(Duration::from_secs(3))
        }
    );
    assert_eq!(held(&outbox), [5]);
}

#[tokio::test]
async fn a_network_error_keeps_the_batch() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url: Url = format!("http://{}/push", listener.local_addr().unwrap())
        .parse()
        .unwrap();
    drop(listener);
    let shipper = Shipper::new(outbox.clone(), move || Some(pusher(&url))).unwrap();
    assert_eq!(shipper.ship_once().await, Shipped::Retry { after: None });
    assert_eq!(held(&outbox), [5]);
}

#[tokio::test]
async fn without_a_target_the_outbox_only_buffers() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let data = tempfile::tempdir().unwrap();
    let data_dir = data.path().to_path_buf();
    let shipper = Shipper::new(outbox.clone(), move || push_target(&data_dir)).unwrap();
    assert_eq!(shipper.ship_once().await, Shipped::Idle);
    drop(LocalMachineStore::open(data.path()).unwrap());
    assert_eq!(shipper.ship_once().await, Shipped::Idle);
    assert_eq!(held(&outbox), [5]);
}

#[test]
fn the_target_follows_machine_json_on_every_read() {
    let data = tempfile::tempdir().unwrap();
    let url: Url = "https://cloud.example/api/machines/m/observe"
        .parse()
        .unwrap();
    let mut machine = LocalMachineStore::open(data.path()).unwrap();
    assert!(push_target(data.path()).is_none());

    machine.set_push_target(Some(url.clone())).unwrap();
    let pusher = push_target(data.path()).unwrap();
    assert_eq!(pusher.url, url);
    assert_eq!(pusher.machine_id, machine.record().id());
    assert_eq!(
        pusher.secret.public_key(),
        machine.record().management_secret().public_key()
    );

    machine.set_push_target(None).unwrap();
    assert!(push_target(data.path()).is_none());
}

#[test]
fn the_signature_matches_the_shared_known_answer_vector() {
    let secret = seeded(7);
    assert_eq!(
        hex::encode(secret.public_key().as_bytes()),
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
    );
    let body = br#"{"stream":"crash","seq":"1760000000123456789","dropped":0,"events":[]}"#;
    assert_eq!(
        signature(
            &secret,
            &MachineId::parse(KAT_MACHINE).unwrap(),
            1_760_000_000,
            body
        ),
        "rOUkHmKtJRTf53MNT4z0t9gV3yOkQnJ_yriYa5G7ebHo5-VrHUa0spxdC56PowBMo9JEMEy_JXT97yIG11RPAw"
    );
}

#[tokio::test]
async fn a_push_is_signed_over_the_exact_bytes_cloud_receives() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let (cloud, url) = Cloud::serve(vec![(StatusCode::NO_CONTENT, None)]).await;
    let shipper = Shipper::new(outbox, move || Some(pusher(&url))).unwrap();
    let before = unix_seconds();
    assert_eq!(shipper.ship_once().await, Shipped::Progress);

    let [(headers, body)] = <[_; 1]>::try_from(cloud.requests()).unwrap();
    let header = |name: &str| headers.get(name).unwrap().to_str().unwrap().to_owned();
    assert_eq!(header("content-type"), "application/json");
    let ts: i64 = header("ployz-timestamp").parse().unwrap();
    assert!((before..=unix_seconds()).contains(&ts));
    let signature = header("ployz-signature");
    let signature = URL_SAFE_NO_PAD
        .decode(signature.strip_prefix("v1=").unwrap())
        .unwrap();

    let mut message = b"ployz-push-v1\0".to_vec();
    message.extend_from_slice(KAT_MACHINE.as_bytes());
    message.push(0);
    message.extend_from_slice(&ts.to_be_bytes());
    message.extend_from_slice(&Sha256::digest(&body));
    let public = iroh::PublicKey::from_bytes(seeded(7).public_key().as_bytes()).unwrap();
    let signature = iroh::Signature::from_bytes(&signature.try_into().unwrap());
    public.verify(&message, &signature).unwrap();

    message.pop();
    message.push(b'!');
    assert!(public.verify(&message, &signature).is_err());
}
