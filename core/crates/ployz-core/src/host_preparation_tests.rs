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

const WRITTEN_BY_PLOYZD: &str = r#"{
  "docker": {
    "finished_at": "2026-10-16T00:01:00Z",
    "started_at": "2026-10-16T00:00:00Z",
    "state": "ready"
  },
  "zfs": {
    "finished_at": null,
    "started_at": "2026-10-16T00:02:00Z",
    "state": "installing"
  },
  "added_packages": [
    "dkms",
    "perl"
  ],
  "zfs_build": {
    "route": "dkms",
    "module": "zfs",
    "version": "2.2.2",
    "source_dir": "/usr/src/zfs-2.2.2"
  },
  "added_contrib_source": true,
  "interrupted": [
    "zfs"
  ]
}"#;

fn written_state() -> PrepState {
    PrepState {
        docker: PrepStep::Finished {
            started_at: Some("2026-10-16T00:00:00Z".to_owned()),
            finished_at: "2026-10-16T00:01:00Z".to_owned(),
            outcome: DockerOutcome::Ready,
        },
        zfs: PrepStep::Running {
            started_at: "2026-10-16T00:02:00Z".to_owned(),
        },
        added_packages: vec!["dkms".to_owned(), "perl".to_owned()],
        zfs_build: Some(ZfsBuild::Dkms {
            module: "zfs".to_owned(),
            version: "2.2.2".to_owned(),
            source_dir: PathBuf::from("/usr/src/zfs-2.2.2"),
        }),
        added_contrib_source: true,
        interrupted: vec!["zfs".to_owned()],
    }
}

#[test]
fn the_state_file_ployzd_writes_is_these_bytes() {
    assert_eq!(
        String::from_utf8(encode_state(&written_state()).unwrap()).unwrap(),
        WRITTEN_BY_PLOYZD
    );
    assert_eq!(
        serde_json::from_str::<PrepState>(WRITTEN_BY_PLOYZD).unwrap(),
        written_state()
    );
    let ubuntu = PrepState {
        zfs_build: Some(ZfsBuild::Ubuntu {
            downloaded: vec!["zfsutils-linux".to_owned()],
        }),
        ..PrepState::default()
    };
    assert_eq!(
        serde_json::to_string(&ubuntu).unwrap(),
        r#"{"docker":{"finished_at":null,"started_at":null,"state":"installing"},"zfs":{"finished_at":null,"started_at":null,"state":"installing"},"added_packages":[],"zfs_build":{"route":"ubuntu","downloaded":["zfsutils-linux"]},"added_contrib_source":false,"interrupted":[]}"#
    );
}

#[test]
fn a_state_file_an_older_daemon_wrote_reads_with_the_later_fields_defaulted() {
    let older = r#"{
        "docker": {"state": "ready", "started_at": "2026-10-16T00:00:00Z", "finished_at": "2026-10-16T00:01:00Z"},
        "zfs": {"state": "installing", "started_at": null, "finished_at": null},
        "added_packages": ["dkms"],
        "zfs_build": {"route": "ubuntu", "downloaded": ["zfsutils-linux"]}
    }"#;
    let state: PrepState = serde_json::from_str(older).unwrap();
    assert!(!state.added_contrib_source);
    assert!(state.interrupted.is_empty());
    assert_eq!(state.added_packages, ["dkms"]);
    assert_eq!(
        state.zfs_build,
        Some(ZfsBuild::Ubuntu {
            downloaded: vec!["zfsutils-linux".to_owned()]
        })
    );
    assert_eq!(state.zfs, PrepStep::NotStarted);
    let steps: PrepSteps = serde_json::from_str(older).unwrap();
    assert_eq!(steps.docker, state.docker);
    assert_eq!(steps.zfs, PrepStep::NotStarted);
}

#[test]
fn the_check_in_reads_the_steps_in_the_bytes_ployzd_writes() {
    let steps: PrepSteps = serde_json::from_str(WRITTEN_BY_PLOYZD).unwrap();
    let state = written_state();
    assert_eq!(steps.docker, state.docker);
    assert_eq!(steps.zfs, state.zfs);
}

const STEPS: &str = r#""docker": {"state": "ready", "finished_at": "2026-10-16T00:00:00Z"}, "zfs": {"state": "off", "finished_at": "2026-10-16T00:00:00Z"}"#;

/// Shapes of `state.json` each reader takes, by what each does with them. ployzd refuses what it
/// can't read back whole and starts from nothing; the check-in reads only the two steps, so it
/// still reads what ployzd refuses outside them.
#[test]
fn the_state_file_shapes_each_reader_accepts_and_refuses() {
    let both = [
        format!("{{{STEPS}, \"added_packages\": []}}"),
        format!("{{{STEPS}, \"added_packages\": [], \"zfs_build\": null}}"),
        format!("{{{STEPS}, \"added_packages\": [], \"future\": {{\"nested\": [1]}}}}"),
        format!("{{{STEPS}, \"added_packages\": [], \"interrupted\": [\"docker\"]}}"),
        r#"{"docker": {"state": "paused"}, "zfs": {"state": "installing"}, "added_packages": []}"#
            .to_owned(),
    ];
    let steps_only = [
        format!("{{{STEPS}}}"),
        format!("{{{STEPS}, \"added_packages\": \"dkms\"}}"),
        format!("{{{STEPS}, \"added_packages\": [], \"zfs_build\": {{\"route\": \"rpm\"}}}}"),
        format!("{{{STEPS}, \"added_packages\": [], \"added_contrib_source\": null}}"),
        format!("{{{STEPS}, \"added_packages\": [], \"interrupted\": 3}}"),
    ];
    let neither = [
        r#"{"zfs": {"state": "off", "finished_at": "2026-10-16T00:00:00Z"}, "added_packages": []}"#,
        r#"{"docker": null, "zfs": {"state": "off"}, "added_packages": []}"#,
        "{}",
        "[]",
        "",
    ];
    for text in both.iter().map(String::as_str) {
        assert!(serde_json::from_str::<PrepState>(text).is_ok(), "{text}");
        assert!(serde_json::from_str::<PrepSteps>(text).is_ok(), "{text}");
    }
    for text in steps_only.iter().map(String::as_str) {
        assert!(serde_json::from_str::<PrepState>(text).is_err(), "{text}");
        assert!(serde_json::from_str::<PrepSteps>(text).is_ok(), "{text}");
    }
    for text in neither {
        assert!(serde_json::from_str::<PrepState>(text).is_err(), "{text}");
        assert!(serde_json::from_str::<PrepSteps>(text).is_err(), "{text}");
    }
}

#[test]
fn a_step_ployzd_cannot_read_survives_a_read_and_write_of_the_whole_state() {
    let paused =
        &r#"{"docker": {"state": "paused"}, "zfs": {"state": "installing"}, "added_packages": []}"#;
    let state: PrepState = serde_json::from_str(paused).unwrap();
    assert!(matches!(state.docker, PrepStep::Unknown(_)));
    let docker = |value: Value| value.get("docker").cloned();
    assert_eq!(
        docker(serde_json::to_value(&state).unwrap()),
        docker(serde_json::from_str(paused).unwrap())
    );
}

#[test]
fn reading_the_state_file_tells_a_missing_file_from_one_that_does_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    assert!(read_state::<PrepState>(&path).unwrap().is_none());

    fs::write(&path, WRITTEN_BY_PLOYZD).unwrap();
    assert_eq!(
        read_state::<PrepState>(&path).unwrap(),
        Some(written_state())
    );
    assert!(read_state::<PrepSteps>(&path).unwrap().is_some());

    fs::write(&path, r#"{"docker": {"state": "ready"}}"#).unwrap();
    assert!(matches!(
        read_state::<PrepState>(&path),
        Err(StateError::Malformed(_))
    ));

    let directory = dir.path().join("directory");
    fs::create_dir(&directory).unwrap();
    assert!(matches!(
        read_state::<PrepState>(&directory),
        Err(StateError::Unreadable(_))
    ));
}

#[test]
fn the_check_in_reader_names_the_record_it_expected_as_it_always_has() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    for (bytes, expected) in [
        (
            "null",
            "invalid type: null, expected struct PrepState at line 1 column 4",
        ),
        (
            "123",
            "invalid type: integer `123`, expected struct PrepState at line 1 column 3",
        ),
        (
            "[]",
            "invalid length 0, expected struct PrepState with 2 elements at line 1 column 2",
        ),
        (
            "\"abc\"",
            "invalid type: string \"abc\", expected struct PrepState at line 1 column 5",
        ),
    ] {
        fs::write(&path, bytes).unwrap();
        let Err(StateError::Malformed(error)) = read_state::<PrepSteps>(&path) else {
            panic!("{bytes} read");
        };
        assert_eq!(error.to_string(), expected);
    }
}
