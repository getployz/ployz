//! Shared acquisition, failure, and client lifetime checks.

use super::*;

#[tokio::test]
async fn last_disconnect_cancels_an_initial_read_that_never_completes() {
    let entry = machine("edge", ENTRY_ID, 1);
    let shared = super::super::RuntimeWatch::default();
    let (wake, changes) = mpsc::channel(1);
    let (started, mut reading) = mpsc::channel(1);
    let stream = shared
        .subscribe_with(async || {
            Ok(serve_runtime_watch(
                entry.id,
                move || {
                    let started = started.clone();
                    async move {
                        started.send(()).await.unwrap();
                        std::future::pending().await
                    }
                },
                async || LatestSample {
                    telemetry: None,
                    observed_at: OBSERVED_AT.into(),
                },
                ReceiverStream::new(changes),
                futures_util::stream::pending(),
            ))
        })
        .await
        .unwrap();
    reading.recv().await.unwrap();
    drop(stream);
    tokio::time::timeout(Duration::from_secs(1), wake.closed())
        .await
        .expect("last disconnect must cancel a pending initial store read");
}

#[tokio::test]
async fn clients_share_acquisition_without_backpressure_or_shared_cancellation() {
    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let shared = super::super::RuntimeWatch::default();
    let (wake, changes) = mpsc::channel(1);
    let mut first = shared
        .subscribe_with(async || Ok(serve_fixture(entry.id, &fixture, changes)))
        .await
        .unwrap();
    let initial = next_frame(&mut first).await;
    let mut slow = shared
        .subscribe_with(async || panic!("second client must reuse the producer"))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut slow).await, initial);

    // More frames than either stream's delivery buffer can hold.
    for index in 0..20 {
        let volume = volume_on(ENTRY_ID, &format!("data-{index}"));
        fixture.set(snapshot(vec![entry.clone()], vec![volume.clone()]));
        wake.send(Ok(())).await.unwrap();
        assert_eq!(next_frame(&mut first).await.volumes, [volume]);
    }
    let mut late = shared
        .subscribe_with(async || panic!("late client must reuse the latest observation"))
        .await
        .unwrap();
    assert_eq!(
        next_frame(&mut late).await.volumes,
        [volume_on(ENTRY_ID, "data-19")]
    );
    drop(late);
    drop(slow);
    let volume = volume_on(ENTRY_ID, "after-disconnect");
    fixture.set(snapshot(vec![entry], vec![volume.clone()]));
    wake.send(Ok(())).await.unwrap();
    assert_eq!(next_frame(&mut first).await.volumes, [volume]);
    drop(first);
    tokio::time::timeout(Duration::from_secs(1), wake.closed())
        .await
        .expect("last client must release acquisition even while the owner remains alive");
}

#[tokio::test]
async fn concurrent_subscribers_share_startup_and_reconnect_after_failure() {
    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let shared = super::super::RuntimeWatch::default();
    let (wake, changes) = mpsc::channel(1);
    let (first, second) = tokio::join!(
        shared.subscribe_with(async || {
            tokio::task::yield_now().await;
            Ok(serve_fixture(entry.id, &fixture, changes))
        }),
        shared.subscribe_with(async || panic!("concurrent subscriber started another producer")),
    );
    let (mut first, mut second) = (first.unwrap(), second.unwrap());
    assert_eq!(next_frame(&mut first).await, next_frame(&mut second).await);
    fixture.fail("store closed");
    wake.send(Ok(())).await.unwrap();
    for stream in [&mut first, &mut second] {
        let error = tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::Unavailable);
        assert!(stream.next().await.is_none());
    }

    // Failed clients remain in scope; their old generation must not poison reconnects.
    let volume = volume_on(ENTRY_ID, "reconnected");
    fixture.set(snapshot(vec![entry.clone()], vec![volume.clone()]));
    let (_wake, changes) = mpsc::channel(1);
    let mut reconnected = shared
        .subscribe_with(async || Ok(serve_fixture(entry.id, &fixture, changes)))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut reconnected).await.volumes, [volume]);
}

#[tokio::test]
async fn failed_startup_can_be_retried() {
    let shared = super::super::RuntimeWatch::default();
    assert!(
        shared
            .subscribe_with(async || Err(Error::Protocol("offline".into())))
            .await
            .is_err()
    );
    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let (_wake, changes) = mpsc::channel(1);
    let mut stream = shared
        .subscribe_with(async || Ok(serve_fixture(entry.id, &fixture, changes)))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut stream).await.machines.len(), 1);
}

#[tokio::test]
async fn immediate_reconnect_after_last_disconnect_starts_fresh() {
    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let shared = super::super::RuntimeWatch::default();
    let (_wake, changes) = mpsc::channel(1);
    let mut stream = shared
        .subscribe_with(async || Ok(serve_fixture(entry.id, &fixture, changes)))
        .await
        .unwrap();
    next_frame(&mut stream).await;
    drop(stream);

    // No yield: stream destruction, not a background task, must end membership.
    let volume = volume_on(ENTRY_ID, "fresh");
    fixture.set(snapshot(vec![entry.clone()], vec![volume.clone()]));
    let (_wake, changes) = mpsc::channel(1);
    let mut reconnected = shared
        .subscribe_with(async || Ok(serve_fixture(entry.id, &fixture, changes)))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut reconnected).await.volumes, [volume]);
}

#[tokio::test]
async fn reconnect_rejects_a_published_error_before_its_sender_is_dropped() {
    let shared = super::super::RuntimeWatch::default();
    let (latest, updates) = tokio::sync::watch::channel(None);
    let updates = Arc::new(updates);
    *shared.current.lock().await = Arc::downgrade(&updates);
    let mut failed = super::super::stream_watch(Arc::clone(&updates));

    // Freeze the multithreaded window after publication but before the producer returns.
    latest.send_replace(Some(Arc::new(Err(tonic::Status::unavailable(
        "store closed",
    )))));
    assert_eq!(
        failed.next().await.unwrap().unwrap_err().code(),
        tonic::Code::Unavailable
    );
    assert!(
        updates.has_changed().is_ok(),
        "the producer's sender is still alive"
    );

    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let (_wake, changes) = mpsc::channel(1);
    let mut reconnected = shared
        .subscribe_with(async || Ok(serve_fixture(entry.id, &fixture, changes)))
        .await
        .unwrap();
    assert_eq!(next_frame(&mut reconnected).await.machines.len(), 1);
    drop(latest);
}
