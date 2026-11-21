use axum::{Router, body::Bytes, extract::State, http::HeaderMap, routing::post};
use ployz_core::MachineId;
use std::sync::{Arc, Mutex};

use super::*;
use crate::machine::LocalMachineStore;

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

type Requests = Arc<Mutex<Vec<(HeaderMap, Bytes)>>>;

async fn record(State(requests): State<Requests>, headers: HeaderMap, body: Bytes) -> &'static str {
    requests.lock().unwrap().push((headers, body));
    "{}"
}

#[tokio::test]
async fn a_salts_fetch_is_signed_over_the_exact_bytes_cloud_receives() {
    let requests = Requests::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            &format!("/api/machines/{KAT_MACHINE}/analytics-salts"),
            post(record),
        )
        .with_state(requests.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let url: Url = format!("http://{address}/api/machines/otlp")
        .parse()
        .unwrap();
    let before = unix_seconds();
    pusher(&url)
        .analytics_salts(&reqwest::Client::new())
        .await
        .unwrap();

    let [(headers, body)] = <[_; 1]>::try_from(requests.lock().unwrap().clone()).unwrap();
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
