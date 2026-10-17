use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use chrono::{DateTime, SubsecRound, TimeDelta, Utc};
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

const OLD: &str = "pflt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const NEW: &str = "pflt_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
const REFUSAL: &str = r#"{"error":"Authentication is required."}"#;

#[derive(Default)]
struct FakeHost {
    prepares: AtomicU32,
    disables: AtomicU32,
    preparing: AtomicBool,
    probe_hangs: bool,
    /// A preparation that finishes, writing this state, while its unit is being checked.
    finishes: Option<(PathBuf, &'static str)>,
    stale: Option<String>,
}

impl Host for FakeHost {
    async fn probe(&self) -> Result<Probe, String> {
        if self.probe_hangs {
            std::future::pending::<()>().await;
        }
        Ok(serde_json::from_str(PROBE).unwrap())
    }

    fn hostname(&self) -> String {
        "waiting-1".to_owned()
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        if let Some((path, state)) = &self.finishes {
            fs::write(path, state).unwrap();
        }
        Ok(self.preparing.load(Ordering::SeqCst))
    }

    async fn start_prepare(&self) -> Result<(), String> {
        self.preparing.store(true, Ordering::SeqCst);
        self.prepares.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn disable_loop(&self) -> Result<(), String> {
        self.disables.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stale_ployzd(&self) -> Option<String> {
        self.stale.clone()
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
    CheckinClient::new(url, FleetToken::literal(OLD)).unwrap()
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
    for body in [&br#"{"kind":"reboot"}"#[..], br#"{"kind":"wait"}"#] {
        assert_eq!(
            wire::answer(body),
            Some(Answer::Wait(Duration::from_secs(60)))
        );
    }
    assert_eq!(
        wire::answer(br#"{"kind":"wait","retryAfter":30}"#),
        Some(Answer::Wait(Duration::from_secs(30)))
    );
    for huge in [
        &br#"{"kind":"wait","retryAfter":999999}"#[..],
        br#"{"kind":"wait","retryAfter":1e300}"#,
    ] {
        assert_eq!(
            wire::answer(huge),
            Some(Answer::Wait(Duration::from_secs(3600)))
        );
    }
}

#[test]
fn an_answer_cloud_could_not_have_sent_is_not_cloud() {
    for body in [
        &b"<!doctype html><title>Welcome to nginx</title>"[..],
        b"not json",
        b"",
        b"[]",
        br#"{"ok":true}"#,
        br#"{"kind":7}"#,
    ] {
        assert_eq!(wire::answer(body), None);
        assert!(matches!(
            client::reply(StatusCode::OK, None, None, body),
            Reply::NotCloud(why) if why.starts_with("answered 200 OK")
        ));
    }
    for status in [
        StatusCode::NOT_FOUND,
        StatusCode::METHOD_NOT_ALLOWED,
        StatusCode::FORBIDDEN,
        StatusCode::GONE,
    ] {
        assert_eq!(
            client::reply(status, None, None, b"Not Found"),
            Reply::NotCloud(format!("answered {status}"))
        );
    }
    for status in [
        StatusCode::REQUEST_TIMEOUT,
        StatusCode::INTERNAL_SERVER_ERROR,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        assert!(matches!(
            client::reply(status, None, None, b""),
            Reply::Unreachable(after, _) if after == Duration::from_secs(60)
        ));
    }
}

#[test]
fn only_cloud_refusing_with_agreeing_clocks_counts_against_the_token() {
    let dated = |skew| Some(Utc::now() + TimeDelta::seconds(skew));
    let refused = |date| client::reply(StatusCode::UNAUTHORIZED, None, date, REFUSAL.as_bytes());
    for skew in [4, 239, -239] {
        let date = dated(skew);
        assert_eq!(refused(date), Reply::Refused(date.unwrap()), "skew {skew}");
    }
    for (skew, why) in [
        (
            -300,
            "Cloud refused this Fleet Token, and this server's clock is 300s off",
        ),
        (
            250,
            "Cloud refused this Fleet Token, and this server's clock is 250s off",
        ),
        (-301, "This server's clock is 301s off"),
        (-900, "This server's clock is 900s off"),
    ] {
        let Reply::ClockOff(said) = refused(dated(skew)) else {
            panic!("a 401 {skew}s off says nothing about the token");
        };
        assert!(said.starts_with(why), "{said}");
    }
    let Reply::ClockOff(near) = refused(dated(250)) else {
        unreachable!()
    };
    assert!(near.ends_with("so the refusal can be confirmed"), "{near}");
    assert!(matches!(
        refused(None),
        Reply::Unreachable(after, why) if after == Duration::from_secs(60) && why.contains("without a readable Date")
    ));
    for body in [
        &b"<html>Authentication required by gateway</html>"[..],
        b"",
        br#"{"error":"Unauthorized."}"#,
    ] {
        assert!(matches!(
            client::reply(StatusCode::UNAUTHORIZED, None, Some(Utc::now()), body),
            Reply::Unreachable(after, _) if after == Duration::from_secs(60)
        ));
    }
}

#[test]
fn an_unreadable_check_in_waits_for_a_newer_ployz_and_rate_limits_wait() {
    assert!(matches!(
        client::reply(StatusCode::BAD_REQUEST, None, None, b""),
        Reply::Outdated(why) if why.ends_with("Install a newer ployz")
    ));
    assert_eq!(
        client::reply(StatusCode::PAYLOAD_TOO_LARGE, None, None, b""),
        Reply::Outdated("Cloud couldn't take this check-in (413 Payload Too Large)".to_owned())
    );
    assert_eq!(
        client::reply(
            StatusCode::TOO_MANY_REQUESTS,
            Some(Duration::from_secs(9)),
            None,
            b""
        ),
        Reply::Answer(Answer::Wait(Duration::from_secs(9)))
    );
    assert_eq!(
        client::reply(
            StatusCode::TOO_MANY_REQUESTS,
            Some(Duration::from_secs(u64::MAX)),
            None,
            b""
        ),
        Reply::Answer(Answer::Wait(Duration::from_secs(3600)))
    );
    assert_eq!(
        client::reply(StatusCode::TOO_MANY_REQUESTS, None, None, b""),
        Reply::Answer(Answer::Wait(Duration::from_secs(1)))
    );
    assert!(matches!(
        client::reply(StatusCode::BAD_GATEWAY, None, None, b""),
        Reply::Unreachable(after, _) if after == Duration::from_secs(60)
    ));
}

#[tokio::test]
async fn stop_removes_only_the_token_it_sent_and_keeps_the_loop_for_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let host = FakeHost::default();
    let sent = client("http://127.0.0.1:9");
    let stopping = |reason| stop(&host, &sent, &paths, reason);
    fs::write(&paths.token_file, format!("{NEW}\n")).unwrap();
    assert_eq!(
        stopping(StopReason::TokenRevoked).await,
        CheckinEnd::TokenChanged
    );
    assert_eq!(fs::read_to_string(&paths.token_file).unwrap().trim(), NEW);
    assert_eq!(host.disables.load(Ordering::SeqCst), 0);
    assert_eq!(
        stopping(StopReason::Rejected).await,
        CheckinEnd::Stopped(StopReason::Rejected)
    );
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    stopping(StopReason::TokenRevoked).await;
    assert!(!paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 2);
    stopping(StopReason::TokenExpired).await;
    assert_eq!(host.disables.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_token_file_still_partial_after_settling_keeps_the_loop_to_restart() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, "pflt_").unwrap();
    let (url, server) = stopping_cloud("token_revoked", 1).await;
    let host = FakeHost::default();
    let end = tokio::time::timeout(
        Duration::from_secs(10),
        run(&host, &mut client(&url), &key(), &paths),
    )
    .await
    .expect("a partial token is never sent, so the loop ends after the one stop");
    assert_eq!(end, CheckinEnd::TokenChanged);
    assert_eq!(server.await.unwrap().len(), 1);
    assert_eq!(fs::read_to_string(&paths.token_file).unwrap(), "pflt_");
    assert_eq!(host.disables.load(Ordering::SeqCst), 0);
}

/// Answers `stops` check-ins with the same stop and returns each request.
async fn stopping_cloud(
    reason: &'static str,
    stops: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let body = format!(r#"{{"kind":"stop","reason":"{reason}","message":"Done."}}"#);
    answering_cloud("200 OK".to_owned(), body, stops).await
}

/// Answers `times` check-ins with 401, dated `skew` seconds off this host's clock.
async fn unauthorized_cloud(
    skew: i64,
    times: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let date = (Utc::now() + TimeDelta::seconds(skew)).to_rfc2822();
    answering_cloud(
        format!("401 Unauthorized\r\nDate: {date}"),
        REFUSAL.to_owned(),
        times,
    )
    .await
}

async fn answering_cloud(
    status: String,
    body: String,
    times: usize,
) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..times {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0; 16384];
            let read = stream.read(&mut buf).await.unwrap();
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            requests.push(String::from_utf8_lossy(buf.get(..read).unwrap()).into_owned());
        }
        requests
    });
    (url, server)
}

async fn old_then_new(server: tokio::task::JoinHandle<Vec<String>>) {
    let requests = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the new token checks in once more");
    let [old, new]: [String; 2] = requests.unwrap().try_into().unwrap();
    assert!(
        old.contains(&format!("authorization: Bearer {OLD}")),
        "{old}"
    );
    assert!(
        new.contains(&format!("authorization: Bearer {NEW}")),
        "{new}"
    );
}

#[tokio::test]
async fn a_stop_for_a_replaced_token_checks_in_with_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{NEW}\n")).unwrap();
    let (url, server) = stopping_cloud("token_revoked", 2).await;
    let host = FakeHost::default();
    let end = run(&host, &mut client(&url), &key(), &paths).await;
    assert!(matches!(end, CheckinEnd::Stopped(StopReason::TokenRevoked)));
    old_then_new(server).await;
    assert!(!paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_token_file_caught_mid_write_is_read_again_before_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, "").unwrap();
    let token_file = paths.token_file.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        fs::write(token_file, format!("{NEW}\n")).unwrap();
    });
    let (url, server) = stopping_cloud("token_expired", 2).await;
    let host = FakeHost::default();
    let end = run(&host, &mut client(&url), &key(), &paths).await;
    writer.await.unwrap();
    assert!(matches!(end, CheckinEnd::Stopped(StopReason::TokenExpired)));
    old_then_new(server).await;
}

#[tokio::test]
async fn a_token_file_holding_only_part_of_a_token_is_read_again_before_stopping() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let mut file = fs::File::create(&paths.token_file).unwrap();
    let (prefix, rest) = NEW.split_at(5);
    std::io::Write::write_all(&mut file, prefix.as_bytes()).unwrap();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::io::Write::write_all(&mut file, format!("{rest}\n").as_bytes()).unwrap();
    });
    let (url, server) = stopping_cloud("token_revoked", 2).await;
    let host = FakeHost::default();
    let end = run(&host, &mut client(&url), &key(), &paths).await;
    writer.await.unwrap();
    assert!(matches!(end, CheckinEnd::Stopped(StopReason::TokenRevoked)));
    old_then_new(server).await;
    assert!(!paths.token_file.exists());
}

#[tokio::test]
async fn a_rejection_stops_even_when_the_token_was_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{NEW}\n")).unwrap();
    let (url, server) = stopping_cloud("rejected", 1).await;
    let host = FakeHost::default();
    let end = tokio::time::timeout(
        Duration::from_secs(10),
        run(&host, &mut client(&url), &key(), &paths),
    )
    .await
    .expect("a rejection ends the loop without checking in again");
    assert!(matches!(end, CheckinEnd::Stopped(StopReason::Rejected)));
    assert_eq!(server.await.unwrap().len(), 1);
    // Only the token Cloud answered is removed. The new one waits for the next install.
    assert_eq!(fs::read_to_string(&paths.token_file).unwrap().trim(), NEW);
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[test]
fn the_preparation_unit_quotes_ployzd_and_refuses_paths_that_break_it() {
    let text = unit::prepare(Path::new("/opt/my tools/ployzd"), true).unwrap();
    assert!(text.contains("ExecStart=\"/opt/my tools/ployzd\" install --prepare --skip-zfs\n"));
    assert!(unit::prepare(Path::new("/opt/a\nExecStart=/bin/sh"), false).is_err());
}

#[test]
fn the_check_in_unit_quotes_paths_and_refuses_values_that_break_it() {
    let text = unit::checkin(
        Path::new("/opt/my tools/ployz"),
        Path::new("/var/lib/ployz/checkin/ployzd"),
        Path::new("/etc/ployz/fleet-token"),
        "https://cloud.example/100%",
    )
    .unwrap();
    assert!(text.contains(
        "ExecStart=\"/opt/my tools/ployz\" server checkin --token-file \"/etc/ployz/fleet-token\" --ployzd \"/var/lib/ployz/checkin/ployzd\"\n"
    ));
    assert!(text.contains("Environment=\"PLOYZ_CLOUD_URL=https://cloud.example/100%%\"\n"));
    for url in ["https://a\nExecStart=/bin/sh", "https://a\"", "https://a\\"] {
        assert!(
            unit::checkin(
                Path::new("/usr/bin/ployz"),
                Path::new("/d"),
                Path::new("/t"),
                url
            )
            .is_err()
        );
    }
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
    for token in [OLD, "pflt_abcdefghijklmnopqrstuvwxyz-_0123456789ABCDE"] {
        fs::write(&path, format!("{token}\n")).unwrap();
        assert!(FleetToken::read(&path).is_ok(), "{token}");
    }
    let short = &OLD[..OLD.len() - 1];
    let long = format!("{OLD}A");
    let odd = format!("{short}=");
    for token in ["pflt_", short, &long, &odd, "pmet_abc", ""] {
        fs::write(&path, token).unwrap();
        assert!(
            matches!(FleetToken::read(&path), Err(KeyError::NotAToken { .. })),
            "{token}"
        );
    }
}

#[tokio::test]
async fn enroll_waits_ten_seconds_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let (url, server) = cloud("200 OK", r#"{"kind":"enroll","instruction":{}}"#).await;
    let host = FakeHost::default();
    let now = at("2026-10-16T00:00:00Z");
    let next = pass(&host, &mut client(&url), &key(), &paths, now).await;
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
            .any(|line| *line == format!("authorization: bearer {}", OLD.to_ascii_lowercase()))
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
    let next = pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
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

fn sending(token: &str) -> CheckinClient {
    CheckinClient::new("http://127.0.0.1:9", FleetToken::literal(token)).unwrap()
}

/// Leaves a status as if the last check-in was Cloud's first refusal of `token`, dated `ago` back
/// on Cloud's clock.
fn refused_once(paths: &Paths, token: &str, ago: TimeDelta) {
    let at = Utc::now() - ago;
    record(
        &LocalCheckin {
            answer: Seen::Refused,
            reason: None,
            message: Some(REFUSED_ONCE.to_owned()),
            at,
            prep: None,
            blockers: Vec::new(),
            prepare_retries: 0,
            prepare_started: None,
            refused: Some(Refusal {
                token: sending(token).fingerprint(),
                date: at,
            }),
        },
        paths,
    );
}

/// Now on Cloud's clock, to the second its `Date` header carries.
fn cloud_now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(0)
}

/// One check-in Cloud refuses, dating it `date`, made at `now` on this host's clock by a fresh
/// process sending `token`.
async fn refused_pass(
    paths: &Paths,
    token: &str,
    now: DateTime<Utc>,
    date: DateTime<Utc>,
    installing: bool,
) -> Next {
    let (url, _server) = answering_cloud(
        format!("401 Unauthorized\r\nDate: {}", date.to_rfc2822()),
        REFUSAL.to_owned(),
        1,
    )
    .await;
    let mut client = CheckinClient::new(&url, FleetToken::literal(token)).unwrap();
    let host = FakeHost::default();
    if installing {
        install_pass(&host, &mut client, &key(), paths, now).await
    } else {
        pass(&host, &mut client, &key(), paths, now).await
    }
}

fn retries_after(next: &Next, seconds: u64) {
    assert!(
        matches!(next, Next::RefusedOnce { after } if *after == Duration::from_secs(seconds)),
        "{next:?}"
    );
}

#[tokio::test]
async fn a_restart_after_one_refusal_waits_out_the_minute_before_confirming_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    let first = cloud_now();
    let now = Utc::now();
    retries_after(&refused_pass(&paths, OLD, now, first, false).await, 60);
    let early = refused_pass(&paths, OLD, now, first + TimeDelta::seconds(15), false).await;
    retries_after(&early, 45);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.headline().title, "Token refused");
    assert_eq!(status.refused.unwrap().date, first);
    let late = refused_pass(&paths, OLD, now, first + TimeDelta::seconds(60), false).await;
    assert_eq!(
        late,
        Next::Stop(StopReason::TokenRevoked, REVOKED.to_owned())
    );
}

#[tokio::test]
async fn a_token_swapped_in_after_a_refusal_gets_its_own_two_refusals_across_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let first = cloud_now() - TimeDelta::seconds(120);
    let now = Utc::now();
    retries_after(&refused_pass(&paths, OLD, now, first, false).await, 60);
    let swapped = first + TimeDelta::seconds(120);
    retries_after(&refused_pass(&paths, NEW, now, swapped, false).await, 60);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.refused,
        Some(Refusal {
            token: sending(NEW).fingerprint(),
            date: swapped
        })
    );
    let early = refused_pass(&paths, NEW, now, swapped + TimeDelta::seconds(10), false).await;
    retries_after(&early, 50);
    let late = refused_pass(&paths, NEW, now, swapped + TimeDelta::seconds(60), false).await;
    assert!(
        matches!(late, Next::Stop(StopReason::TokenRevoked, _)),
        "{late:?}"
    );
}

#[tokio::test]
async fn an_install_rerun_right_after_a_refusal_does_not_confirm_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let installed = cloud_now();
    let now = Utc::now();
    retries_after(&refused_pass(&paths, OLD, now, installed, true).await, 60);
    let rerun = refused_pass(&paths, OLD, now, installed + TimeDelta::seconds(3), true).await;
    retries_after(&rerun, 57);
}

#[tokio::test]
async fn a_pass_slow_on_this_host_does_not_stretch_the_gap_between_two_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    let date = cloud_now();
    let now = Utc::now();
    let slow = refused_pass(&paths, OLD, now - TimeDelta::seconds(62), date, false).await;
    retries_after(&slow, 60);
    retries_after(&refused_pass(&paths, OLD, now, date, false).await, 60);
    assert!(paths.token_file.exists());
}

#[tokio::test]
async fn this_hosts_clock_jumping_forward_does_not_confirm_a_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    let date = cloud_now();
    let now = Utc::now();
    retries_after(&refused_pass(&paths, OLD, now, date, false).await, 60);
    let jumped = refused_pass(&paths, OLD, now + TimeDelta::seconds(120), date, false).await;
    retries_after(&jumped, 60);
    assert!(paths.token_file.exists());
}

#[tokio::test]
async fn this_hosts_clock_jumping_back_still_confirms_a_minute_of_clouds_time() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    let date = cloud_now() - TimeDelta::seconds(60);
    let now = Utc::now();
    retries_after(&refused_pass(&paths, OLD, now, date, false).await, 60);
    let jumped = refused_pass(
        &paths,
        OLD,
        now - TimeDelta::seconds(100),
        date + TimeDelta::seconds(60),
        false,
    )
    .await;
    assert_eq!(
        jumped,
        Next::Stop(StopReason::TokenRevoked, REVOKED.to_owned())
    );
}

#[tokio::test]
async fn a_first_refusal_dated_ahead_of_cloud_starts_over_instead_of_confirming_or_stalling() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    refused_once(&paths, OLD, TimeDelta::seconds(-3600));
    let date = cloud_now();
    retries_after(
        &refused_pass(&paths, OLD, Utc::now(), date, false).await,
        60,
    );
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.refused.unwrap().date, date);
    assert!(paths.token_file.exists());
    let late = refused_pass(
        &paths,
        OLD,
        Utc::now(),
        date + TimeDelta::seconds(60),
        false,
    )
    .await;
    assert!(
        matches!(late, Next::Stop(StopReason::TokenRevoked, _)),
        "{late:?}"
    );
}

#[tokio::test]
async fn a_first_refusal_dated_too_long_ago_starts_over() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    refused_once(&paths, OLD, TimeDelta::seconds(2 * 3600));
    let date = cloud_now();
    retries_after(
        &refused_pass(&paths, OLD, Utc::now(), date, false).await,
        60,
    );
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.refused.unwrap().date, date);
    assert!(paths.token_file.exists());
}

#[test]
fn status_names_the_refused_token_without_holding_it() {
    let fingerprint = sending(OLD).fingerprint();
    assert_eq!(fingerprint.len(), 16);
    assert_ne!(fingerprint, sending(NEW).fingerprint());
    assert!(!OLD.contains(&fingerprint));
}

#[tokio::test]
async fn one_refusal_keeps_the_token_and_a_success_after_it_clears_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    let host = FakeHost::default();
    let (url, _server) = unauthorized_cloud(4, 1).await;
    let next = pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
    retries_after(&next, 60);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.headline(),
        Headline {
            title: "Token refused",
            ended: false
        }
    );
    assert!(status.says_why());

    let (url, _server) = cloud("200 OK", r#"{"kind":"wait","retryAfter":30}"#).await;
    pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
    let (url, _server) = unauthorized_cloud(4, 1).await;
    let next = pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
    assert!(matches!(next, Next::RefusedOnce { .. }), "{next:?}");
    assert!(paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_second_refusal_in_a_row_stops_as_revoked_and_removes_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    refused_once(&paths, OLD, TimeDelta::seconds(61));
    let (url, server) = unauthorized_cloud(4, 1).await;
    let host = FakeHost::default();
    let end = tokio::time::timeout(
        Duration::from_secs(10),
        run(&host, &mut client(&url), &key(), &paths),
    )
    .await
    .expect("a token refused twice ends the loop");
    assert!(matches!(end, CheckinEnd::Stopped(StopReason::TokenRevoked)));
    assert_eq!(server.await.unwrap().len(), 1);
    assert!(!paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.headline(),
        Headline {
            title: "Token revoked",
            ended: true
        }
    );
}

#[tokio::test]
async fn a_401_that_may_not_be_clouds_keeps_the_token_after_any_number() {
    let gateway = "<html>Authentication required by gateway</html>";
    let date = Utc::now().to_rfc2822();
    for (status, body) in [
        (format!("401 Unauthorized\r\nDate: {date}"), gateway),
        ("401 Unauthorized".to_owned(), REFUSAL),
        ("401 Unauthorized\r\nDate: yesterday".to_owned(), REFUSAL),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
        let host = FakeHost::default();
        for _ in 0..3 {
            let (url, _server) = answering_cloud(status.clone(), body.to_owned(), 1).await;
            let next = pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
            assert!(
                matches!(&next, Next::Retry { after, .. } if *after == Duration::from_secs(60)),
                "{status}: {next:?}"
            );
        }
        let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
        assert_eq!(status.headline().title, "Can't reach Cloud");
        assert!(paths.token_file.exists());
        assert_eq!(host.disables.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn a_401_from_a_skewed_clock_keeps_the_token_and_says_the_clock_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    refused_once(&paths, OLD, TimeDelta::seconds(61));
    let (url, _server) = unauthorized_cloud(-900, 1).await;
    let host = FakeHost::default();
    let next = pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
    assert!(matches!(next, Next::ClockOff(_)), "{next:?}");
    assert!(paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 0);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.headline(),
        Headline {
            title: "Clock is off",
            ended: false
        }
    );
    assert!(status.says_why());
}

#[tokio::test]
async fn a_new_token_written_after_a_refusal_gets_its_own_two_chances() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::write(&paths.token_file, format!("{NEW}\n")).unwrap();
    refused_once(&paths, OLD, TimeDelta::seconds(61));
    let (url, server) = unauthorized_cloud(0, 2).await;
    let host = FakeHost::default();
    let ran = tokio::time::timeout(
        Duration::from_secs(3),
        run(&host, &mut client(&url), &key(), &paths),
    )
    .await;
    assert!(
        ran.is_err(),
        "one refusal of the new token keeps the loop going"
    );
    old_then_new(server).await;
    assert_eq!(fs::read_to_string(&paths.token_file).unwrap().trim(), NEW);
    assert_eq!(host.disables.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_outdated_check_in_waits_an_hour_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let (url, _server) = cloud("413 Payload Too Large", "").await;
    let host = FakeHost::default();
    let Next::Retry { after, .. } =
        pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await
    else {
        panic!("a check-in this Cloud can't read waits for a newer ployz");
    };
    assert_eq!(after, Duration::from_secs(3600));
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.headline().title, "Needs a newer ployz");
}

#[tokio::test]
async fn a_missing_token_file_ends_the_loop_and_disables_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let host = FakeHost::default();
    assert!(loop_token(&host, &paths).await.unwrap().is_none());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
    fs::write(&paths.token_file, format!("{OLD}\n")).unwrap();
    assert!(loop_token(&host, &paths).await.unwrap().is_some());
    fs::write(&paths.token_file, "pmet_abc\n").unwrap();
    assert!(matches!(
        loop_token(&host, &paths).await,
        Err(KeyError::NotAToken { .. })
    ));
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[test]
fn the_check_in_unit_gives_up_after_repeated_failed_starts() {
    let text = unit::checkin(
        Path::new("/usr/bin/ployz"),
        Path::new("/d"),
        Path::new("/t"),
        "https://cloud.example",
    )
    .unwrap();
    let unit_section = text.split("[Service]").next().unwrap();
    assert!(
        unit_section.contains("StartLimitIntervalSec=600\n"),
        "{text}"
    );
    assert!(unit_section.contains("StartLimitBurst=5\n"), "{text}");
}

#[tokio::test(start_paused = true)]
async fn a_probe_that_never_finishes_is_cut_off_and_retried() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let host = FakeHost {
        probe_hangs: true,
        ..FakeHost::default()
    };
    let Next::Retry { after, why } = pass(
        &host,
        &mut client("http://127.0.0.1:9"),
        &key(),
        &paths,
        Utc::now(),
    )
    .await
    else {
        panic!("a hung probe must retry");
    };
    assert_eq!(after, Duration::from_secs(60));
    assert!(why.contains("did not finish in 60s"), "{why}");
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.answer, Seen::ProbeFailed);
}

#[tokio::test(start_paused = true)]
async fn one_failed_preparation_is_retried_once_however_often_the_loop_polls() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"failed","message":"dkms","finished_at":"2026-10-16T00:00:00Z"}}"#,
    )
    .unwrap();
    let host = FakeHost {
        probe_hangs: true,
        ..FakeHost::default()
    };
    let mut unused = client("http://127.0.0.1:9");
    for minute in 20..24 {
        let now = at("2026-10-16T00:00:00Z") + TimeDelta::minutes(minute);
        pass(&host, &mut unused, &key(), &paths, now).await;
    }
    assert_eq!(host.prepares.load(Ordering::SeqCst), 1);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.prepare_retries, 1);
}

#[tokio::test(start_paused = true)]
async fn a_failed_preparation_is_retried_after_each_reboot_up_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"failed","message":"dkms","finished_at":"2026-10-16T00:00:00Z"}}"#,
    )
    .unwrap();
    let mut unused = client("http://127.0.0.1:9");
    let mut prepares = 0;
    for minute in (20..70).step_by(10) {
        let rebooted = FakeHost {
            probe_hangs: true,
            ..FakeHost::default()
        };
        let now = at("2026-10-16T00:00:00Z") + TimeDelta::minutes(minute);
        pass(&rebooted, &mut unused, &key(), &paths, now).await;
        prepares += rebooted.prepares.load(Ordering::SeqCst);
    }
    assert_eq!(prepares, PREPARE_TRIES);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.prepare_retries, PREPARE_TRIES);
}

#[tokio::test(start_paused = true)]
async fn an_explicit_install_restarts_a_preparation_that_used_up_its_retries() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"failed","message":"dkms","finished_at":"2026-10-16T00:00:00Z"}}"#,
    )
    .unwrap();
    let mut unused = client("http://127.0.0.1:9");
    let hangs = || FakeHost {
        probe_hangs: true,
        ..FakeHost::default()
    };
    for minute in (20..70).step_by(10) {
        let now = at("2026-10-16T00:00:00Z") + TimeDelta::minutes(minute);
        pass(&hangs(), &mut unused, &key(), &paths, now).await;
    }
    let spent = hangs();
    pass(
        &spent,
        &mut unused,
        &key(),
        &paths,
        at("2026-10-16T01:30:00Z"),
    )
    .await;
    assert_eq!(spent.prepares.load(Ordering::SeqCst), 0);

    let state = fs::read_to_string(&paths.prepare_state).unwrap();
    let reinstalled = hangs();
    install_pass(
        &reinstalled,
        &mut unused,
        &key(),
        &paths,
        at("2026-10-16T01:31:00Z"),
    )
    .await;
    assert_eq!(reinstalled.prepares.load(Ordering::SeqCst), 0);
    let answered = at("2026-10-16T01:32:00Z");
    restart_preparation(&reinstalled, &paths, answered).await;
    assert_eq!(reinstalled.prepares.load(Ordering::SeqCst), 1);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.prepare_retries, 0);
    assert_eq!(status.prepare_started, Some(answered));
    // ployzd records what it added in the state file, so the restart leaves it alone.
    assert_eq!(fs::read_to_string(&paths.prepare_state).unwrap(), state);
}

#[tokio::test]
async fn an_install_starts_no_preparation_before_cloud_answers_the_token() {
    for (status, body) in [
        ("200 OK", r#"{"kind":"wait","retryAfter":30}"#),
        (
            "401 Unauthorized\r\nDate: {date}",
            r#"{"error":"Authentication is required."}"#,
        ),
        ("404 Not Found", "Not Found"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(&dir);
        let status = status.replace("{date}", &Utc::now().to_rfc2822());
        let (url, _server) = answering_cloud(status, body.to_owned(), 1).await;
        let host = FakeHost::default();
        install_pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
        assert_eq!(host.prepares.load(Ordering::SeqCst), 0, "{body}");
    }
}

#[tokio::test]
async fn a_preparation_that_finished_or_runs_is_not_restarted_by_an_install() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"off"}}"#,
    )
    .unwrap();
    let ready = FakeHost::default();
    restart_preparation(&ready, &paths, Utc::now()).await;
    assert_eq!(ready.prepares.load(Ordering::SeqCst), 0);
    fs::remove_file(&paths.prepare_state).unwrap();
    let running = FakeHost::default();
    running.preparing.store(true, Ordering::SeqCst);
    restart_preparation(&running, &paths, Utc::now()).await;
    assert_eq!(running.prepares.load(Ordering::SeqCst), 0);
    let fresh = FakeHost::default();
    restart_preparation(&fresh, &paths, Utc::now()).await;
    assert_eq!(fresh.prepares.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_url_that_is_not_cloud_is_retried_and_named_in_status() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let (url, _server) = answering_cloud(
        "404 Not Found".to_owned(),
        "<html>Not Found</html>".to_owned(),
        1,
    )
    .await;
    let host = FakeHost::default();
    let Next::NotCloud(why) = pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await
    else {
        panic!("a 404 is not Cloud");
    };
    assert!(
        why.starts_with(&format!("{url}/api/checkin answered 404 Not Found")),
        "{why}"
    );
    assert!(why.ends_with("so it isn't Ployz Cloud"), "{why}");
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.headline(), Headline::open("Not Ployz Cloud"));
    assert!(status.says_why());
}

#[tokio::test]
async fn a_probe_cut_off_takes_the_processes_it_started_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let ployzd = dir.path().join("ployzd");
    let child = dir.path().join("child");
    fs::write(
        &ployzd,
        format!(
            "#!/bin/sh\nsleep 300 &\necho $! > {}\nwait\n",
            child.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&ployzd, fs::Permissions::from_mode(0o755)).unwrap();
    let system = System::with(ployzd);
    let probed = tokio::time::timeout(Duration::from_secs(2), system.probe()).await;
    assert!(probed.is_err(), "the probe should hang until cut off");
    let pid = fs::read_to_string(&child).unwrap();
    let stat = PathBuf::from(format!("/proc/{}/stat", pid.trim()));
    for _ in 0..50 {
        let alive = fs::read_to_string(&stat).is_ok_and(|stat| {
            !stat
                .rsplit(')')
                .next()
                .unwrap_or("")
                .trim_start()
                .starts_with('Z')
        });
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the probe's child {} outlived it", pid.trim());
}

#[tokio::test]
async fn a_cloud_that_does_not_answer_is_retried_in_a_minute() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let host = FakeHost::default();
    let Next::Retry { after, .. } =
        pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await
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
fn preparation_starts_when_missing_and_retries_when_it_ended_long_enough_ago() {
    let now = at("2026-10-16T01:00:00Z");
    assert_eq!(prepare_due(None, None, 0, now), Some(PrepareRun::Start));
    let started = |minutes| Some(now - TimeDelta::minutes(minutes));
    assert_eq!(prepare_due(None, started(5), 0, now), None);
    assert_eq!(
        prepare_due(None, started(10), 0, now),
        Some(PrepareRun::Retry)
    );
    assert_eq!(prepare_due(None, started(10), PREPARE_TRIES, now), None);
    let ready = state(r#"{"docker":{"state":"ready"},"zfs":{"state":"off"}}"#);
    assert_eq!(prepare_due(Some(&ready), Some(now), 0, now), None);

    let installing = state(r#"{"docker":{"state":"ready"},"zfs":{"state":"installing"}}"#);
    let died =
        |written: &str, retries| prepare_due(Some(&installing), Some(at(written)), retries, now);
    assert_eq!(died("2026-10-16T00:55:00Z", 0), None);
    assert_eq!(died("2026-10-16T00:50:00Z", 0), Some(PrepareRun::Retry));
    assert_eq!(died("2026-10-16T00:50:00Z", PREPARE_TRIES), None);

    let failed = |finished: &str| {
        state(&format!(
            r#"{{"docker":{{"state":"ready"}},"zfs":{{"state":"failed","message":"dkms","finished_at":"{finished}"}}}}"#
        ))
    };
    let after = |finished: &str, retries| prepare_due(Some(&failed(finished)), None, retries, now);
    assert_eq!(after("2026-10-16T00:55:00Z", 0), None);
    assert_eq!(after("2026-10-16T00:50:00Z", 0), Some(PrepareRun::Retry));
    assert_eq!(after("2026-10-16T00:50:00Z", 2), Some(PrepareRun::Retry));
    assert_eq!(after("2026-10-16T00:50:00Z", 3), None);
    assert_eq!(
        prepare_due(Some(&failed("2026-10-16T00:50:00Z")), started(5), 0, now),
        None
    );
}

#[tokio::test(start_paused = true)]
async fn a_preparation_that_died_mid_step_is_retried_up_to_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"installing"}}"#,
    )
    .unwrap();
    let written = at("2026-10-16T00:00:00Z");
    fs::File::options()
        .write(true)
        .open(&paths.prepare_state)
        .unwrap()
        .set_modified(written.into())
        .unwrap();
    let mut unused = client("http://127.0.0.1:9");
    let mut prepares = 0;
    for minute in (20..120).step_by(10) {
        let rebooted = FakeHost {
            probe_hangs: true,
            ..FakeHost::default()
        };
        pass(
            &rebooted,
            &mut unused,
            &key(),
            &paths,
            written + TimeDelta::minutes(minute),
        )
        .await;
        prepares += rebooted.prepares.load(Ordering::SeqCst);
    }
    assert_eq!(prepares, PREPARE_TRIES);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.prepare_retries, PREPARE_TRIES);
}

#[tokio::test(start_paused = true)]
async fn a_start_that_writes_no_state_waits_out_the_retry_delay() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let mut unused = client("http://127.0.0.1:9");
    let started = at("2026-10-16T00:00:00Z");
    let mut prepares = 0;
    for minute in 0..=10 {
        let rebooted = FakeHost {
            probe_hangs: true,
            ..FakeHost::default()
        };
        pass(
            &rebooted,
            &mut unused,
            &key(),
            &paths,
            started + TimeDelta::minutes(minute),
        )
        .await;
        prepares += rebooted.prepares.load(Ordering::SeqCst);
        let expected = if minute < 10 { 1 } else { 2 };
        assert_eq!(prepares, expected, "minute {minute}");
    }
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.prepare_retries, 1);
}

#[tokio::test(start_paused = true)]
async fn a_retry_that_leaves_an_old_state_file_waits_out_the_retry_delay() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"installing"}}"#,
    )
    .unwrap();
    let written = at("2026-10-16T00:00:00Z");
    fs::File::options()
        .write(true)
        .open(&paths.prepare_state)
        .unwrap()
        .set_modified(written.into())
        .unwrap();
    let mut unused = client("http://127.0.0.1:9");
    let mut prepares = 0;
    for minute in 60..70 {
        let rebooted = FakeHost {
            probe_hangs: true,
            ..FakeHost::default()
        };
        pass(
            &rebooted,
            &mut unused,
            &key(),
            &paths,
            written + TimeDelta::minutes(minute),
        )
        .await;
        prepares += rebooted.prepares.load(Ordering::SeqCst);
    }
    assert_eq!(prepares, 1);
}

#[tokio::test(start_paused = true)]
async fn a_preparation_that_finishes_while_its_unit_is_checked_reads_as_finished() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"ready"},"zfs":{"state":"installing"}}"#,
    )
    .unwrap();
    let written = at("2026-10-16T00:00:00Z");
    fs::File::options()
        .write(true)
        .open(&paths.prepare_state)
        .unwrap()
        .set_modified(written.into())
        .unwrap();
    let host = FakeHost {
        probe_hangs: true,
        finishes: Some((
            paths.prepare_state.clone(),
            r#"{"docker":{"state":"ready"},"zfs":{"state":"ready"}}"#,
        )),
        ..FakeHost::default()
    };
    let mut unused = client("http://127.0.0.1:9");
    pass(
        &host,
        &mut unused,
        &key(),
        &paths,
        written + TimeDelta::hours(1),
    )
    .await;
    assert_eq!(host.prepares.load(Ordering::SeqCst), 0);
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(
        status.prep,
        Some(Prep {
            docker: DockerPrep::Ready,
            zfs: ZfsPrep::Ready
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_step_left_installing_after_the_last_retry_reads_as_failed() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"installing"},"zfs":{"state":"installing"}}"#,
    )
    .unwrap();
    let written = at("2026-10-16T00:00:00Z");
    fs::File::options()
        .write(true)
        .open(&paths.prepare_state)
        .unwrap()
        .set_modified(written.into())
        .unwrap();
    let mut unused = client("http://127.0.0.1:9");
    for minute in (20..80).step_by(10) {
        let rebooted = FakeHost {
            probe_hangs: true,
            ..FakeHost::default()
        };
        pass(
            &rebooted,
            &mut unused,
            &key(),
            &paths,
            written + TimeDelta::minutes(minute),
        )
        .await;
    }
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.prepare_retries, PREPARE_TRIES);
    assert_eq!(
        status.prep,
        Some(Prep {
            docker: DockerPrep::Failed,
            zfs: ZfsPrep::Failed
        })
    );
    assert_ne!(status.headline().title, "Installing Docker");
    assert_ne!(status.headline().title, "Building ZFS");
}

#[tokio::test(start_paused = true)]
async fn a_spent_preparation_tells_cloud_its_steps_failed_and_how_to_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
    fs::write(
        &paths.prepare_state,
        r#"{"docker":{"state":"installing"},"zfs":{"state":"installing"}}"#,
    )
    .unwrap();
    record(
        &LocalCheckin {
            answer: Seen::Wait,
            reason: None,
            message: None,
            at: Utc::now(),
            prep: None,
            blockers: Vec::new(),
            prepare_retries: PREPARE_TRIES,
            prepare_started: Some(Utc::now() - TimeDelta::hours(1)),
            refused: None,
        },
        &paths,
    );
    let (url, server) = cloud("200 OK", r#"{"kind":"wait","retryAfter":30}"#).await;
    let host = FakeHost::default();
    pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
    let request = server.await.unwrap();
    assert!(request.contains("docker_install_failed"), "{request}");
    assert!(
        request.contains("sudo ployz server checkin --install"),
        "{request}"
    );
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.headline().title, "Blocked");
    assert_eq!(host.prepares.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_older_ployzd_blocks_the_server_in_cloud_and_in_status() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let stale =
        "/usr/local/bin/ployzd is older than this ployz. Run sudo ployz server checkin --install";
    let (url, server) = cloud("200 OK", r#"{"kind":"wait","retryAfter":30}"#).await;
    let host = FakeHost {
        stale: Some(stale.to_owned()),
        ..FakeHost::default()
    };
    pass(&host, &mut client(&url), &key(), &paths, Utc::now()).await;
    let request = server.await.unwrap();
    assert!(request.contains(r#""code":"ployzd_outdated""#), "{request}");
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.headline().title, "Blocked");
    assert!(status.blockers.iter().any(|issue| issue.message == stale));
    assert_eq!(
        host.prepares.load(Ordering::SeqCst),
        0,
        "an older ployzd never prepares"
    );

    let host = FakeHost {
        stale: Some(stale.to_owned()),
        probe_hangs: true,
        ..FakeHost::default()
    };
    tokio::time::pause();
    let mut unused = client("http://127.0.0.1:9");
    pass(&host, &mut unused, &key(), &paths, Utc::now()).await;
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.answer, Seen::ProbeFailed);
    assert!(
        status.blockers.iter().any(|issue| issue.message == stale),
        "a probe the older ployzd fails still says why"
    );
}

#[tokio::test(start_paused = true)]
async fn a_running_preparation_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(&dir);
    let host = FakeHost {
        probe_hangs: true,
        preparing: AtomicBool::new(true),
        ..FakeHost::default()
    };
    let mut unused = client("http://127.0.0.1:9");
    pass(&host, &mut unused, &key(), &paths, Utc::now()).await;
    assert_eq!(host.prepares.load(Ordering::SeqCst), 0);
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
        prepare_started: None,
        refused: None,
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
