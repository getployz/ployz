//! Cloud-published commands against a fake Cloud: the first run fetches the manifest,
//! runs print what Cloud answers, and the cache follows what Cloud says.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::{Command, Output},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/published-manifest.json");

/// What the fake Cloud says, and what it saw.
#[derive(Default)]
struct Cloud {
    /// The revision every reply names.
    revision: String,
    /// The reply to the next run, as `(status, body)`.
    run: Option<(u16, Value)>,
    routes: Vec<String>,
    /// Request keys seen on run routes.
    keys: Vec<String>,
}

fn fake_cloud(cloud: Arc<Mutex<Cloud>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut length = 0;
            let mut key = None;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                let lower = header.to_ascii_lowercase();
                if let Some(value) = lower.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if let Some(value) = lower.strip_prefix("x-ployz-request-key:") {
                    key = Some(value.trim().to_owned());
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let route = request_line.rsplit_once(' ').unwrap().0.to_owned();
            let mut cloud = cloud.lock().unwrap();
            cloud.routes.push(route.clone());
            let (status, body) = match route.as_str() {
                "GET /api/cli/manifest" => {
                    let mut manifest: Value = serde_json::from_str(FIXTURE).unwrap();
                    manifest["revision"] = Value::from(cloud.revision.as_str());
                    (200, manifest)
                }
                run if run.starts_with("POST /api/cli/run/") => {
                    cloud.keys.extend(key);
                    cloud.run.clone().unwrap_or((200, json!({ "output": {} })))
                }
                _ => (404, json!({})),
            };
            let revision = cloud.revision.clone();
            drop(cloud);
            let body = body.to_string();
            write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\nx-ployz-manifest: {revision}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    url
}

struct Fixture {
    cloud: Arc<Mutex<Cloud>>,
    url: String,
    home: tempfile::TempDir,
}

impl Fixture {
    fn new(revision: &str) -> Self {
        let cloud = Arc::new(Mutex::new(Cloud {
            revision: revision.to_owned(),
            ..Cloud::default()
        }));
        let url = fake_cloud(Arc::clone(&cloud));
        Self {
            cloud,
            url,
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn config(&self) -> PathBuf {
        self.home.path().join("config.yaml")
    }

    fn ployz(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_ployz"))
            .args(args)
            .env("HOME", self.home.path())
            .env("PLOYZ_CONFIG", self.config())
            .env("PLOYZ_TOKEN", "ployz_test_token")
            .env("PLOYZ_CLOUD_URL", &self.url)
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    }

    fn routes(&self) -> Vec<String> {
        self.cloud.lock().unwrap().routes.clone()
    }

    fn answer_runs_with(&self, status: u16, body: Value) {
        self.cloud.lock().unwrap().run = Some((status, body));
    }

    fn cache(&self) -> Option<Value> {
        let dir = self.home.path().join("published");
        let file = std::fs::read_dir(dir).ok()?.next()?.ok()?.path();
        serde_json::from_slice(&std::fs::read(file).ok()?).ok()
    }

    fn cached_names(&self) -> Vec<String> {
        self.cache()
            .and_then(|cache| {
                cache["manifest"]["commands"].as_array().map(|commands| {
                    commands
                        .iter()
                        .map(|entry| entry["name"].as_str().unwrap().to_owned())
                        .collect()
                })
            })
            .unwrap_or_default()
    }
}

fn fixture_entries() -> Vec<Value> {
    serde_json::from_str::<Value>(FIXTURE).unwrap()["commands"]
        .as_array()
        .cloned()
        .unwrap()
}

/// The fixture entry with `render`, so no test names a published command.
fn entry_rendering(render: &str) -> Value {
    fixture_entries()
        .into_iter()
        .find(|entry| entry["render"] == render && entry["typed"].is_null())
        .unwrap_or_else(|| panic!("the fixture has an untyped {render} entry"))
}

fn argv(entry: &Value) -> Vec<String> {
    entry["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|part| part.as_str().unwrap().to_owned())
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}\nstdout: {}\nstderr: {}",
            text(&output.stdout),
            text(&output.stderr)
        )
    })
}

#[test]
fn the_first_run_with_an_empty_cache_fetches_the_manifest_and_runs() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("record");
    cloud.answer_runs_with(200, json!({ "output": { "channel": "beta" } }));
    let mut args = argv(&entry);
    args.push("--json".into());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = cloud.ployz(&args);

    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        text(&output.stdout),
        text(&output.stderr)
    );
    assert_eq!(stdout_json(&output), json!({ "channel": "beta" }));
    let name = entry["name"].as_str().unwrap();
    assert_eq!(
        cloud.routes(),
        vec![
            "GET /api/cli/manifest".to_owned(),
            format!("POST /api/cli/run/{name}")
        ],
        "the miss refreshes once, then runs"
    );
    assert!(
        cloud.cached_names().contains(&name.to_owned()),
        "the manifest is kept beside the config"
    );
    let keys = cloud.cloud.lock().unwrap().keys.clone();
    assert_eq!(keys.len(), 1, "every run carries a request key");
    assert!(!keys[0].is_empty());
}

#[test]
fn a_cached_manifest_answers_help_without_the_network() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("record");
    let args = argv(&entry);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cloud.ployz(&args);
    let before = cloud.routes().len();

    let path = &args[..args.len() - 1];
    let help = cloud.ployz(&[path, &["--help"]].concat());

    assert!(help.status.success(), "{}", text(&help.stderr));
    assert!(
        text(&help.stdout).contains(args[args.len() - 1]),
        "the published leaf shows under its group:\n{}",
        text(&help.stdout)
    );
    assert_eq!(cloud.routes().len(), before, "help reads the cache only");
}

#[test]
fn a_reply_naming_a_newer_revision_refreshes_after_the_command() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("record");
    let args = argv(&entry);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cloud.ployz(&args);
    assert_eq!(cloud.cache().unwrap()["manifest"]["revision"], "one");

    cloud.cloud.lock().unwrap().revision = "two".into();
    let output = cloud.ployz(&args);

    assert!(output.status.success(), "{}", text(&output.stderr));
    let name = entry["name"].as_str().unwrap();
    assert_eq!(
        cloud.routes()[2..],
        [
            format!("POST /api/cli/run/{name}"),
            "GET /api/cli/manifest".to_owned()
        ],
        "the run goes first, the refresh follows"
    );
    assert_eq!(cloud.cache().unwrap()["manifest"]["revision"], "two");
}

#[test]
fn a_reply_naming_the_cached_revision_costs_no_refresh() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("record");
    let args = argv(&entry);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cloud.ployz(&args);

    cloud.ployz(&args);

    let name = entry["name"].as_str().unwrap();
    assert_eq!(cloud.routes()[2..], [format!("POST /api/cli/run/{name}")]);
}

#[test]
fn unknown_command_evicts_the_entry_and_names_the_loss() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("record");
    let args = argv(&entry);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    cloud.ployz(&args);
    let name = entry["name"].as_str().unwrap();
    assert!(cloud.cached_names().contains(&name.to_owned()));

    cloud.answer_runs_with(
        404,
        json!({ "error": { "code": "unknown_command", "message": "gone" } }),
    );
    let output = cloud.ployz(&args);

    assert!(!output.status.success());
    assert!(
        text(&output.stderr).contains("no longer has the command"),
        "{}",
        text(&output.stderr)
    );
    assert!(
        !cloud.cached_names().contains(&name.to_owned()),
        "the entry leaves the cache"
    );
}

#[test]
fn a_typed_name_is_refused_without_a_terminal_before_cloud_is_asked() {
    let cloud = Fixture::new("one");
    let entry = fixture_entries()
        .into_iter()
        .find(|entry| entry["typed"] == "organization")
        .expect("the fixture has a typed entry");
    let mut args = argv(&entry);
    for positional in entry["positional"].as_array().unwrap() {
        args.push(format!("{}-value", positional.as_str().unwrap()));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = cloud.ployz(&args);

    assert!(!output.status.success());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("terminal"), "{stderr}");
    assert!(
        stderr.contains(&cloud.url),
        "the hint names the dashboard:\n{stderr}"
    );
    assert!(
        !cloud
            .routes()
            .iter()
            .any(|route| route.starts_with("POST /api/cli/run/")),
        "nothing ran"
    );
}

#[test]
fn a_table_render_prints_one_row_per_item() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("table");
    let columns: Vec<String> = entry["output"]["items"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let row: serde_json::Map<String, Value> = columns
        .iter()
        .map(|column| (column.clone(), Value::from(format!("{column}-cell"))))
        .collect();
    cloud.answer_runs_with(200, json!({ "output": [row] }));
    let args = argv(&entry);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = cloud.ployz(&args);

    assert!(output.status.success(), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    for column in &columns {
        assert!(stdout.contains(&format!("{column}-cell")), "{stdout}");
    }
}

#[test]
fn a_command_line_that_misses_without_a_cloud_never_touches_the_network() {
    let cloud = Fixture::new("one");
    let entry = entry_rendering("record");
    let args = argv(&entry);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let output = Command::new(env!("CARGO_BIN_EXE_ployz"))
        .args(&args)
        .env("HOME", cloud.home.path())
        .env("PLOYZ_CONFIG", cloud.config())
        .env_remove("PLOYZ_TOKEN")
        .env_remove("PLOYZ_CLOUD_URL")
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(cloud.routes().is_empty(), "{:?}", cloud.routes());
}
