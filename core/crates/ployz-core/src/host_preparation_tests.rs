use serde_json::json;

use super::*;

fn zfs(value: Value) -> PrepStep<ZfsOutcome> {
    serde_json::from_value(value).unwrap()
}

#[test]
fn each_step_ployzd_has_written_reads_back_as_that_step() {
    assert_eq!(
        zfs(json!({"state": "installing", "started_at": null, "finished_at": null})),
        PrepStep::NotStarted
    );
    assert_eq!(
        zfs(
            json!({"state": "installing", "started_at": "2026-10-16T00:00:00Z", "finished_at": null})
        ),
        PrepStep::Running {
            started_at: "2026-10-16T00:00:00Z".to_owned()
        }
    );
    assert_eq!(
        zfs(json!({
            "state": "failed",
            "message": "dkms",
            "started_at": "2026-10-16T00:00:00Z",
            "finished_at": "2026-10-16T00:05:00Z"
        })),
        PrepStep::Finished {
            started_at: Some("2026-10-16T00:00:00Z".to_owned()),
            finished_at: "2026-10-16T00:05:00Z".to_owned(),
            outcome: ZfsOutcome::Failed {
                message: "dkms".to_owned()
            },
        }
    );
    // Discarding a build records it off without a start.
    assert_eq!(
        zfs(json!({"state": "off", "started_at": null, "finished_at": "2026-10-16T00:05:00Z"})),
        PrepStep::Finished {
            started_at: None,
            finished_at: "2026-10-16T00:05:00Z".to_owned(),
            outcome: ZfsOutcome::Off,
        }
    );
}

#[test]
fn a_step_whose_fields_disagree_reads_as_unknown_and_writes_back_unchanged() {
    for value in [
        json!({"state": "failed", "message": "dkms"}),
        json!({"state": "ready", "started_at": "2026-10-16T00:00:00Z"}),
        json!({"state": "installing", "started_at": "2026-10-16T00:00:00Z", "finished_at": "2026-10-16T00:05:00Z"}),
        json!({"state": "installing", "started_at": 7}),
        json!({"state": "paused", "finished_at": "2026-10-16T00:05:00Z"}),
        json!({"state": "skipped", "finished_at": "2026-10-16T00:05:00Z"}),
    ] {
        let step = zfs(value.clone());
        assert!(matches!(step, PrepStep::Unknown(_)), "{value}");
        assert_eq!(serde_json::to_value(&step).unwrap(), value);
    }
    let docker: PrepStep<DockerOutcome> =
        serde_json::from_value(json!({"state": "off", "finished_at": "2026-10-16T00:05:00Z"}))
            .unwrap();
    assert!(matches!(docker, PrepStep::Unknown(_)));
}

#[test]
fn a_step_writes_the_fields_an_older_reader_parses() {
    #[derive(Deserialize)]
    struct Older {
        state: String,
        message: Option<String>,
        started_at: Option<String>,
        finished_at: Option<String>,
    }
    let begun = Begun::at("2026-10-16T00:00:00Z".to_owned());
    let running: Older =
        serde_json::from_value(serde_json::to_value(begun.running::<DockerOutcome>()).unwrap())
            .unwrap();
    assert_eq!(running.state, "installing");
    assert_eq!(running.started_at.as_deref(), Some("2026-10-16T00:00:00Z"));
    assert_eq!(running.finished_at, None);

    let failed = begun.finish(
        DockerOutcome::Failed {
            message: "too old".to_owned(),
        },
        "2026-10-16T00:05:00Z".to_owned(),
    );
    let written = serde_json::to_value(&failed).unwrap();
    let older: Older = serde_json::from_value(written.clone()).unwrap();
    assert_eq!(older.state, "failed");
    assert_eq!(older.message.as_deref(), Some("too old"));
    assert_eq!(older.started_at.as_deref(), Some("2026-10-16T00:00:00Z"));
    assert_eq!(older.finished_at.as_deref(), Some("2026-10-16T00:05:00Z"));
    assert_eq!(
        serde_json::from_value::<PrepStep<_>>(written).unwrap(),
        failed
    );
}

#[test]
fn only_a_finished_failure_is_dated_as_one() {
    let failed =
        zfs(json!({"state": "failed", "message": "dkms", "finished_at": "2026-10-16T00:05:00Z"}));
    assert_eq!(failed.failed_at(), Some("2026-10-16T00:05:00Z"));
    let skipped =
        zfs(json!({"state": "skipped", "reason": "vm", "finished_at": "2026-10-16T00:05:00Z"}));
    assert_eq!(skipped.failed_at(), None);
    assert_eq!(skipped.last_dated(), Some("2026-10-16T00:05:00Z"));
}
