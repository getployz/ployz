//! The loop the unit starts must run the ployzd `--install` chose.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

use chrono::Utc;
use clap::{ArgMatches, Command};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::{LoopPloyzd, command, first_pass, loop_ployzd, loop_start};
use crate::checkin::{
    self, CheckinClient, CheckinKey, FleetToken, Host, LocalCheckin, Next, Paths, Probe, Seen,
    StopReason, System, unit,
};

const PROBE: &str = r#"{"facts":{"os":{"id":"ubuntu","version_id":"24.04"},"kernel":"6.8.0","arch":"x86_64","virt":"BareMetal","cgroup":"V2","cpu_count":4,"memory_total_bytes":8589934592,"disks":[],"docker":"Absent"},"assessment":{"blockers":[],"warnings":[],"zfs":{"Ineligible":{"reason":{"code":"zfs_off","message":"Off."}}}}}"#;

fn exec_start(text: &str) -> Vec<String> {
    let line = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .unwrap();
    line.split('"')
        .enumerate()
        .flat_map(|(at, part)| {
            if at % 2 == 1 {
                vec![part.to_owned()]
            } else {
                part.split_whitespace().map(str::to_owned).collect()
            }
        })
        .collect()
}

/// A ployzd that answers `version` with `version`.
fn daemon(path: &Path, version: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, format!("#!/bin/sh\necho {version}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A ployzd that answers `version` with `version` and the host probe with a probe.
fn probing_daemon(path: &Path, version: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = install ]; then echo '{PROBE}'; else echo {version}; fi\n"
        ),
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn temp_paths(dir: &tempfile::TempDir) -> Paths {
    Paths {
        checkin_dir: dir.path().join("checkin"),
        prepare_state: dir.path().join("prepare/state.json"),
        token_file: dir.path().join("fleet-token"),
    }
}

fn pinned(path: &Path) -> ArgMatches {
    command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(["--ployzd", path.to_str().unwrap()])
        .unwrap()
}

#[tokio::test]
async fn the_unit_starts_the_loop_with_the_ployzd_install_chose() {
    let dir = tempfile::tempdir().unwrap();
    let chosen = dir.path().join("checkin/ployzd");
    daemon(&chosen, env!("CARGO_PKG_VERSION"));
    let chosen = chosen.as_path();
    let text = unit::checkin(
        Path::new("/usr/local/bin/ployz"),
        chosen,
        Path::new("/etc/ployz/fleet-token"),
        "https://cloud.example",
    )
    .unwrap();
    let args = exec_start(&text);
    assert_eq!(args.get(1..3).unwrap(), ["server", "checkin"]);
    let matches = command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(args.get(3..).unwrap())
        .unwrap();
    let paths = Paths::system(PathBuf::from("/etc/ployz/fleet-token"));
    assert_eq!(
        loop_ployzd(&matches, &paths, None).await.unwrap(),
        current(chosen)
    );
}

fn current(path: &Path) -> LoopPloyzd {
    LoopPloyzd {
        path: path.to_owned(),
        stale: None,
    }
}

#[tokio::test]
async fn a_pinned_ployzd_that_is_gone_or_stale_gives_way_to_a_current_one_without_a_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let bin = dir.path().join("bin");
    let on_path = bin.join("ployzd");
    let copy = paths.checkin_dir.join("ployzd");
    let stale = dir.path().join("old/ployzd");
    daemon(&stale, "0.0.1");
    let search = bin.clone().into_os_string();
    let missing = dir.path().join("gone/ployzd");

    daemon(&copy, env!("CARGO_PKG_VERSION"));
    for pin in [&stale, &missing] {
        assert_eq!(
            loop_ployzd(&pinned(pin), &paths, Some(&search))
                .await
                .unwrap(),
            current(&copy)
        );
    }
    daemon(&on_path, env!("CARGO_PKG_VERSION"));
    assert_eq!(
        loop_ployzd(&pinned(&stale), &paths, Some(&search))
            .await
            .unwrap(),
        current(&on_path)
    );
}

#[tokio::test]
async fn with_no_current_ployzd_the_loop_runs_an_older_one_and_says_to_reinstall() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let bin = dir.path().join("bin");
    let search = bin.clone().into_os_string();
    let pin = dir.path().join("old/ployzd");
    let copy = paths.checkin_dir.join("ployzd");
    daemon(&pin, "0.0.1");
    daemon(&bin.join("ployzd"), "0.0.2");
    daemon(&copy, "0.0.3");

    let found = loop_ployzd(&pinned(&pin), &paths, Some(&search))
        .await
        .unwrap();
    assert_eq!(found.path, pin, "the pin still comes first");
    let stale = found.stale.unwrap();
    assert!(stale.contains("is older than this ployz"), "{stale}");
    assert!(
        stale.ends_with("Run sudo ployz server checkin --install"),
        "{stale}"
    );

    let gone = dir.path().join("gone/ployzd");
    fs::remove_file(bin.join("ployzd")).unwrap();
    let found = loop_ployzd(&pinned(&gone), &paths, Some(&search))
        .await
        .unwrap();
    assert_eq!(found.path, copy);
    assert!(found.stale.is_some());
    assert!(!dir.path().join("gone").exists(), "the loop never fetches");
}

/// The chosen ployzd, probed for real, on a host whose preparation must not start.
struct Chosen(System);

impl Host for Chosen {
    async fn probe(&self) -> Result<Probe, String> {
        self.0.probe().await
    }

    fn hostname(&self) -> String {
        "waiting-1".to_owned()
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        Ok(false)
    }

    async fn start_prepare(&self) -> Result<(), String> {
        panic!("an older ployzd never prepares")
    }

    async fn disable_loop(&self) -> Result<(), String> {
        unreachable!("the check-in waits")
    }

    fn stale_ployzd(&self) -> Option<String> {
        self.0.stale_ployzd()
    }
}

#[tokio::test]
async fn a_stale_pin_that_fails_its_probe_gives_way_to_an_older_ployzd_that_answers() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let bin = dir.path().join("bin");
    let search = bin.clone().into_os_string();
    let pin = dir.path().join("old/ployzd");
    let on_path = bin.join("ployzd");
    daemon(&pin, "0.0.1");
    probing_daemon(&on_path, "0.0.2");

    let found = loop_ployzd(&pinned(&pin), &paths, Some(&search))
        .await
        .unwrap();
    assert_eq!(found.path, on_path);
    let host = Chosen(System {
        ployzd: found.path,
        stale: found.stale,
    });

    fs::write(
        &paths.token_file,
        "pflt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = vec![0; 16384];
        loop {
            let read = stream.read(&mut buf).await.unwrap();
            request.extend_from_slice(buf.get(..read).unwrap());
            let text = String::from_utf8_lossy(&request).to_lowercase();
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|length| length.trim().parse::<usize>().ok())
                .unwrap_or_default();
            if read == 0 || body.len() >= length {
                break;
            }
        }
        let body = r#"{"kind":"wait","retryAfter":30}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    let token = FleetToken::read(&paths.token_file).unwrap();
    let key = CheckinKey::load_or_create(&paths.key()).unwrap();
    let mut client = CheckinClient::new(&url, token).unwrap();
    checkin::pass(&host, &mut client, &key, &paths, Utc::now()).await;
    let request = server.await.unwrap();
    assert!(request.contains(r#""code":"ployzd_outdated""#), "{request}");
    assert!(request.contains(r#""kernel":"6.8.0""#), "{request}");
}

#[derive(Default)]
struct Stopper {
    disables: AtomicU32,
}

impl Host for Stopper {
    async fn probe(&self) -> Result<Probe, String> {
        unreachable!("the loop never starts")
    }

    fn hostname(&self) -> String {
        unreachable!("the loop never starts")
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        unreachable!("the loop never starts")
    }

    async fn start_prepare(&self) -> Result<(), String> {
        unreachable!("the loop never starts")
    }

    async fn disable_loop(&self) -> Result<(), String> {
        self.disables.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stale_ployzd(&self) -> Option<String> {
        unreachable!("the loop never starts")
    }
}

#[tokio::test]
async fn a_loop_without_a_token_ends_before_it_looks_for_ployzd() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let search = dir.path().join("bin").into_os_string();
    let stopper = Stopper::default();
    let started = loop_start(
        &stopper,
        &pinned(&dir.path().join("gone/ployzd")),
        &paths,
        Some(&search),
    )
    .await;
    assert!(matches!(started, Ok(None)), "{:?}", started.err());
    assert_eq!(stopper.disables.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_loop_with_no_ployzd_at_all_says_so_in_status_before_it_exits() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    fs::write(
        &paths.token_file,
        "pflt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let search = dir.path().join("bin").into_os_string();
    let stopper = Stopper::default();
    let started = loop_start(
        &stopper,
        &pinned(&dir.path().join("gone/ployzd")),
        &paths,
        Some(&search),
    )
    .await;
    let error = started.err().unwrap();
    assert!(
        format!("{error:?}").contains("Run sudo ployz server checkin --install"),
        "{error:?}"
    );
    assert_eq!(
        stopper.disables.load(Ordering::SeqCst),
        0,
        "systemd retries it"
    );
    let status = LocalCheckin::read(&paths.status()).unwrap().unwrap();
    assert_eq!(status.answer, Seen::NoPloyzd);
    assert_eq!(status.headline().title, "No ployzd");
    assert!(status.says_why());
    assert!(
        status
            .message
            .unwrap()
            .starts_with("No ployzd on PATH or at "),
        "the status names what is missing"
    );
}

#[test]
fn an_install_ends_with_an_error_when_cloud_refuses_the_token() {
    let refused = first_pass(
        Next::Refused {
            after: std::time::Duration::from_secs(60),
        },
        "cloud.example.com",
    )
    .unwrap_err();
    assert!(
        refused
            .to_string()
            .contains("Cloud at cloud.example.com refused this Fleet Token"),
        "{refused}"
    );
}

#[test]
fn an_install_stopped_by_its_token_says_to_run_it_again_with_a_new_one() {
    for (reason, again) in [
        (StopReason::TokenExpired, true),
        (StopReason::TokenRevoked, true),
        (StopReason::Rejected, false),
    ] {
        let stopped = first_pass(Next::Stop(reason, "Said Cloud.".to_owned()), "ployz.dev")
            .unwrap_err()
            .to_string();
        assert!(
            stopped.contains(&format!("{}. Said Cloud.", reason.title())),
            "{stopped}"
        );
        assert_eq!(
            stopped.contains("Run the install again with a new token."),
            again,
            "{stopped}"
        );
    }
}

#[test]
fn an_install_that_waits_or_cannot_reach_cloud_leaves_the_loop_running() {
    let after = std::time::Duration::from_secs(60);
    assert!(first_pass(Next::Wait { after, log: None }, "ployz.dev").is_ok());
    let why = "Could not reach Ployz Cloud: refused.".to_owned();
    assert!(first_pass(Next::Retry { after, why }, "ployz.dev").is_ok());
}
