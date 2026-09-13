//! Build acceptance and capability-check admission tests.

use super::*;

#[tokio::test]
async fn build_revocation_rejects_new_and_queued_work_but_preserves_admitted_upload() {
    let fixture = Fixture::new().await;
    let (first, mut running) = fixture.request(Output::Load).await;
    assert!(matches!(event(&mut running).await, Event::Admitted { .. }));
    let (_second, mut queued) = fixture.request(Output::Load).await;
    assert!(matches!(
        event(&mut queued).await,
        Event::Progress(Progress::Stage(Stage::Queued))
    ));
    fixture
        .local
        .update(
            serde_json::from_value(serde_json::json!({
                "update": { "accepts_builds": false }
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let (_third, mut rejected) = fixture.request(Output::Load).await;
    assert!(matches!(terminal(&mut rejected).await, Outcome::Failed {
        stage: Stage::Admission, message, ..
    } if message.contains("does not accept Builds")));
    // Admission remains owned by the first stream after revocation.
    assert!(Admission::try_acquire_with(&fixture.policy).is_err());
    drop(first);
    assert!(matches!(
        terminal(&mut running).await,
        Outcome::Failed {
            stage: Stage::Upload,
            ..
        }
    ));
    assert!(matches!(terminal(&mut queued).await, Outcome::Failed {
        stage: Stage::Admission, message, ..
    } if message.contains("does not accept Builds")));
    assert!(!fixture.root.join("executed").exists());
}

#[tokio::test]
async fn running_build_finishes_after_build_acceptance_is_revoked() {
    let fixture = Fixture::new().await;
    let capture = fixture.capture();
    let client = ployz::connect::connect(
        Path::new("/missing-test-config"),
        Some(&fixture.address.replace("http://", "tcp://")),
        None,
    )
    .await
    .unwrap();
    fs::write(fixture.root.join("hold-build"), "").unwrap();
    let revoke = async {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !fixture.root.join("executed").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        fixture
            .local
            .update(
                serde_json::from_value(serde_json::json!({
                    "update": { "accepts_builds": false }
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        fs::remove_file(fixture.root.join("hold-build")).unwrap();
    };
    let (outcome, ()) = tokio::join!(
        capture.execute_remote(&client, fixture.machine.id, Default::default(), |_| {}),
        revoke,
    );
    assert!(matches!(outcome, Outcome::Images { .. }), "{outcome:?}");
}

#[tokio::test]
async fn capability_check_verifies_every_target_without_uploading_or_executing() {
    let fixture = Fixture::new().await;
    for (platforms, supported) in [
        (vec!["linux/amd64"], true),
        (vec!["linux/amd64", "linux/arm64"], false),
    ] {
        let targets = platforms
            .into_iter()
            .enumerate()
            .map(|(i, platform)| ployz_build::Target {
                name: format!("service{i}"),
                platforms: vec![platform.into()],
            })
            .collect();
        let (_sender, mut response) = fixture.request_frame(Input::Check(targets)).await;
        let outcome = terminal(&mut response).await;
        if supported {
            assert!(
                matches!(outcome, Outcome::CapabilitiesChecked { machine_id } if machine_id == fixture.machine.id)
            );
        } else {
            assert!(
                matches!(outcome, Outcome::Failed { message, .. } if message.contains("cannot build linux/arm64"))
            );
        }
        assert!(!fixture.root.join("executed").exists());
        assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
    }
}

#[tokio::test]
async fn capability_check_waits_for_active_build_and_rechecks_acceptance() {
    for revoke in [false, true] {
        let fixture = Fixture::new().await;
        let (first, mut running) = fixture.request(Output::Load).await;
        assert!(matches!(event(&mut running).await, Event::Admitted { .. }));
        let (_sender, mut response) = fixture
            .request_frame(Input::Check(vec![ployz_build::Target {
                name: "api".into(),
                platforms: vec!["linux/amd64".into()],
            }]))
            .await;
        assert!(matches!(
            event(&mut response).await,
            Event::Progress(Progress::Stage(Stage::Queued))
        ));
        assert!(Admission::try_acquire_with(&fixture.policy).is_err());
        if revoke {
            fixture
                .local
                .update(
                    serde_json::from_value(
                        serde_json::json!({"update": {"accepts_builds": false}}),
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
        }
        drop(first);
        assert!(matches!(
            terminal(&mut running).await,
            Outcome::Failed {
                stage: Stage::Upload,
                ..
            }
        ));
        let outcome = terminal(&mut response).await;
        if revoke {
            assert!(
                matches!(outcome, Outcome::Failed { stage: Stage::Admission, message, .. } if message.contains("does not accept Builds"))
            );
        } else {
            assert!(matches!(outcome, Outcome::CapabilitiesChecked { .. }));
        }
        assert!(!fixture.root.join("executed").exists());
    }
}

#[tokio::test]
async fn capability_check_cancellation_confirms_cleanup_or_retains_quarantine() {
    for uncertain in [false, true] {
        let fixture = Fixture::new().await;
        fs::write(fixture.root.join("slow-check"), "").unwrap();
        if uncertain {
            fs::write(fixture.root.join("fail-cleanup"), "").unwrap();
        }
        let (sender, mut response) = fixture
            .request_frame(Input::Check(vec![ployz_build::Target {
                name: "api".into(),
                platforms: vec!["linux/amd64".into()],
            }]))
            .await;
        assert!(matches!(event(&mut response).await, Event::Admitted { .. }));
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.root.join("checking").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        sender
            .send(remote::encode(&Input::Cancel).unwrap())
            .await
            .unwrap();
        let outcome = terminal(&mut response).await;
        if uncertain {
            assert!(matches!(outcome, Outcome::Unknown { .. }), "{outcome:?}");
            assert!(
                matches!(Admission::try_acquire_with(&fixture.policy), Err(error) if error.is_unknown())
            );
        } else {
            assert!(
                matches!(outcome, Outcome::Failed { message, .. } if message.contains("cancel"))
            );
            assert!(Admission::try_acquire_with(&fixture.policy).is_ok());
        }
        assert!(!fixture.root.join("executed").exists());
    }
}
