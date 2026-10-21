use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use chrono::Utc;
use iroh::SecretKey;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::super::{FleetToken, Headline, pass, run};
use super::super::{Probe, wire::EnrollPolicy};
use super::*;

const TOKEN: &str = "pmet_never_written_down";
const FLEET: &str = "pflt_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const PROBE: &str = r#"{
    "facts": {
        "os": {"id": "debian", "version_id": "12"},
        "kernel": "6.1.0",
        "arch": "x86_64",
        "virt": "BareMetal",
        "cgroup": "V2",
        "cpu_count": 4,
        "memory_total_bytes": 8589934592,
        "disks": [],
        "docker": "Absent"
    },
    "assessment": {"blockers": [], "warnings": [], "zfs": {"Ineligible": {"reason": {"code": "zfs_off", "message": "Off."}}}}
}"#;
const WAIT: &str = r#"{"kind":"wait","retryAfter":30}"#;

/// A pause a paced script or a slow Cloud takes. Only tests on paused time build one.
type Pause = Arc<dyn Fn() -> futures_util::future::BoxFuture<'static, ()> + Send + Sync>;

fn pause(length: Duration) -> Pause {
    Arc::new(move || Box::pin(tokio::time::sleep(length)))
}

struct Cloud {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    seen: tokio::sync::watch::Receiver<usize>,
    arrived: tokio::sync::watch::Receiver<usize>,
    release: Arc<tokio::sync::Notify>,
}

impl Cloud {
    async fn answering(answers: Vec<String>) -> Self {
        Self::serving(answers, None, None).await
    }

    async fn delaying(answers: Vec<String>, delay: Duration) -> Self {
        Self::serving(answers, Some(pause(delay)), None).await
    }

    /// Holds the answer to the `held`th request, counting from zero, until `release`.
    async fn holding(answers: Vec<String>, held: usize) -> Self {
        Self::serving(answers, None, Some(held)).await
    }

    async fn serving(answers: Vec<String>, delay: Option<Pause>, held: Option<usize>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let (count, seen) = tokio::sync::watch::channel(0);
        let kept = Arc::clone(&bodies);
        let (arrival, arrived) = tokio::sync::watch::channel(0);
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::clone(&release);
        tokio::spawn(async move {
            let mut answers = answers.into_iter();
            for request in 0.. {
                let (mut stream, _) = listener.accept().await.unwrap();
                let body = read_body(&mut stream).await;
                arrival.send_replace(request + 1);
                if held == Some(request) {
                    gate.notified().await;
                }
                if let Some(delay) = &delay {
                    delay().await;
                }
                let answer = answers.next().unwrap_or_else(|| WAIT.to_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}",
                    answer.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
                let total = {
                    let mut bodies = kept.lock().unwrap();
                    bodies.push(serde_json::from_slice(&body).unwrap());
                    bodies.len()
                };
                count.send_replace(total);
            }
        });
        Self {
            url,
            bodies,
            seen,
            arrived,
            release,
        }
    }

    async fn arrival(&self, count: usize) {
        self.arrived
            .clone()
            .wait_for(|arrived| *arrived >= count)
            .await
            .unwrap();
    }

    fn attempts(&self) -> Vec<Value> {
        self.bodies
            .lock()
            .unwrap()
            .iter()
            .filter_map(|body| body.get("attempt").cloned())
            .collect()
    }

    fn stages(&self) -> Vec<String> {
        let mut stages: Vec<String> = Vec::new();
        for attempt in self.attempts() {
            let stage = attempt
                .get("stage")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_owned();
            if stages.last() != Some(&stage) {
                stages.push(stage);
            }
        }
        stages
    }

    async fn told(&self, stage: EnrollStage) {
        let mut seen = self.seen.clone();
        let stage: &str = stage.into();
        seen.wait_for(|_| {
            self.attempts()
                .iter()
                .any(|attempt| attempt["stage"] == stage)
        })
        .await
        .unwrap();
    }
}

async fn read_body(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut read = Vec::new();
    let mut buf = [0; 4096];
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        read.extend_from_slice(buf.get(..n).unwrap());
        let text = String::from_utf8_lossy(&read);
        if let Some(end) = text.find("\r\n\r\n") {
            let length: usize = text
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|length| length.trim().parse().unwrap())
                })
                .unwrap_or(0);
            if read.len() >= end + 4 + length {
                return read.split_off(end + 4);
            }
        }
    }
}

struct Scripted {
    stages: Vec<EnrollStage>,
    result: Result<(), String>,
    discard: Result<(), String>,
    cloud: Arc<Cloud>,
    events: Mutex<Vec<&'static str>>,
    disables: AtomicU32,
    participating: bool,
    follow_up: Option<String>,
    pace: Option<Pause>,
    mid_post: bool,
    first: EnrollStage,
    installer: Option<std::path::PathBuf>,
}

impl Scripted {
    fn new(cloud: &Arc<Cloud>, stages: Vec<EnrollStage>, result: Result<(), String>) -> Self {
        Self {
            stages,
            result,
            discard: Ok(()),
            cloud: Arc::clone(cloud),
            events: Mutex::new(Vec::new()),
            disables: AtomicU32::new(0),
            participating: false,
            follow_up: None,
            pace: None,
            mid_post: false,
            first: EnrollStage::Installing,
            installer: None,
        }
    }

    fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }
}

impl Host for Scripted {
    async fn probe(&self) -> Result<Probe, String> {
        Ok(serde_json::from_str(PROBE).unwrap())
    }

    fn hostname(&self) -> String {
        "waiting-1".to_owned()
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        Ok(false)
    }

    async fn start_prepare(&self) -> Result<(), String> {
        Ok(())
    }

    async fn disable_loop(&self) -> Result<(), String> {
        self.disables.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stale_ployzd(&self) -> Option<String> {
        None
    }

    async fn enroll(
        &self,
        instruction: &EnrollInstruction,
        progress: &dyn Fn(EnrollStage),
    ) -> Result<crate::checkin::Committed, String> {
        assert_eq!(instruction.token.as_str(), TOKEN);
        self.events.lock().unwrap().push("enroll");
        progress(self.first);
        if let Some(installer) = &self.installer {
            assert!(
                tokio::process::Command::new(installer)
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
            self.events.lock().unwrap().push("installed");
        }
        if self.mid_post {
            self.cloud.arrival(2).await;
            for stage in &self.stages {
                progress(*stage);
            }
            self.cloud.release.notify_one();
            self.cloud.told(*self.stages.last().unwrap()).await;
        } else {
            if self.pace.is_none() {
                self.cloud.told(self.first).await;
            }
            for stage in &self.stages {
                if let Some(pace) = &self.pace {
                    pace().await;
                }
                progress(*stage);
                if self.pace.is_none() && *stage >= self.first {
                    self.cloud.told(*stage).await;
                }
            }
            if let Some(pace) = &self.pace {
                pace().await;
                if let Some(last) = self.stages.last() {
                    tokio::time::timeout(HEARTBEAT / 2, self.cloud.told(*last))
                        .await
                        .unwrap();
                }
            }
        }
        self.events.lock().unwrap().push("finished");
        Committed::from_setup(
            self.result
                .clone()
                .map_err(crate::handlers::Error::usage)
                .map(|()| {
                    self.follow_up.clone().map_or(Ok(()), |message| {
                        Err(crate::handlers::Error::usage(message))
                    })
                }),
        )
    }

    async fn participating(&self) -> Option<String> {
        self.participating.then(|| "web-1".to_owned())
    }

    async fn discard_zfs_build(&self) -> Result<(), String> {
        self.events.lock().unwrap().push("discard");
        self.discard.clone()
    }
}

fn instruction(storage: &str) -> String {
    format!(
        r#"{{"kind":"enroll","attemptId":"att_1","token":"{TOKEN}","role":"join","name":"web-1","storage":{{"kind":"{storage}"}},"policy":{{"labels":{{"tier":"edge"}},"acceptsBuilds":false,"acceptsServices":true,"acceptsIngress":true}}}}"#
    )
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: Paths,
    key: CheckinKey,
}

impl Fixture {
    fn new(zfs: &str) -> Self {
        Self::with_zfs(&format!(r#"{{"state":"{zfs}"}}"#))
    }

    fn with_zfs(zfs: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            checkin_dir: dir.path().join("checkin"),
            prepare_state: dir.path().join("prepare/state.json"),
            token_file: dir.path().join("fleet-token"),
        };
        fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
        fs::write(
            &paths.prepare_state,
            format!(r#"{{"docker":{{"state":"ready"}},"zfs":{zfs}}}"#),
        )
        .unwrap();
        fs::write(&paths.token_file, FLEET).unwrap();
        Self {
            _dir: dir,
            paths,
            key: CheckinKey::new(SecretKey::from_bytes(&[7; 32])),
        }
    }

    fn client(&self, cloud: &Cloud) -> CheckinClient {
        CheckinClient::new(&cloud.url, FleetToken::literal(FLEET)).unwrap()
    }

    fn status(&self) -> LocalCheckin {
        LocalCheckin::read(&self.paths.status()).unwrap().unwrap()
    }
}

const EVERY_STAGE: [EnrollStage; 5] = [
    EnrollStage::Enrolling,
    EnrollStage::Queued,
    EnrollStage::Storage,
    EnrollStage::Joining,
    EnrollStage::Publishing,
];

#[tokio::test]
async fn cloud_hears_every_stage_in_order_and_the_loop_ends_enrolled() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![instruction("zfs")]).await);
    let host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));

    let end = run(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
    )
    .await;

    assert!(matches!(end, CheckinEnd::Enrolled));
    assert_eq!(
        cloud.stages(),
        [
            "installing",
            "enrolling",
            "queued",
            "storage",
            "joining",
            "publishing"
        ]
    );
    assert!(
        cloud
            .attempts()
            .iter()
            .all(|attempt| attempt["attemptId"] == "att_1" && attempt.get("failure").is_none())
    );
    assert_eq!(fixture.status().answer, Seen::Joined);
    assert_eq!(
        fixture.status().headline(),
        Headline {
            title: "Joined",
            ended: false
        }
    );
    assert!(!fixture.paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_enroll_token_is_never_written_to_the_status_or_sent_back() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![instruction("zfs")]).await);
    let host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));

    pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;

    assert!(
        !fs::read_to_string(fixture.paths.status())
            .unwrap()
            .contains(TOKEN)
    );
    for body in cloud.bodies.lock().unwrap().iter() {
        assert!(!body.to_string().contains(TOKEN));
    }
}

#[tokio::test]
async fn a_failed_enroll_goes_back_to_waiting_and_tells_cloud_where_it_failed() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![instruction("zfs")]).await);
    let host = Scripted::new(
        &cloud,
        vec![EnrollStage::Enrolling],
        Err("Docker is not running".to_owned()),
    );
    let mut client = fixture.client(&cloud);

    let next = pass(&host, &mut client, &fixture.key, &fixture.paths, Utc::now()).await;

    assert_eq!(
        next,
        Next::Wait {
            after: Duration::ZERO
        }
    );
    let status = fixture.status();
    assert_eq!(status.answer, Seen::Wait);
    assert_eq!(
        status.headline(),
        Headline {
            title: "Enroll failed",
            ended: false
        }
    );
    assert!(status.says_why());
    assert_eq!(
        status.message.as_deref(),
        Some("Enroll failed at enrolling: Docker is not running")
    );

    let next = pass(&host, &mut client, &fixture.key, &fixture.paths, Utc::now()).await;

    assert_eq!(
        next,
        Next::Wait {
            after: Duration::from_secs(30)
        }
    );
    let last = cloud.bodies.lock().unwrap().last().cloned().unwrap();
    assert_eq!(
        last.get("attempt").unwrap(),
        &serde_json::json!({
            "attemptId": "att_1",
            "stage": "enrolling",
            "failure": {"stage": "enrolling", "message": "Docker is not running"}
        })
    );
    assert_eq!(fixture.status().headline().title, "Enroll failed");
    assert_eq!(host.events(), ["enroll", "finished"]);
}

#[tokio::test]
async fn an_attempt_cut_short_is_sent_again_so_cloud_serves_it_to_resume() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(Vec::new()).await);
    let host = Scripted::new(&cloud, Vec::new(), Ok(()));
    record(
        &LocalCheckin {
            answer: Seen::Enroll,
            token_fingerprint: Some(
                CheckinClient::new("https://cloud.example", FleetToken::literal(FLEET))
                    .unwrap()
                    .fingerprint(),
            ),
            reason: None,
            message: None,
            at: Utc::now(),
            prep: None,
            blockers: Vec::new(),
            prepare_retries: 0,
            prepare_started: None,
            refused: None,
            attempt: Some(Attempt {
                attempt_id: "att_1".to_owned(),
                stage: EnrollStage::Joining,
                failure: None,
            }),
            no_cluster: false,
        },
        &fixture.paths,
    );

    pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;

    assert_eq!(
        cloud.attempts(),
        [serde_json::json!({"attemptId": "att_1", "stage": "joining"})]
    );
}

#[tokio::test]
async fn a_server_enrolled_without_storage_discards_the_zfs_build_prep_began_first() {
    for (storage, zfs, events) in [
        ("none", r#"{"state":"ready"}"#, vec!["discard", "enroll"]),
        (
            "none",
            r#"{"state":"installing"}"#,
            vec!["discard", "enroll"],
        ),
        (
            "none",
            r#"{"state":"failed","message":"dkms","finished_at":"2026-10-16T00:00:00Z"}"#,
            vec!["discard", "enroll"],
        ),
        ("none", r#"{"state":"skipped"}"#, vec!["enroll"]),
        ("zfs", r#"{"state":"ready"}"#, vec!["enroll"]),
        ("none", r#"{"state":"off"}"#, vec!["enroll"]),
    ] {
        let fixture = Fixture::with_zfs(zfs);
        let cloud = Arc::new(Cloud::answering(vec![instruction(storage)]).await);
        let host = Scripted::new(&cloud, Vec::new(), Ok(()));

        pass(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now(),
        )
        .await;

        assert_eq!(
            host.events()
                .into_iter()
                .filter(|event| *event != "finished")
                .collect::<Vec<_>>(),
            events,
            "storage {storage}, zfs {zfs}"
        );
    }
}

#[tokio::test]
async fn a_zfs_build_that_cannot_be_discarded_fails_the_attempt_before_enrolling() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![instruction("none")]).await);
    let mut host = Scripted::new(&cloud, Vec::new(), Ok(()));
    host.discard = Err("dkms remove exited 3".to_owned());

    pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;

    assert_eq!(host.events(), ["discard"]);
    assert_eq!(
        fixture.status().message.as_deref(),
        Some("Enroll failed at installing: Could not discard the ZFS build: dkms remove exited 3")
    );
}

#[test]
fn the_add_arguments_carry_the_whole_instruction_and_a_root_config_without_home() {
    let instruction = EnrollInstruction {
        attempt_id: "att_1".to_owned(),
        token: ployz_core::CloudEnrollToken::parse(TOKEN).unwrap(),
        name: "web-1".to_owned(),
        storage: EnrollStorage::Zfs { device: None },
        policy: EnrollPolicy {
            labels: BTreeMap::from([("tier".to_owned(), "edge".to_owned())]),
            accepts_builds: false,
            accepts_services: true,
            accepts_ingress: true,
        },
    };

    let arguments = add_arguments(&instruction, "https://cloud.example", false);

    assert_eq!(
        arguments,
        [
            "ployz",
            "server",
            "add",
            "--yes",
            "--connect",
            "unix:///run/ployz/ployz.sock",
            "--name",
            "web-1",
            "--storage",
            "zfs",
            "--cloud-url",
            "https://cloud.example",
            "--accepts-builds=false",
            "--accepts-services=true",
            "--accepts-ingress=true",
            "--label-add",
            "tier=edge",
            "--ployz-config",
            "/root/.config/ployz/config.yaml",
        ]
    );
    assert!(!arguments.iter().any(|argument| argument.contains(TOKEN)));
    assert!(
        crate::cli::command()
            .try_get_matches_from(&arguments)
            .is_ok()
    );
    assert!(
        !add_arguments(&instruction, "https://cloud.example", true)
            .contains(&"--ployz-config".to_owned())
    );
}

#[test]
fn a_failure_sent_to_cloud_keeps_the_cause_under_its_sentence() {
    let error =
        crate::handlers::Error::coded(ployz_core::RpcErrorCode::Unavailable, "Store not ready")
            .context("the first Server did not become ready");

    assert_eq!(
        failure_message(&error),
        "the first Server did not become ready; cause: Store not ready"
    );
}

fn previous(fixture: &Fixture, answer: Seen, stage: EnrollStage) {
    record(
        &LocalCheckin {
            answer,
            token_fingerprint: Some(
                CheckinClient::new("https://cloud.example", FleetToken::literal(FLEET))
                    .unwrap()
                    .fingerprint(),
            ),
            reason: None,
            message: None,
            at: Utc::now(),
            prep: None,
            blockers: Vec::new(),
            prepare_retries: 0,
            prepare_started: None,
            refused: None,
            attempt: Some(Attempt {
                attempt_id: "att_1".to_owned(),
                stage,
                failure: None,
            }),
            no_cluster: false,
        },
        &fixture.paths,
    );
}

#[tokio::test]
async fn a_committed_enroll_with_failed_follow_up_cleans_up_as_joined() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![instruction("zfs")]).await);
    let mut host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));
    host.follow_up = Some("Global catch-up failed".to_owned());
    let next = pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;
    assert_eq!(next, Next::Enrolled);
    assert_eq!(finish(&host, &fixture.paths).await, CheckinEnd::Enrolled);
    assert_eq!(fixture.status().answer, Seen::Joined);
    assert_eq!(
        fixture.status().message.as_deref(),
        Some("Global catch-up failed")
    );
    assert!(fixture.status().attempt.unwrap().failure.is_none());
    assert!(!fixture.paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn rejection_during_installing_or_slot_wait_stops_before_storage() {
    let _ticker = crate::checkin::tests::tick_through_io();
    for stage in [
        EnrollStage::Installing,
        EnrollStage::Enrolling,
        EnrollStage::Queued,
    ] {
        let fixture = Fixture::new("ready");
        let cloud = Arc::new(
            Cloud::answering(vec![
                instruction("zfs"),
                r#"{"kind":"stop","reason":"rejected","message":"Rejected by owner"}"#.to_owned(),
            ])
            .await,
        );
        if stage != EnrollStage::Installing {
            previous(&fixture, Seen::Enroll, stage);
        }
        let mut host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));
        host.pace = Some(pause(Duration::from_millis(250)));
        let next = pass(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now(),
        )
        .await;
        assert_eq!(
            next,
            Next::Stop(StopReason::Rejected, "Rejected by owner".to_owned()),
            "{stage:?}"
        );
        assert_eq!(fixture.status().answer, Seen::Stop);
        assert_eq!(fixture.status().reason, Some(StopReason::Rejected));
        assert!(!host.events().contains(&"finished"));
        assert!(fixture.paths.token_file.exists());
    }
}

#[tokio::test(start_paused = true)]
async fn rejection_after_storage_allows_committed_work_to_finish() {
    let _ticker = crate::checkin::tests::tick_through_io();
    for stage in [EnrollStage::Joining, EnrollStage::Publishing] {
        let fixture = Fixture::new("ready");
        let cloud = Arc::new(
            Cloud::delaying(
                vec![
                    instruction("zfs"),
                    r#"{"kind":"stop","reason":"rejected","message":"Rejected by owner"}"#
                        .to_owned(),
                ],
                Duration::from_millis(350),
            )
            .await,
        );
        previous(&fixture, Seen::Enroll, stage);
        let mut host = Scripted::new(
            &cloud,
            vec![
                EnrollStage::Enrolling,
                EnrollStage::Joining,
                EnrollStage::Publishing,
            ],
            Ok(()),
        );
        host.pace = Some(pause(Duration::from_millis(250)));
        let next = pass(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now(),
        )
        .await;
        assert_eq!(next, Next::Enrolled, "saved {stage:?}");
        assert!(host.events().contains(&"finished"));
        assert_eq!(fixture.status().answer, Seen::Joined);
        assert_eq!(
            fixture.status().attempt.unwrap().stage,
            EnrollStage::Publishing
        );
        assert!(fixture.paths.token_file.exists());
        assert_eq!(host.disables.load(Ordering::SeqCst), 0);

        assert_eq!(finish(&host, &fixture.paths).await, CheckinEnd::Enrolled);
        assert!(!fixture.paths.token_file.exists());
        assert_eq!(host.disables.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn committed_resume_keeps_heartbeat_pacing_when_cloud_keeps_rejecting() {
    let _ticker = crate::checkin::tests::tick_through_io();
    let fixture = Fixture::new("ready");
    previous(&fixture, Seen::Enroll, EnrollStage::Joining);
    let mut answers = vec![instruction("zfs")];
    answers.extend(std::iter::repeat_n(
        r#"{"kind":"stop","reason":"rejected","message":"Rejected by owner"}"#.to_owned(),
        100,
    ));
    let cloud = Arc::new(Cloud::answering(answers).await);
    let mut host = Scripted::new(
        &cloud,
        vec![
            EnrollStage::Enrolling,
            EnrollStage::Joining,
            EnrollStage::Publishing,
        ],
        Ok(()),
    );
    host.pace = Some(pause(Duration::from_millis(250)));

    assert_eq!(
        pass(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now(),
        )
        .await,
        Next::Enrolled
    );
    assert!(host.events().contains(&"finished"));
    assert!(
        cloud.attempts().len() <= 5,
        "Repeated rejection bypassed stage/heartbeat pacing: {} posts",
        cloud.attempts().len()
    );
    assert_eq!(cloud.stages(), ["joining", "publishing"]);
}

#[tokio::test(start_paused = true)]
async fn rejected_committed_resume_reports_its_actual_failure() {
    let _ticker = crate::checkin::tests::tick_through_io();
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(
        Cloud::answering(vec![
            instruction("zfs"),
            r#"{"kind":"stop","reason":"rejected","message":"Rejected by owner"}"#.to_owned(),
        ])
        .await,
    );
    previous(&fixture, Seen::Enroll, EnrollStage::Joining);
    let mut host = Scripted::new(&cloud, vec![], Err("Publish failed".to_owned()));
    host.first = EnrollStage::Joining;
    host.pace = Some(pause(Duration::from_millis(250)));
    let next = pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;
    assert_eq!(
        next,
        Next::Wait {
            after: Duration::ZERO
        }
    );
    assert!(host.events().contains(&"finished"));
    assert_eq!(fixture.status().answer, Seen::Wait);
    let failure = fixture.status().attempt.unwrap().failure.unwrap();
    assert_eq!(failure.stage, EnrollStage::Joining);
    assert_eq!(failure.message, "Publish failed");
    assert!(fixture.paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn token_expiration_or_revocation_allows_an_open_attempt_to_finish() {
    for reason in ["token_expired", "token_revoked"] {
        let fixture = Fixture::new("ready");
        let cloud = Arc::new(
            Cloud::answering(vec![
                instruction("zfs"),
                format!(r#"{{"kind":"stop","reason":"{reason}"}}"#),
            ])
            .await,
        );
        let host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));
        assert_eq!(
            run(
                &host,
                &mut fixture.client(&cloud),
                &fixture.key,
                &fixture.paths
            )
            .await,
            CheckinEnd::Enrolled
        );
    }
}

#[tokio::test]
async fn persisted_joined_resumes_cleanup_before_another_checkin() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![]).await);
    previous(&fixture, Seen::Joined, EnrollStage::Publishing);
    let host = Scripted::new(&cloud, vec![], Ok(()));
    let next = pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;
    assert_eq!(next, Next::Enrolled);
    assert_eq!(finish(&host, &fixture.paths).await, CheckinEnd::Enrolled);
    assert!(cloud.bodies.lock().unwrap().is_empty());
    assert!(!fixture.paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn participating_after_callback_resumes_cleanup_when_cloud_answers_wait() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![]).await);
    previous(&fixture, Seen::Enroll, EnrollStage::Publishing);
    let mut host = Scripted::new(&cloud, vec![], Ok(()));
    host.participating = true;
    let next = pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;
    assert_eq!(next, Next::Enrolled);
    assert_eq!(finish(&host, &fixture.paths).await, CheckinEnd::Enrolled);
    assert_eq!(fixture.status().answer, Seen::Joined);
    assert!(!fixture.paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn terminal_cleanup_is_harmless_twice() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![]).await);
    let host = Scripted::new(&cloud, vec![], Ok(()));
    assert_eq!(finish(&host, &fixture.paths).await, CheckinEnd::Enrolled);
    assert_eq!(finish(&host, &fixture.paths).await, CheckinEnd::Enrolled);
    assert!(!fixture.paths.token_file.exists());
    assert_eq!(host.disables.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn resumed_attempt_reports_only_forward_moves() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![instruction("zfs")]).await);
    previous(&fixture, Seen::Enroll, EnrollStage::Joining);
    let mut host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));
    host.first = EnrollStage::Joining;
    assert_eq!(
        pass(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now()
        )
        .await,
        Next::Enrolled
    );
    assert_eq!(cloud.stages(), ["joining", "publishing"]);
}

#[tokio::test]
async fn stage_changes_during_a_post_are_coalesced_and_sent_promptly() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::holding(vec![instruction("zfs")], 1).await);
    let mut host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));
    host.mid_post = true;
    let next = tokio::time::timeout(
        HEARTBEAT / 2,
        pass(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(next, Next::Enrolled);
    assert_eq!(cloud.stages(), ["installing", "publishing"]);
    assert_eq!(cloud.attempts().len(), 2);
}

#[test]
fn the_zfs_device_reaches_server_add() {
    let answer = wire::answer(
        instruction("zfs")
            .replace(
                r#""kind":"zfs""#,
                r#""kind":"zfs","device":"/dev/disk/by-id/spare""#,
            )
            .as_bytes(),
    );
    let Some(Answer::Enroll(instruction)) = answer else {
        panic!("enroll instruction");
    };
    assert_eq!(
        instruction.storage,
        EnrollStorage::Zfs {
            device: Some("/dev/disk/by-id/spare".to_owned())
        }
    );
    assert!(
        add_arguments(&instruction, "https://cloud.example", true)
            .windows(2)
            .any(|pair| pair == ["--storage", "zfs:/dev/disk/by-id/spare"])
    );
}

async fn reject_resumed_installer(saved: EnrollStage) {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("ready");
    previous(&fixture, Seen::Enroll, saved);
    let cloud = Arc::new(
        Cloud::delaying(
            vec![
                instruction("zfs"),
                r#"{"kind":"stop","reason":"rejected","message":"Rejected by owner"}"#.to_owned(),
            ],
            Duration::from_millis(50),
        )
        .await,
    );
    let installed = fixture._dir.path().join("installed");
    let script = fixture._dir.path().join("installer");
    fs::write(
        &script,
        format!("#!/bin/sh\nsleep 0.25\ntouch '{}'\n", installed.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let mut host = Scripted::new(&cloud, EVERY_STAGE.to_vec(), Ok(()));
    host.installer = Some(script);
    host.pace = Some(pause(Duration::from_millis(100)));
    let next = pass(
        &host,
        &mut fixture.client(&cloud),
        &fixture.key,
        &fixture.paths,
        Utc::now(),
    )
    .await;

    assert_eq!(
        next,
        Next::Stop(StopReason::Rejected, "Rejected by owner".to_owned()),
        "saved {saved:?}"
    );
    assert!(
        installed.exists(),
        "saved {saved:?} returned before the child finished"
    );
    assert_eq!(host.events(), ["enroll", "installed"]);
    assert!(fixture.paths.token_file.exists());
}

#[tokio::test(start_paused = true)]
async fn resumed_rejection_awaits_the_installer_and_stops_before_storage() {
    let _ticker = crate::checkin::tests::tick_through_io();
    for saved in [
        EnrollStage::Installing,
        EnrollStage::Enrolling,
        EnrollStage::Queued,
    ] {
        reject_resumed_installer(saved).await;
    }
}

#[tokio::test(start_paused = true)]
async fn resumed_rejection_from_saved_storage_stops_at_live_enrolling() {
    let _ticker = crate::checkin::tests::tick_through_io();
    reject_resumed_installer(EnrollStage::Storage).await;
}

async fn replace_token(installing: bool) {
    for answer in [Seen::Joined, Seen::Enroll] {
        let fixture = Fixture::new("ready");
        previous(&fixture, answer, EnrollStage::Publishing);
        let cloud = Arc::new(Cloud::answering(vec![]).await);
        let mut host = Scripted::new(&cloud, vec![], Ok(()));
        host.participating = true;
        let new_token = "pflt_BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB";
        fs::write(&fixture.paths.token_file, new_token).unwrap();
        let mut client = CheckinClient::new(&cloud.url, FleetToken::literal(new_token)).unwrap();
        let next = super::super::pass_from(
            &host,
            &mut client,
            &fixture.key,
            &fixture.paths,
            Utc::now(),
            installing,
            &mut super::super::Probes::load(fixture.paths.probe()),
        )
        .await;
        assert_eq!(
            next,
            Next::Wait {
                after: Duration::from_secs(30)
            },
            "{answer:?}, install {installing}"
        );
        assert!(cloud.attempts().is_empty());
        assert!(fixture.status().attempt.is_none());
        assert_eq!(fixture.status().answer, Seen::Wait);
        assert_eq!(
            fixture.status().token_fingerprint,
            Some(client.fingerprint())
        );
        assert_eq!(
            fs::read_to_string(&fixture.paths.token_file).unwrap(),
            new_token
        );
        assert_eq!(host.disables.load(Ordering::SeqCst), 0);
        assert_eq!(
            pass(&host, &mut client, &fixture.key, &fixture.paths, Utc::now()).await,
            Next::Wait {
                after: Duration::from_secs(30)
            }
        );
    }
}

#[tokio::test]
async fn a_new_fleet_token_does_not_reuse_joined_or_an_old_attempt_without_install() {
    replace_token(false).await;
}

#[tokio::test]
async fn a_new_fleet_token_does_not_reuse_joined_or_an_old_attempt_with_install() {
    replace_token(true).await;
}

#[test]
fn the_host_adapter_overrides_remote_context_and_connection_environment() {
    const CHILD: &str = "PLOYZ_TEST_HOST_ADAPTER";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("remote.yaml");
        fs::write(&config, "current_context: remote\ncontexts:\n  remote:\n    connections:\n      - tcp: 127.0.0.1:9\n").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "checkin::enroll::tests::the_host_adapter_overrides_remote_context_and_connection_environment", "--nocapture"])
            .env(CHILD, "1")
            .env("PLOYZ_CONFIG", &config)
            .env("PLOYZ_CONTEXT", "remote")
            .env("PLOYZ_CONNECT", "tcp://127.0.0.1:9")
            .output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        assert!(
            stdout.contains("running 1 test\n"),
            "the child did not run exactly one test: {stdout}"
        );
        assert!(
            stdout.contains("1 passed;"),
            "the child did not pass its test: {stdout}"
        );
        return;
    }
    let Some(Answer::Enroll(instruction)) = wire::answer(instruction("none").as_bytes()) else {
        panic!("enroll");
    };
    let root = crate::cli::command()
        .try_get_matches_from(add_arguments(&instruction, "https://cloud.example", true))
        .unwrap();
    let matches = root
        .subcommand_matches("server")
        .unwrap()
        .subcommand_matches("add")
        .unwrap();
    let selected = crate::connect::resolve_connections(
        Path::new(matches.get_one::<String>("ployz-config").unwrap()),
        matches.get_one::<String>("connect").map(String::as_str),
        None,
        Path::new(crate::connect::DEFAULT_LOCAL_SOCKET),
    )
    .unwrap();
    assert_eq!(selected.connections.len(), 1);
    assert_eq!(
        selected.connections.first().unwrap().to_string(),
        "unix:///run/ployz/ployz.sock"
    );
}

#[tokio::test]
async fn a_check_in_reports_the_saved_probe_and_token_bound_attempt_together() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(vec![WAIT.to_owned()]).await);
    let host = Scripted::new(&cloud, Vec::new(), Ok(()));
    let mut client = fixture.client(&cloud);
    let attempt = serde_json::json!({
        "attemptId": "att_1",
        "stage": "enrolling",
        "failure": {"stage": "enrolling", "message": "Docker is not running"}
    });
    let status = serde_json::from_value(serde_json::json!({
        "answer": "wait",
        "token_fingerprint": client.fingerprint(),
        "at": Utc::now(),
        "attempt": attempt
    }))
    .unwrap();
    record(&status, &fixture.paths);
    let probe = serde_json::json!({
        "handout": "completed-window",
        "heardFrom": ["192.0.2.1"],
        "bindFailed": ["tcp:443"]
    });
    fs::write(fixture.paths.probe(), serde_json::to_vec(&probe).unwrap()).unwrap();

    let next = pass(&host, &mut client, &fixture.key, &fixture.paths, Utc::now()).await;

    assert!(matches!(next, Next::Wait { .. }));
    let bodies = cloud.bodies.lock().unwrap();
    let body = bodies.first().unwrap();
    assert_eq!(body["attempt"], attempt);
    assert_eq!(body["probe"], probe);
    assert_eq!(fixture.status().headline(), Headline::open("Enroll failed"));
}

#[tokio::test]
async fn an_enroll_instruction_closes_the_probe_ports_on_install_and_loop_passes() {
    use super::super::{Probes, pass_from};

    for installing in [true, false] {
        let fixture = Fixture::new("ready");
        let cloud = Arc::new(Cloud::answering(vec![instruction("zfs")]).await);
        let host = Scripted::new(&cloud, Vec::new(), Ok(()));
        let port = std::net::UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let handout = serde_json::from_value(serde_json::json!({
            "nonce": "nonce",
            "handout": "open-window",
            "windowMs": 10000,
            "listen": [format!("udp:{port}")],
            "send": []
        }))
        .unwrap();
        let mut probes = Probes::load(fixture.paths.probe());
        probes
            .open(handout, &fixture.key, tokio::time::Instant::now())
            .await;
        assert!(std::net::UdpSocket::bind(("0.0.0.0", port)).is_err());

        let next = pass_from(
            &host,
            &mut fixture.client(&cloud),
            &fixture.key,
            &fixture.paths,
            Utc::now(),
            installing,
            &mut probes,
        )
        .await;

        assert!(std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok());
        assert!(probes.report().await.is_none());
        if installing {
            assert_eq!(next, Next::Enrolling);
            assert!(host.events().is_empty());
        } else {
            assert_eq!(next, Next::Enrolled);
            assert_eq!(fixture.status().answer, Seen::Joined);
        }
    }
}
