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

use super::{
    Answered, Install, LoopPloyzd, Token, answer_install, command, install_outcome, install_start,
    keep_answered_token, loop_ployzd, loop_start, replaces_token, stdin_token,
};
use crate::checkin::{
    self, CheckinClient, CheckinKey, FleetToken, Hold, Host, LocalCheckin, Next, Paths, Probe,
    Seen, StopReason, System, unit,
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

/// Answers one check-in with `status` and `body` once `meanwhile` has run, and returns its request.
async fn answering_once(
    status: &'static str,
    body: &'static str,
    meanwhile: impl FnOnce() + Send + 'static,
) -> (String, tokio::task::JoinHandle<String>) {
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
        meanwhile();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8_lossy(&request).into_owned()
    });
    (url, server)
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

    async fn enroll(
        &self,
        _: &crate::checkin::EnrollInstruction,
        _: &dyn Fn(crate::cloud_enroll::EnrollStage),
    ) -> Result<crate::checkin::Committed, String> {
        unreachable!("the check-in never enrolls here")
    }

    async fn participating(&self) -> bool {
        false
    }

    async fn discard_zfs_build(&self) -> Result<(), String> {
        unreachable!("the check-in never enrolls here")
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
        cloud: String::new(),
    });

    fs::write(
        &paths.token_file,
        "pflt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let (url, server) = answering_once("200 OK", r#"{"kind":"wait","retryAfter":30}"#, || {}).await;
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

    async fn enroll(
        &self,
        _: &crate::checkin::EnrollInstruction,
        _: &dyn Fn(crate::cloud_enroll::EnrollStage),
    ) -> Result<crate::checkin::Committed, String> {
        unreachable!("the check-in never enrolls here")
    }

    async fn participating(&self) -> bool {
        false
    }

    async fn discard_zfs_build(&self) -> Result<(), String> {
        unreachable!("the check-in never enrolls here")
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

const PIPED: Install<'static> = Install {
    cloud_url: "ployz.dev",
    url_source: None,
    token: Token::Piped,
};
const FROM_FILE: Install<'static> = Install {
    token: Token::InFile,
    ..PIPED
};

#[test]
fn an_install_ends_with_an_error_when_cloud_refuses_the_token() {
    let refused = |url_source| {
        install_outcome(
            Next::RefusedOnce {
                after: std::time::Duration::from_secs(60),
            },
            Install {
                cloud_url: "cloud.example.com",
                url_source,
                ..PIPED
            },
        )
        .unwrap_err()
        .to_string()
    };
    let unset = refused(None);
    assert!(
        unset.contains(
            "Cloud at cloud.example.com refused this Fleet Token. Check the token, then run the install again."
        ),
        "{unset}"
    );
    assert!(!unset.contains("PLOYZ_CLOUD_URL"), "{unset}");
    let set = refused(Some("PLOYZ_CLOUD_URL"));
    assert!(
        set.contains(
            "Cloud at cloud.example.com refused this Fleet Token. Check the token and PLOYZ_CLOUD_URL, then run the install again."
        ),
        "{set}"
    );
}

#[test]
fn an_install_stopped_by_its_token_says_to_run_it_again_with_a_new_one() {
    for (reason, again) in [
        (StopReason::TokenExpired, true),
        (StopReason::TokenRevoked, true),
        (StopReason::Rejected, false),
    ] {
        let stopped = install_outcome(Next::Stop(reason, "Said Cloud.".to_owned()), PIPED)
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
fn an_install_says_waiting_only_when_cloud_answered_the_token() {
    let after = std::time::Duration::from_secs(60);
    assert_eq!(
        install_outcome(Next::Wait { after }, PIPED).unwrap(),
        Answered::Waiting
    );
    let why = "Cloud held this check-in back: This Fleet Token already has 5 waiting servers.";
    assert_eq!(
        install_outcome(
            Next::Held {
                after,
                hold: Hold::Full,
                why: why.to_owned()
            },
            PIPED
        )
        .unwrap(),
        Answered::Held(Hold::Full, why.to_owned())
    );
    let why = "Could not reach Ployz Cloud: refused.".to_owned();
    assert_eq!(
        install_outcome(
            Next::Retry {
                after,
                why: why.clone()
            },
            PIPED
        )
        .unwrap(),
        Answered::Unconfirmed(why)
    );
}

#[test]
fn an_install_that_reaches_something_other_than_cloud_names_the_setting_only_when_set() {
    let why = "http://127.0.0.1:9/api/checkin answered 404 Not Found, which isn't how Ployz Cloud answers. A proxy or firewall in between may have answered instead";
    let not_cloud = |setting| {
        install_outcome(
            Next::NotCloud(why.to_owned()),
            Install {
                url_source: setting,
                ..PIPED
            },
        )
        .unwrap_err()
        .to_string()
    };
    let unset = not_cloud(None);
    assert!(
        unset.contains(&format!(
            "{why}. Run the install again once this host reaches Ployz Cloud."
        )),
        "{unset}"
    );
    assert!(!unset.contains("PLOYZ_CLOUD_URL"), "{unset}");
    for setting in ["PLOYZ_CLOUD_URL", "--cloud-url"] {
        let set = not_cloud(Some(setting));
        assert!(
            set.contains(&format!(
                "{why}, or {setting} doesn't name Ployz Cloud. Check {setting}, then run the install again."
            )),
            "{set}"
        );
    }
    let clock = install_outcome(Next::ClockOff("Clock off".to_owned()), PIPED)
        .unwrap_err()
        .to_string();
    assert!(
        clock.contains("Clock off, then run the install again."),
        "{clock}"
    );
}

const OLD_TOKEN: &str = "pflt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const NEW_TOKEN: &str = "pflt_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";

fn token_file_holding(dir: &tempfile::TempDir, token: &str) -> PathBuf {
    let token_file = dir.path().join("etc/fleet-token");
    fs::create_dir_all(token_file.parent().unwrap()).unwrap();
    fs::write(&token_file, format!("{token}\n")).unwrap();
    token_file
}

fn sending(token: &str) -> CheckinClient {
    CheckinClient::new("http://127.0.0.1:9", FleetToken::parse(token).unwrap()).unwrap()
}

#[test]
fn a_token_from_stdin_replaces_the_old_one_only_once_cloud_answers_it() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = token_file_holding(&dir, OLD_TOKEN);
    let candidate = sending(NEW_TOKEN);
    let after = std::time::Duration::from_secs(60);
    for unanswered in [
        Next::RefusedOnce { after },
        Next::Stop(StopReason::TokenExpired, "Expired.".to_owned()),
        Next::NotCloud("answered 404 Not Found".to_owned()),
        Next::ClockOff("Clock off".to_owned()),
        Next::Retry {
            after,
            why: "Could not reach Ployz Cloud: refused.".to_owned(),
        },
        Next::Held {
            after,
            hold: Hold::Full,
            why:
                "Cloud held this check-in back: This Fleet Token already has 1000 waiting servers."
                    .to_owned(),
        },
        Next::Held {
            after,
            hold: Hold::Busy,
            why: "Cloud held this check-in back: Too many check-ins.".to_owned(),
        },
    ] {
        assert!(keep_answered_token(unanswered, &candidate, &token_file, PIPED).is_err());
        assert_eq!(
            fs::read_to_string(&token_file).unwrap(),
            format!("{OLD_TOKEN}\n")
        );
    }
    keep_answered_token(Next::Wait { after }, &candidate, &token_file, PIPED).unwrap();
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        format!("{NEW_TOKEN}\n")
    );
    let mode = fs::metadata(&token_file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_rerun_cloud_cannot_answer_or_holds_back_keeps_the_old_token_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = token_file_holding(&dir, OLD_TOKEN);
    let after = std::time::Duration::from_secs(60);
    for (why, next) in [
        "Could not reach Ployz Cloud: error sending request.",
        "Could not probe this server: ployzd install --probe did not finish in 30s.",
        "Cloud held this check-in back: Too many check-ins.",
        "Cloud held this check-in back: Busy, retry.",
    ]
    .map(|why| {
        let next = if why.starts_with("Cloud held") {
            Next::Held {
                after,
                hold: Hold::Busy,
                why: why.to_owned(),
            }
        } else {
            Next::Retry {
                after,
                why: why.to_owned(),
            }
        };
        (why, next)
    }) {
        let kept = keep_answered_token(next, &sending(NEW_TOKEN), &token_file, PIPED)
            .unwrap_err()
            .to_string();
        assert!(
            kept.contains(&format!(
                "{why} Until Cloud takes the new Fleet Token, this install keeps {} and changes nothing else. Run the install again.",
                token_file.display()
            )),
            "{kept}"
        );
        assert_eq!(
            fs::read_to_string(&token_file).unwrap(),
            format!("{OLD_TOKEN}\n")
        );
    }
}

#[test]
fn a_rerun_with_a_token_that_has_all_its_waiting_servers_keeps_the_old_token() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = token_file_holding(&dir, OLD_TOKEN);
    let why = "Cloud held this check-in back: This Fleet Token already has 1000 waiting servers.";
    let full = || Next::Held {
        after: std::time::Duration::from_secs(1),
        hold: Hold::Full,
        why: why.to_owned(),
    };
    let kept = keep_answered_token(full(), &sending(NEW_TOKEN), &token_file, PIPED)
        .unwrap_err()
        .to_string();
    assert!(
        kept.contains(&format!(
            "{why} Cloud didn't take the new Fleet Token, so this install keeps {} and changes nothing else. Run the install again with another token, or revoke this one to clear its waiting servers.",
            token_file.display()
        )),
        "{kept}"
    );
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        format!("{OLD_TOKEN}\n")
    );
    let fresh = dir.path().join("new/fleet-token");
    assert_eq!(
        keep_answered_token(full(), &sending(NEW_TOKEN), &fresh, PIPED).unwrap(),
        Answered::Held(Hold::Full, why.to_owned())
    );
    assert_eq!(
        fs::read_to_string(&fresh).unwrap(),
        format!("{NEW_TOKEN}\n")
    );
}

#[test]
fn a_first_install_or_the_same_token_keeps_trying_while_cloud_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let after = std::time::Duration::from_secs(60);
    let offline = || Next::Retry {
        after,
        why: "Could not reach Ployz Cloud: refused.".to_owned(),
    };
    let fresh = dir.path().join("new/fleet-token");
    assert!(matches!(
        keep_answered_token(offline(), &sending(NEW_TOKEN), &fresh, PIPED).unwrap(),
        Answered::Unconfirmed(_)
    ));
    assert_eq!(
        fs::read_to_string(&fresh).unwrap(),
        format!("{NEW_TOKEN}\n")
    );
    let same = token_file_holding(&dir, NEW_TOKEN);
    keep_answered_token(offline(), &sending(NEW_TOKEN), &same, PIPED).unwrap();
    assert_eq!(fs::read_to_string(&same).unwrap(), format!("{NEW_TOKEN}\n"));
}

#[test]
fn only_a_missing_token_file_makes_a_first_install() {
    let dir = tempfile::tempdir().unwrap();
    let new = sending(NEW_TOKEN);
    let replaces = |path: &Path| replaces_token(path, |held| new.sends(held));
    assert!(!replaces(&dir.path().join("new/fleet-token")).unwrap());
    assert!(!replaces(&token_file_holding(&dir, NEW_TOKEN)).unwrap());
    assert!(replaces(&token_file_holding(&dir, "not a token")).unwrap());
    assert!(replaces(&token_file_holding(&dir, OLD_TOKEN)).unwrap());
    let unreadable = dir.path().join("a-directory");
    fs::create_dir(&unreadable).unwrap();
    let error = replaces(&unreadable).unwrap_err().to_string();
    assert!(
        error.contains(&format!(
            "Could not read {}, so this install can't tell whether it replaces a Fleet Token.",
            unreadable.display()
        )),
        "{error}"
    );
}

#[test]
fn a_rerun_over_a_file_that_isnt_a_token_swaps_it_only_once_cloud_takes_the_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = token_file_holding(&dir, "not a token");
    let after = std::time::Duration::from_secs(60);
    for unanswered in [
        Next::Retry {
            after,
            why: "Could not reach Ployz Cloud: refused.".to_owned(),
        },
        Next::Held {
            after,
            hold: Hold::Busy,
            why: "Cloud held this check-in back: Busy, retry.".to_owned(),
        },
        Next::Held {
            after,
            hold: Hold::Full,
            why: "Cloud held this check-in back: This Fleet Token already has 5 waiting servers."
                .to_owned(),
        },
    ] {
        let kept = keep_answered_token(unanswered, &sending(NEW_TOKEN), &token_file, PIPED)
            .unwrap_err()
            .to_string();
        assert!(
            kept.contains(&format!("this install keeps {}", token_file.display())),
            "{kept}"
        );
        assert_eq!(fs::read_to_string(&token_file).unwrap(), "not a token\n");
    }
    keep_answered_token(
        Next::Wait { after },
        &sending(NEW_TOKEN),
        &token_file,
        PIPED,
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        format!("{NEW_TOKEN}\n")
    );
}

#[tokio::test]
async fn an_install_keeps_a_token_another_install_wrote_while_cloud_was_answering() {
    for (status, body) in [
        ("502 Bad Gateway", ""),
        (
            "429 Too Many Requests",
            r#"{"error":"This Fleet Token already has 1000 waiting servers."}"#,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        let ployzd = dir.path().join("bin/ployzd");
        probing_daemon(&ployzd, "0.0.1");
        let host = Chosen(System {
            ployzd,
            stale: Some("Older.".to_owned()),
            cloud: String::new(),
        });
        let landed = paths.token_file.clone();
        let other_status = paths.status();
        let (url, server) = answering_once(status, body, move || {
            fs::write(landed, format!("{OLD_TOKEN}\n")).unwrap();
            fs::write(other_status, OTHER_STATUS).unwrap();
        })
        .await;
        let key = CheckinKey::load_or_create(&paths.key()).unwrap();
        let mut client = CheckinClient::new(&url, FleetToken::parse(NEW_TOKEN).unwrap()).unwrap();
        assert!(!replaces_token(&paths.token_file, |held| client.sends(held)).unwrap());
        let pass = checkin::install_pass(&host, &mut client, &key, &paths, Utc::now()).await;
        server.await.unwrap();
        let kept = answer_install(pass, &client, &paths, PIPED)
            .unwrap_err()
            .to_string();
        // The other install's token keeps its status, and with it any enrollment.
        assert_eq!(
            fs::read_to_string(paths.status()).unwrap(),
            OTHER_STATUS,
            "{status}"
        );
        assert!(
            kept.contains(&format!(
                "this install keeps {}",
                paths.token_file.display()
            )),
            "{status}: {kept}"
        );
        assert_eq!(
            fs::read_to_string(&paths.token_file).unwrap(),
            format!("{OLD_TOKEN}\n"),
            "{status}"
        );
    }
}

const OTHER_STATUS: &str = "the status another install's token earned\n";

#[tokio::test]
async fn an_install_records_what_cloud_said_only_for_the_token_it_keeps() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let ployzd = dir.path().join("bin/ployzd");
    probing_daemon(&ployzd, "0.0.1");
    let host = Chosen(System {
        ployzd,
        stale: Some("Older.".to_owned()),
        cloud: String::new(),
    });
    let key = CheckinKey::load_or_create(&paths.key()).unwrap();
    fs::create_dir_all(paths.token_file.parent().unwrap()).unwrap();
    fs::create_dir_all(paths.status().parent().unwrap()).unwrap();
    let full = r#"{"error":"This Fleet Token already has 1000 waiting servers."}"#;
    for (held, kept) in [(OLD_TOKEN, false), (NEW_TOKEN, true)] {
        fs::write(&paths.token_file, format!("{held}\n")).unwrap();
        fs::write(paths.status(), OTHER_STATUS).unwrap();
        let (url, server) = answering_once("429 Too Many Requests", full, || {}).await;
        let mut client = CheckinClient::new(&url, FleetToken::parse(NEW_TOKEN).unwrap()).unwrap();
        let pass = checkin::install_pass(&host, &mut client, &key, &paths, Utc::now()).await;
        server.await.unwrap();
        assert_eq!(answer_install(pass, &client, &paths, PIPED).is_ok(), kept);
        let status = fs::read_to_string(paths.status()).unwrap();
        if kept {
            let seen = LocalCheckin::read(&paths.status()).unwrap().unwrap();
            assert!(seen.token_fingerprint.is_some(), "{status}");
        } else {
            assert_eq!(status, OTHER_STATUS, "{held}");
        }
    }
}

const ENROLL: &str = r#"{"kind":"enroll","attemptId":"att_1","token":"pmet_never_written_down","role":"join","name":"web-1","storage":{"kind":"none"},"policy":{"labels":{},"acceptsBuilds":true,"acceptsServices":true,"acceptsIngress":true}}"#;

#[tokio::test]
async fn an_install_cloud_already_enrolls_does_not_say_it_waits_for_approval() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let ployzd = dir.path().join("bin/ployzd");
    probing_daemon(&ployzd, "0.0.1");
    let host = Chosen(System {
        ployzd,
        stale: Some("Older.".to_owned()),
        cloud: String::new(),
    });
    let key = CheckinKey::load_or_create(&paths.key()).unwrap();
    fs::create_dir_all(paths.status().parent().unwrap()).unwrap();
    let (url, server) = answering_once("200 OK", ENROLL, || {}).await;
    let mut client = CheckinClient::new(&url, FleetToken::parse(NEW_TOKEN).unwrap()).unwrap();

    let pass = checkin::install_pass(&host, &mut client, &key, &paths, Utc::now()).await;
    server.await.unwrap();
    let answered = answer_install(pass, &client, &paths, PIPED).unwrap();

    assert_eq!(answered, Answered::Enrolling);
}

#[test]
fn an_install_waits_for_another_to_finish_writing_the_token_before_it_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    let token_file = paths.token_file.clone();
    fs::create_dir_all(token_file.parent().unwrap()).unwrap();
    let other = fs::File::open(token_file.parent().unwrap()).unwrap();
    rustix::fs::flock(&other, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let install = std::thread::spawn(move || {
        let next = Next::Retry {
            after: std::time::Duration::from_secs(60),
            why: "Could not reach Ployz Cloud: refused.".to_owned(),
        };
        let pass = checkin::InstallPass { next, seen: None };
        answer_install(pass, &sending(NEW_TOKEN), &paths, PIPED).map_err(|error| error.to_string())
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(
        !install.is_finished(),
        "the install wrote while another held the lock"
    );
    fs::write(&token_file, format!("{OLD_TOKEN}\n")).unwrap();
    drop(other);
    let kept = install.join().unwrap().unwrap_err();
    assert!(
        kept.contains(&format!("this install keeps {}", token_file.display())),
        "{kept}"
    );
    assert_eq!(
        fs::read_to_string(&token_file).unwrap(),
        format!("{OLD_TOKEN}\n")
    );
}

#[test]
fn a_piped_install_stops_on_an_unreadable_token_file_before_it_writes_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(&dir);
    fs::create_dir(&paths.token_file).unwrap();
    let piped = fs::File::open(token_file_holding(&tempfile::tempdir().unwrap(), NEW_TOKEN));
    let Err(error) = install_start(&paths, "http://127.0.0.1:9", Some(piped.unwrap())) else {
        panic!("an unreadable token file was taken");
    };
    let error = error.to_string();
    assert!(
        error.contains(&format!("Could not read {}", paths.token_file.display())),
        "{error}"
    );
    assert!(
        !paths.checkin_dir.exists(),
        "the install wrote its key first"
    );
}

#[test]
fn a_token_read_from_its_file_stays_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = token_file_holding(&dir, OLD_TOKEN);
    fs::set_permissions(&token_file, fs::Permissions::from_mode(0o644)).unwrap();
    let client = sending(OLD_TOKEN);
    let after = std::time::Duration::from_secs(60);
    for next in [
        Next::Wait { after },
        Next::Retry {
            after,
            why: "Could not reach Ployz Cloud: refused.".to_owned(),
        },
    ] {
        keep_answered_token(next, &client, &token_file, FROM_FILE).unwrap();
        assert_eq!(
            fs::read_to_string(&token_file).unwrap(),
            format!("{OLD_TOKEN}\n")
        );
        let mode = fs::metadata(&token_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn a_token_on_stdin_must_be_piped_not_typed() {
    let terminal = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/ptmx")
        .unwrap();
    let Err(typed) = stdin_token(terminal) else {
        panic!("a terminal on stdin was read");
    };
    let typed = typed.to_string();
    assert!(
        typed.contains("--token-stdin reads a piped Fleet Token"),
        "{typed}"
    );
    let piped = fs::File::open(token_file_holding(&tempfile::tempdir().unwrap(), NEW_TOKEN));
    assert!(stdin_token(piped.unwrap()).is_ok());
}

#[test]
fn a_token_on_stdin_belongs_to_an_install() {
    let parsed = command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(["--token-stdin"]);
    assert!(parsed.is_err());
    let parsed = command(Command::new("checkin"))
        .no_binary_name(true)
        .try_get_matches_from(["--install", "--token-stdin"])
        .unwrap();
    assert!(parsed.get_flag("token-stdin"));
}
