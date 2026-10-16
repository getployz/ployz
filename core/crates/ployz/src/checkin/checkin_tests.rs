use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use chrono::{DateTime, TimeDelta, Utc};
use iroh::SecretKey;
use reqwest::StatusCode;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::client::{self, Reply};
use super::wire::{self, Answer, PrepState, Probe};
use super::*;

const PROBE: &str = r#"{
    "facts": {
        "os": {"id": "ubuntu", "version_id": "24.04"},
        "kernel": "6.8.0",
        "arch": "x86_64",
        "virt": "BareMetal",
        "cgroup": "V2",
        "cpu_count": 4,
        "memory_total_bytes": 8589934592,
        "disks": [],
        "docker": "Absent"
    },
    "assessment": {
        "blockers": [{"code": "kernel_too_old", "message": "Kernel 4.4 is too old."}],
        "warnings": [],
        "zfs": {"Ineligible": {"reason": {"code": "zfs_off", "message": "Off."}}}
    }
}"#;

#[derive(Default)]
struct FakeHost {
    prepares: AtomicU32,
    disables: AtomicU32,
}

impl Host for FakeHost {
    async fn probe(&self) -> Result<Probe, String> {
        Ok(serde_json::from_str(PROBE).unwrap())
    }

    fn hostname(&self) -> String {
        "waiting-1".to_owned()
    }

    async fn start_prepare(&self) -> Result<(), String> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn disable_loop(&self) -> Result<(), String> {
        self.disables.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn paths(dir: &tempfile::TempDir) -> Paths {
    Paths {
        checkin_dir: dir.path().join("checkin"),
        prepare_state: dir.path().join("prepare/state.json"),
        token_file: dir.path().join("fleet-token"),
    }
}

fn key() -> CheckinKey {
    CheckinKey::new(SecretKey::from_bytes(&[7; 32]))
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text).unwrap().to_utc()
}

async fn cloud(
    status: &'static str,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut buf = vec![0; 16384];
        let read = stream.read(&mut buf).await.unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8_lossy(buf.get(..read).unwrap()).into_owned()
    });
    (url, server)
}

fn client(url: &str) -> CheckinClient {
    CheckinClient::new(url, FleetToken::literal("pflt_test")).unwrap()
}

#[test]
fn the_signature_matches_what_cloud_verifies() {
    let key = key();
    assert_eq!(
        key.public_hex(),
        "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
    );
    assert_eq!(
        key.sign(1_760_000_000, br#"{"protocolVersion":1}"#),
        "YiupzRJe5Ujvmov2lPxHcQxiGIX0lbhzNXr-H9osVYOCfD-T2atlnlReOLVA6nBhjl-jehzUe6iZ0t5ttcoIDw"
    );
}

#[test]
fn an_answer_this_cli_cannot_read_waits_a_minute() {
    for body in [
        &br#"{"kind":"reboot"}"#[..],
        b"not json",
        br#"{"kind":"wait"}"#,
    ] {
        assert_eq!(wire::answer(body), Answer::Wait(Duration::from_secs(60)));
    }
    assert_eq!(
        wire::answer(br#"{"kind":"wait","retryAfter":30}"#),
        Answer::Wait(Duration::from_secs(30))
    );
    assert_eq!(
        wire::answer(br#"{"kind":"wait","retryAfter":999999}"#),
        Answer::Wait(Duration::from_secs(3600))
    );
}

#[test]
fn unauthorized_stops_as_revoked_and_rate_limits_wait() {
    let Reply::Answer(Answer::Stop(reason, _)) = client::reply(StatusCode::UNAUTHORIZED, None, b"")
    else {
        panic!("401 must stop the loop");
    };
    assert_eq!(reason, StopReason::TokenRevoked);
    assert_eq!(
        client::reply(
            StatusCode::TOO_MANY_REQUESTS,
            Some(Duration::from_secs(9)),
            b""
        ),
        Reply::Answer(Answer::Wait(Duration::from_secs(9)))
    );
    assert_eq!(
        client::reply(StatusCode::TOO_MANY_REQUESTS, None, b""),
        Reply::Answer(Answer::Wait(Duration::from_secs(1)))
    );
    assert!(matches!(
        client::reply(StatusCode::BAD_GATEWAY, None, b""),
        Reply::Unreachable(after, _) if after == Duration::from_secs(60)
    ));
}

#[tokio::test]
async fn stop_removes_the_token_and_disables_the_loop() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, "pflt_test\n").unwrap();
    let host = FakeHost::default();
    stop(&host, &paths).await;
    assert!(!paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
    stop(&host, &paths).await;
    assert_eq!(host.disables.load(Ordering::SeqCst), 2);
}

#[test]
fn a_rerun_keeps_the_same_check_in_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("checkin/key");
    let first = CheckinKey::load_or_create(&path).unwrap();
    let second = CheckinKey::load_or_create(&path).unwrap();
    assert_eq!(first.public_hex(), second.public_hex());
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&path, b"short").unwrap();
    assert!(matches!(
        CheckinKey::load_or_create(&path),
        Err(KeyError::Malformed { .. })
    ));
}

#[test]
fn a_fleet_token_file_must_hold_one_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet-token");
    fs::write(&path, "pflt_abc\n").unwrap();
    assert!(FleetToken::read(&path).is_ok());
    fs::write(&path, "pmet_abc").unwrap();
    assert!(matches!(
        FleetToken::read(&path),
        Err(KeyError::NotAToken { .. })
    ));
}

#[tokio::test]
async fn enroll_waits_ten_seconds_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let (url, server) = cloud("200 OK", r#"{"kind":"enroll","instruction":{}}"#).await;
    let host = FakeHost::default();
    let now = at("2026-10-16T00:00:00Z");
    let next = pass(&host, &client(&url), &key(), &paths, now).await;
    assert_eq!(
        next,
        Next::Wait {
            after: Duration::from_secs(10),
            log: Some(ENROLL_LINE)
        }
    );
    let request = server.await.unwrap();
    assert!(request.starts_with("POST /api/checkin HTTP/1.1"));
    let lines: Vec<String> = request.lines().map(str::to_ascii_lowercase).collect();
    assert!(
        lines
            .iter()
            .any(|line| line == "authorization: bearer pflt_test")
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("ployz-signature: v1="))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("ployz-timestamp: "))
    );
    assert!(request.contains(r#""checkinKey":"ea4a6c63"#));
    assert_eq!(host.prepares.load(Ordering::SeqCst), 1);

    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.answer, Seen::Enroll);
    assert_eq!(status.at, now);
    assert_eq!(status.blockers.first().unwrap().code, "kernel_too_old");
    assert_eq!(
        fs::metadata(paths.status()).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[tokio::test]
async fn a_stop_answer_is_recorded_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let (url, _server) = cloud(
        "200 OK",
        r#"{"kind":"stop","reason":"rejected","message":"This server was rejected in Ployz Cloud."}"#,
    )
    .await;
    let host = FakeHost::default();
    let next = pass(&host, &client(&url), &key(), &paths, Utc::now()).await;
    assert_eq!(
        next,
        Next::Stop(
            StopReason::Rejected,
            "This server was rejected in Ployz Cloud.".to_owned()
        )
    );
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.headline(),
        Headline {
            title: "Rejected",
            ended: true
        }
    );
}

#[tokio::test]
async fn a_cloud_that_does_not_answer_is_retried_in_a_minute() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let host = FakeHost::default();
    let Next::Retry { after, .. } = pass(&host, &client(&url), &key(), &paths, Utc::now()).await
    else {
        panic!("an unreachable Cloud must retry");
    };
    assert_eq!(after, Duration::from_secs(60));
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.headline(),
        Headline {
            title: "Can't reach Cloud",
            ended: false
        }
    );
}

fn state(json: &str) -> PrepState {
    serde_json::from_str(json).unwrap()
}

#[test]
fn preparation_starts_when_missing_running_or_failed_long_enough_ago() {
    let now = at("2026-10-16T01:00:00Z");
    assert_eq!(prepare_due(None, 0, now), Some(PrepareRun::Start));
    let installing = state(r#"{"docker":{"state":"installing"},"zfs":{"state":"installing"}}"#);
    assert_eq!(
        prepare_due(Some(&installing), 3, now),
        Some(PrepareRun::Start)
    );
    let ready = state(r#"{"docker":{"state":"ready"},"zfs":{"state":"off"}}"#);
    assert_eq!(prepare_due(Some(&ready), 0, now), None);

    let failed = |finished: &str| {
        state(&format!(
            r#"{{"docker":{{"state":"ready"}},"zfs":{{"state":"failed","message":"dkms","finished_at":"{finished}"}}}}"#
        ))
    };
    assert_eq!(
        prepare_due(Some(&failed("2026-10-16T00:55:00Z")), 0, now),
        None
    );
    assert_eq!(
        prepare_due(Some(&failed("2026-10-16T00:50:00Z")), 0, now),
        Some(PrepareRun::Retry)
    );
    assert_eq!(
        prepare_due(Some(&failed("2026-10-16T00:50:00Z")), 2, now),
        Some(PrepareRun::Retry)
    );
    assert_eq!(
        prepare_due(Some(&failed("2026-10-16T00:50:00Z")), 3, now),
        None
    );
}

#[test]
fn the_headline_says_where_the_host_stands() {
    let mut status = LocalCheckin {
        answer: Seen::Wait,
        reason: None,
        message: None,
        at: Utc::now(),
        prep: Some(Prep {
            docker: DockerPrep::Installing,
            zfs: ZfsPrep::Installing,
        }),
        blockers: Vec::new(),
        prepare_retries: 0,
    };
    assert_eq!(
        status.headline(),
        Headline {
            title: "Installing Docker",
            ended: false
        }
    );
    status.prep = Some(Prep {
        docker: DockerPrep::Ready,
        zfs: ZfsPrep::Installing,
    });
    assert_eq!(
        status.headline(),
        Headline {
            title: "Building ZFS",
            ended: false
        }
    );
    status.prep = Some(Prep {
        docker: DockerPrep::Ready,
        zfs: ZfsPrep::Ready,
    });
    assert_eq!(
        status.headline(),
        Headline {
            title: "Waiting for approval",
            ended: false
        }
    );
    status.blockers = vec![Issue {
        code: "docker_stopped".into(),
        message: "Docker is installed but not running.".into(),
    }];
    assert_eq!(
        status.headline(),
        Headline {
            title: "Blocked",
            ended: false
        }
    );
    status.answer = Seen::Stop;
    status.reason = Some(StopReason::TokenExpired);
    assert_eq!(
        status.headline(),
        Headline {
            title: "Token expired",
            ended: true
        }
    );
}

#[test]
fn ago_reads_in_the_largest_whole_unit() {
    let now = at("2026-10-16T12:00:00Z");
    assert_eq!(ago(now - TimeDelta::seconds(12), now), "12s ago");
    assert_eq!(ago(now - TimeDelta::minutes(5), now), "5m ago");
    assert_eq!(ago(now - TimeDelta::hours(3), now), "3h ago");
    assert_eq!(ago(now - TimeDelta::days(2), now), "2d ago");
    assert_eq!(ago(now + TimeDelta::seconds(5), now), "0s ago");
}
