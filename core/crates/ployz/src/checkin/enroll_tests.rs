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

/// Answers each check-in in turn, then WAIT, and keeps every body it was sent.
struct Cloud {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    seen: tokio::sync::watch::Receiver<usize>,
}

impl Cloud {
    async fn answering(answers: Vec<String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let (count, seen) = tokio::sync::watch::channel(0);
        let kept = Arc::clone(&bodies);
        tokio::spawn(async move {
            let mut answers = answers.into_iter();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let body = read_body(&mut stream).await;
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
        Self { url, bodies, seen }
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

    /// Waits until Cloud has been told the attempt reached `stage`.
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

/// An enrollment that moves through `stages`, each once Cloud has heard the last, then ends
/// with `result`.
struct Scripted {
    stages: Vec<EnrollStage>,
    result: Result<(), String>,
    cloud: Arc<Cloud>,
    events: Mutex<Vec<&'static str>>,
    disables: AtomicU32,
}

impl Scripted {
    fn new(cloud: &Arc<Cloud>, stages: Vec<EnrollStage>, result: Result<(), String>) -> Self {
        Self {
            stages,
            result,
            cloud: Arc::clone(cloud),
            events: Mutex::new(Vec::new()),
            disables: AtomicU32::new(0),
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
    ) -> Result<(), String> {
        assert_eq!(instruction.token.as_str(), TOKEN);
        self.events.lock().unwrap().push("enroll");
        self.cloud.told(EnrollStage::Installing).await;
        for stage in &self.stages {
            progress(*stage);
            self.cloud.told(*stage).await;
        }
        self.result.clone()
    }

    async fn discard_zfs_build(&self) -> Result<(), String> {
        self.events.lock().unwrap().push("discard");
        Ok(())
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
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            checkin_dir: dir.path().join("checkin"),
            prepare_state: dir.path().join("prepare/state.json"),
            token_file: dir.path().join("fleet-token"),
        };
        fs::create_dir_all(paths.prepare_state.parent().unwrap()).unwrap();
        fs::write(
            &paths.prepare_state,
            format!(r#"{{"docker":{{"state":"ready"}},"zfs":{{"state":"{zfs}"}}}}"#),
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
    EnrollStage::Storage,
    EnrollStage::Enrolling,
    EnrollStage::Queued { ahead: 2 },
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
            "storage",
            "enrolling",
            "queued",
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
        vec![EnrollStage::Storage, EnrollStage::Enrolling],
        Err("Docker is not running".to_owned()),
    );
    let mut client = fixture.client(&cloud);

    let next = pass(&host, &mut client, &fixture.key, &fixture.paths, Utc::now()).await;

    assert_eq!(
        next,
        Next::Wait {
            after: Duration::ZERO,
            log: None
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
            after: Duration::from_secs(30),
            log: None
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
    assert_eq!(host.events(), ["enroll"]);
}

#[tokio::test]
async fn an_attempt_cut_short_is_sent_again_so_cloud_serves_it_to_resume() {
    let fixture = Fixture::new("ready");
    let cloud = Arc::new(Cloud::answering(Vec::new()).await);
    let host = Scripted::new(&cloud, Vec::new(), Ok(()));
    record(
        &LocalCheckin {
            answer: Seen::Enroll,
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
async fn a_server_enrolled_without_storage_discards_the_ready_zfs_build_first() {
    for (storage, zfs, events) in [
        ("none", "ready", vec!["discard", "enroll"]),
        ("zfs", "ready", vec!["enroll"]),
        ("none", "off", vec!["enroll"]),
    ] {
        let fixture = Fixture::new(zfs);
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

        assert_eq!(host.events(), events, "storage {storage}, zfs {zfs}");
    }
}

#[test]
fn the_add_arguments_carry_the_whole_instruction_and_a_root_config_without_home() {
    let instruction = EnrollInstruction {
        attempt_id: "att_1".to_owned(),
        token: ployz_core::CloudEnrollToken::parse(TOKEN).unwrap(),
        name: "web-1".to_owned(),
        storage: EnrollStorage::Zfs,
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
