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
    Stream::ack(&*reopened, &held(&reopened)).unwrap();
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
    Stream::ack(&*outbox, &[acked]).unwrap();
    outbox.push(crash(i64::try_from(CAP).unwrap() + 1)).unwrap();
    assert!(!outbox.push_at(acked, crash(1)).unwrap());
    let held = held(&outbox);
    assert_eq!((held.len(), held.first()), (CAP, Some(&(acked + 1))));
    assert_eq!(outbox.ledger().1, 0);
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
    assert_eq!(outbox.ledger().1, 2);
    drop(outbox);
    assert_eq!(Outbox::open(&store).unwrap().ledger().1, 2);
}

#[test]
fn refusing_a_batch_the_full_outbox_already_dropped_counts_it_once() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    let cap = i64::try_from(CAP).unwrap();
    for ts in 1..=cap {
        outbox.push(crash(ts)).unwrap();
    }
    let batch = outbox.next().unwrap().unwrap();
    assert_eq!(batch.seqs.len(), BATCH);
    for ts in cap + 1..=cap + 100 {
        outbox.push(crash(ts)).unwrap();
    }
    assert_eq!(outbox.ledger().1, 100);
    outbox.refuse(&batch.seqs).unwrap();
    assert_eq!(outbox.ledger().1, 100);
}

#[test]
fn preparing_the_store_keeps_the_outbox() {
    let (_root, store) = store();
    Outbox::open(&store).unwrap().push(crash(7)).unwrap();
    store.prepare().unwrap();
    assert_eq!(held(&Outbox::open(&store).unwrap()), [7]);
}

#[test]
fn a_crash_batch_serializes_seqs_as_decimal_strings() {
    const SEQ: u64 = 1_760_000_000_123_456_789;
    let events = [Entry {
        seq: SEQ,
        marker: crash(i64::try_from(SEQ).unwrap()),
    }];
    let batch = Batch {
        stream: CRASH,
        seq: SEQ,
        dropped: 3,
        events: &events,
    };
    let json = serde_json::to_string(&batch).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        serde_json::json!({
            "stream": "crash",
            "seq": "1760000000123456789",
            "dropped": 3,
            "events": [{
                "seq": "1760000000123456789",
                "marker": {
                    "machine_id": "a".repeat(32),
                    "container_id": "b".repeat(64),
                    "service_id": "c".repeat(32),
                    "service": "prod/web",
                    "deployment_id": "d1",
                    "replica": "web-1",
                    "ts": 1_760_000_000_123_456_789_i64,
                    "kind": "exit",
                    "exit_code": 137,
                    "oom": true,
                    "stopped": false,
                },
            }],
        })
    );
}

#[test]
fn a_crash_batch_body_is_these_bytes() {
    let (_root, store) = store();
    let outbox = Outbox::open(&store).unwrap();
    outbox.push(crash(1_760_000_000_123_456_789)).unwrap();

    let held = Stream::next(&*outbox).unwrap().unwrap();

    assert_eq!(
        String::from_utf8(held.body).unwrap(),
        concat!(
            r#"{"stream":"crash","seq":"1760000000123456789","dropped":0,"events":[{"#,
            r#""seq":"1760000000123456789","marker":{"machine_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","#,
            r#""container_id":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","#,
            r#""service_id":"cccccccccccccccccccccccccccccccc","service":"prod/web","deployment_id":"d1","#,
            r#""replica":"web-1","ts":1760000000123456789,"kind":"exit","exit_code":137,"oom":true,"stopped":false}}]}"#
        )
    );
    assert_eq!(held.seqs, [1_760_000_000_123_456_789]);
}
