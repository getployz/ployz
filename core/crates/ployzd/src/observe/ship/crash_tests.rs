use std::sync::{Arc, Mutex};

use axum::{Router, body::Bytes, extract::State, http::StatusCode, routing::post};
use ployz_core::metrics::MarkerKind;
use serde_json::{Value, json};

use super::*;
use crate::observe::ship::fixtures::{crash, held, store};

#[test]
fn seqs_strictly_increase_across_a_restart_even_when_clocks_repeat() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(100)).unwrap();
    outbox.push(crash(100)).unwrap();
    outbox.push(crash(50)).unwrap();
    drop(outbox);
    let reopened = Outbox::open(&store).unwrap();
    reopened.ack_seqs(&held(&reopened)).unwrap();
    reopened.push(crash(90)).unwrap();
    assert_eq!(
        reopened.oldest(CAP).unwrap(),
        [Entry {
            seq: 103,
            marker: crash(90),
        }]
    );
}

#[test]
fn queuing_a_crash_cloud_took_again_drops_no_newer_crash() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    for ts in 1..=i64::try_from(CAP).unwrap() {
        outbox.push(crash(ts)).unwrap();
    }
    let acked = held(&outbox).first().copied().unwrap();
    outbox.ack_seqs(&[acked]).unwrap();
    outbox.push(crash(i64::try_from(CAP).unwrap() + 1)).unwrap();
    assert!(!outbox.push_at(acked, crash(1)).unwrap());
    let held = held(&outbox);
    assert_eq!((held.len(), held.first()), (CAP, Some(&(acked + 1))));
    assert_eq!(outbox.dropped(), 0);
}

#[test]
fn a_full_outbox_drops_its_oldest_entry_and_counts_it_across_a_restart() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    for ts in 1..=i64::try_from(CAP).unwrap() + 2 {
        outbox.push(crash(ts)).unwrap();
    }
    let seqs = held(&outbox);
    assert_eq!(seqs.len(), CAP);
    assert_eq!(seqs.first(), Some(&3));
    assert_eq!(outbox.dropped(), 2);
    drop(outbox);
    assert_eq!(Outbox::open(&store).unwrap().dropped(), 2);
}

#[test]
fn an_ack_keeps_an_entry_rewritten_since_it_was_selected() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    let seq = outbox.reserve(1).unwrap();
    assert!(outbox.push_at(seq, crash(1)).unwrap());
    let selected = outbox.select(CAP).unwrap();
    let rewritten = MetricMarker {
        replica: "web-2".into(),
        ..crash(1)
    };

    assert!(outbox.push_at(seq, rewritten.clone()).unwrap());
    outbox.ack(&selected).unwrap();

    assert_eq!(
        outbox.oldest(CAP).unwrap(),
        [Entry {
            seq,
            marker: rewritten,
        }]
    );
}

#[test]
fn preparing_the_store_keeps_the_outbox() {
    let (_root, store) = store();
    Outbox::open(&store).unwrap().push(crash(7)).unwrap();
    store.prepare().unwrap();
    assert_eq!(held(&Outbox::open(&store).unwrap()), [7]);
}

#[test]
fn a_crash_renders_as_the_otlp_record_cloud_reads() {
    let body: Value = serde_json::from_slice(&render(&[Entry {
        seq: 1,
        marker: crash(1_760_000_000_123_456_789),
    }]))
    .unwrap();
    assert_eq!(
        body,
        json!({
            "resourceLogs": [{
                "scopeLogs": [{
                    "logRecords": [{
                        "timeUnixNano": "1760000000123456789",
                        "observedTimeUnixNano": "1760000000123456789",
                        "severityNumber": 17,
                        "severityText": "ERROR",
                        "body": { "stringValue": "web-1 exited with code 137 (out of memory)" },
                        "eventName": "ployz.container.exit",
                        "attributes": [
                            { "key": "event.name", "value": { "stringValue": "ployz.container.exit" } },
                            { "key": "ployz.event.id", "value": { "stringValue": format!("{}:1760000000123456789", "b".repeat(64)) } },
                            { "key": "ployz.machine.id", "value": { "stringValue": "a".repeat(32) } },
                            { "key": "ployz.namespace", "value": { "stringValue": "prod" } },
                            { "key": "ployz.service.name", "value": { "stringValue": "web" } },
                            { "key": "ployz.service.id", "value": { "stringValue": "c".repeat(32) } },
                            { "key": "container.id", "value": { "stringValue": "b".repeat(64) } },
                            { "key": "ployz.replica", "value": { "stringValue": "web-1" } },
                            { "key": "ployz.deployment.id", "value": { "stringValue": "d1" } },
                            { "key": "ployz.exit.code", "value": { "intValue": "137" } },
                            { "key": "ployz.exit.oom", "value": { "boolValue": true } },
                            { "key": "ployz.exit.reason", "value": { "stringValue": "out_of_memory" } },
                        ],
                    }],
                }],
            }],
        })
    );
}

fn rendered(marker: MetricMarker) -> Value {
    let body: Value = serde_json::from_slice(&render(&[Entry { seq: 1, marker }])).unwrap();
    body.pointer("/resourceLogs/0/scopeLogs/0/logRecords/0")
        .cloned()
        .unwrap()
}

fn attribute_keys(record: &Value) -> Vec<&str> {
    record["attributes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|attribute| attribute["key"].as_str())
        .collect()
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
fn a_crash_without_a_deployment_or_exit_code_omits_those_attributes() {
    let record = rendered(MetricMarker {
        deployment_id: None,
        kind: MarkerKind::Exit {
            exit_code: None,
            oom: true,
            stopped: false,
        },
        ..crash(5)
    });
    assert_eq!(
        record["body"]["stringValue"],
        "web-1 was killed by the out-of-memory killer"
    );
    let keys = attribute_keys(&record);
    assert!(!keys.contains(&"ployz.deployment.id"));
    assert!(!keys.contains(&"ployz.exit.code"));
}

#[test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
fn a_plain_failed_exit_names_its_code_and_is_not_out_of_memory() {
    let record = rendered(MetricMarker {
        kind: MarkerKind::Exit {
            exit_code: Some(1),
            oom: false,
            stopped: false,
        },
        ..crash(5)
    });
    assert_eq!(record["body"]["stringValue"], "web-1 exited with code 1");
    let attributes = record["attributes"].as_array().unwrap();
    assert!(
        attributes.contains(&json!({ "key": "ployz.exit.oom", "value": { "boolValue": false } }))
    );
    assert!(attributes.contains(
        &json!({ "key": "ployz.exit.reason", "value": { "stringValue": "exit_nonzero" } })
    ));
}

#[derive(Clone, Default)]
struct Collector {
    replies: Arc<Mutex<Vec<StatusCode>>>,
    bodies: Arc<Mutex<Vec<Value>>>,
}

impl Collector {
    async fn serve(replies: Vec<StatusCode>) -> (Self, String) {
        let collector = Self {
            replies: Arc::new(Mutex::new(replies.into_iter().rev().collect())),
            bodies: Arc::default(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let app = Router::new()
            .route("/v1/logs", post(Self::logs))
            .with_state(collector.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (collector, address)
    }

    async fn logs(State(collector): State<Self>, body: Bytes) -> StatusCode {
        collector
            .bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&body).unwrap());
        collector.replies.lock().unwrap().pop().unwrap()
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    fn event_ids(&self) -> Vec<Vec<String>> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .map(|body| {
                let records = body
                    .pointer("/resourceLogs/0/scopeLogs/0/logRecords")
                    .and_then(Value::as_array)
                    .unwrap();
                records
                    .iter()
                    .map(|record| {
                        record["attributes"][1]["value"]["stringValue"]
                            .as_str()
                            .unwrap()
                            .to_owned()
                    })
                    .collect()
            })
            .collect()
    }
}

fn sender(outbox: &Arc<Outbox>, address: &str) -> Sender<impl Fn() -> bool> {
    Sender::new(outbox.clone(), address, || true).unwrap()
}

#[tokio::test]
async fn an_unacked_crash_survives_a_failed_post_and_is_posted_again() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    outbox.push(crash(9)).unwrap();
    let (collector, address) = Collector::serve(vec![
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::BAD_REQUEST,
        StatusCode::OK,
    ])
    .await;
    let sender = sender(&outbox, &address);

    assert_eq!(sender.send_once().await, Sent::Retry);
    assert_eq!(sender.send_once().await, Sent::Retry);
    assert_eq!(held(&outbox), [5, 9]);
    assert_eq!(sender.send_once().await, Sent::Progress);
    assert_eq!(held(&outbox), [] as [u64; 0]);
    assert_eq!(sender.send_once().await, Sent::Idle);

    let id = |ts: i64| format!("{}:{ts}", "b".repeat(64));
    assert_eq!(
        collector.event_ids(),
        vec![vec![id(5), id(9)]; 3],
        "every post carried the same ids, so Cloud dedupes a resend"
    );
}

#[tokio::test]
async fn crashes_wait_on_disk_while_the_collector_is_down() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    drop(listener);
    let sender = sender(&outbox, &address);

    for _ in 0..3 {
        assert_eq!(sender.send_once().await, Sent::Retry);
    }
    assert_eq!(held(&outbox), [5]);
    assert_eq!(outbox.dropped(), 0);
}

#[tokio::test]
async fn without_a_push_target_nothing_is_posted_and_the_crashes_wait() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(5)).unwrap();
    let (collector, address) = Collector::serve(Vec::new()).await;
    let sender = Sender::new(outbox.clone(), &address, || false).unwrap();

    assert_eq!(sender.send_once().await, Sent::Idle);
    assert_eq!(held(&outbox), [5]);
    assert!(collector.event_ids().is_empty());
}

#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn a_post_carries_at_most_a_hundred_of_the_oldest_crashes() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    for ts in 1..=150 {
        outbox.push(crash(ts)).unwrap();
    }
    let (collector, address) = Collector::serve(vec![StatusCode::OK, StatusCode::OK]).await;
    let sender = sender(&outbox, &address);

    assert_eq!(sender.send_once().await, Sent::Progress);
    assert_eq!(held(&outbox).len(), 50);
    assert_eq!(sender.send_once().await, Sent::Progress);
    let posts = collector.event_ids();
    assert_eq!(posts.iter().map(Vec::len).collect::<Vec<_>>(), [100, 50]);
    assert_eq!(posts[0].first(), Some(&format!("{}:1", "b".repeat(64))));
    assert_eq!(posts[1].first(), Some(&format!("{}:101", "b".repeat(64))));
}
