//! Every surface shows the commands Ployz Cloud publishes the same way: `ployz schema`
//! lists each with its arguments under origin "cloud", `ployz mcp` offers each one
//! marked for agents with the entry's own input schema, a name a compiled tool owns
//! stays compiled, and a local Store shows none.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};

const FIXTURE: &str = include_str!("fixtures/published-manifest.json");

struct Cloud {
    manifest: Value,
    routes: Vec<String>,
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
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header == "\r\n" {
                    break;
                }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let route = request_line.rsplit_once(' ').unwrap().0.to_owned();
            let mut cloud = cloud.lock().unwrap();
            cloud.routes.push(route.clone());
            let (status, body) = match route.as_str() {
                "GET /api/cli/manifest" => (200, cloud.manifest.clone()),
                run if run.starts_with("POST /api/cli/run/") => (200, json!({ "output": {} })),
                _ => (404, json!({})),
            };
            let revision = cloud.manifest["revision"].as_str().unwrap().to_owned();
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
    fn serving(manifest: Value) -> Self {
        let cloud = Arc::new(Mutex::new(Cloud {
            manifest,
            routes: Vec::new(),
        }));
        let url = fake_cloud(Arc::clone(&cloud));
        Self {
            cloud,
            url,
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn new() -> Self {
        Self::serving(serde_json::from_str(FIXTURE).unwrap())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
        command
            .args(args)
            .env("HOME", self.home.path())
            .env("PLOYZ_CONFIG", self.home.path().join("config.yaml"))
            .env("PLOYZ_TOKEN", "ployz_test_token")
            .env("PLOYZ_CLOUD_URL", &self.url)
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .env("NO_COLOR", "1");
        command
    }

    /// The first run of any published command fills the cache `ployz schema` reads.
    fn warm(&self) {
        let entry = &entries()[0];
        let path: Vec<&str> = entry["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment.as_str().unwrap())
            .collect();
        let output = self.command(&path).output().unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
    }

    fn schema_commands(&self) -> Vec<Value> {
        let output = self.command(&["schema"]).output().unwrap();
        assert!(output.status.success(), "{}", text(&output.stderr));
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["x-ployz-commands"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn mcp(&self, local: bool) -> Mcp {
        let mut command = self.command(&["mcp"]);
        if local {
            command.env(
                "PLOYZ_STORE",
                format!("sqlite:{}", self.home.path().join("store.db").display()),
            );
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp {
            child,
            stdin,
            stdout,
        };
        mcp.initialize();
        mcp
    }

    fn routes(&self) -> Vec<String> {
        self.cloud.lock().unwrap().routes.clone()
    }
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Mcp {
    fn send(&mut self, frame: &Value) {
        self.stdin
            .write_all(format!("{frame}\n").as_bytes())
            .unwrap();
        self.stdin.flush().unwrap();
    }

    fn reply(&mut self, id: u64) -> Value {
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "the server closed stdout before answering {id}"
            );
            let frame: Value = serde_json::from_str(&line).unwrap();
            if frame["id"] == id {
                return frame;
            }
        }
    }

    fn initialize(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        }));
        let reply = self.reply(1);
        assert!(reply["result"]["serverInfo"].is_object(), "{reply}");
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    }

    fn tools(&mut self) -> Vec<Value> {
        self.send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }));
        self.reply(2)["result"]["tools"].as_array().unwrap().clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn entries() -> Vec<Value> {
    serde_json::from_str::<Value>(FIXTURE).unwrap()["commands"]
        .as_array()
        .unwrap()
        .clone()
}

fn name(entry: &Value) -> &str {
    entry["name"].as_str().unwrap()
}

fn path(entry: &Value) -> String {
    entry["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|segment| segment.as_str().unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn named<'a>(tools: &'a [Value], name: &str) -> Option<&'a Value> {
    tools.iter().find(|tool| tool["name"] == name)
}

/// The argument names `ployz schema` lists for an entry: `--name` for a flag, the
/// upper-cased name for a positional, and `--approval` when the gate asks.
fn expected_args(entry: &Value) -> Vec<String> {
    let positional: Vec<&str> = entry["positional"]
        .as_array()
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut args: Vec<String> = entry["input"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(|property| {
            if positional.contains(&property.as_str()) {
                property.to_uppercase()
            } else {
                format!("--{property}")
            }
        })
        .collect();
    if entry["gate"] == "ask" {
        args.push("--approval".to_owned());
    }
    args.sort();
    args
}

#[test]
fn schema_lists_every_published_entry_under_origin_cloud_with_one_arg_per_property() {
    let fixture = Fixture::new();
    fixture.warm();
    let commands = fixture.schema_commands();
    let entries = entries();
    assert!(!entries.is_empty(), "the fixture publishes something");
    for entry in &entries {
        let listed = commands
            .iter()
            .find(|command| command["command"] == path(entry))
            .unwrap_or_else(|| panic!("schema lists {}", path(entry)));
        assert_eq!(listed["origin"], "cloud", "{listed}");
        assert_eq!(listed["surface"], "cloud", "{listed}");
        assert_eq!(listed["about"], entry["about"], "{listed}");
        let mut args: Vec<String> = listed["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg["name"].as_str().unwrap().to_owned())
            .collect();
        args.sort();
        assert_eq!(args, expected_args(entry), "{}", path(entry));
        let required: Vec<&str> = listed["args"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|arg| arg["required"] == true)
            .map(|arg| arg["name"].as_str().unwrap())
            .collect();
        let schema_required: Vec<&str> = entry["input"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect();
        assert_eq!(
            required.len(),
            schema_required.len(),
            "{} requires {schema_required:?}, schema says {required:?}",
            path(entry)
        );
    }
    let compiled = commands
        .iter()
        .filter(|command| command["origin"] == "compiled")
        .count();
    assert!(compiled > 0, "compiled commands keep their origin");
    assert_eq!(
        commands.len(),
        compiled + entries.len(),
        "no entry is listed twice"
    );
}

#[test]
fn mcp_offers_a_published_entry_only_when_marked_for_agents_with_the_entry_s_input_schema() {
    let fixture = Fixture::new();
    let mut mcp = fixture.mcp(false);
    let tools = mcp.tools();
    let entries = entries();
    let agent_facing = entries
        .iter()
        .filter(|entry| entry["agents"] == true)
        .count();
    assert!(
        agent_facing > 0 && agent_facing < entries.len(),
        "the fixture has entries of both kinds"
    );
    for entry in &entries {
        let tool = named(&tools, name(entry));
        if entry["agents"] != true {
            assert!(tool.is_none(), "{} is not for agents", name(entry));
            continue;
        }
        let tool = tool.unwrap_or_else(|| panic!("mcp lists {}", name(entry)));
        assert_eq!(tool["description"], entry["about"], "{tool}");
        if entry["gate"] == "ask" {
            assert!(tool["inputSchema"]["properties"]["approval"].is_object());
            for (property, schema) in entry["input"]["properties"].as_object().unwrap() {
                assert_eq!(&tool["inputSchema"]["properties"][property], schema);
            }
        } else {
            assert_eq!(tool["inputSchema"], entry["input"], "{tool}");
        }
        assert_eq!(tool["outputSchema"], entry["output"], "{tool}");
        let read = entry["kind"] == "read";
        assert_eq!(tool["annotations"]["readOnlyHint"], read, "{tool}");
    }
    assert_eq!(
        fixture.routes(),
        vec!["GET /api/cli/manifest"],
        "listing costs one manifest read"
    );
}

#[test]
fn a_published_name_a_compiled_tool_owns_stays_compiled_on_every_surface() {
    let mut manifest: Value = serde_json::from_str(FIXTURE).unwrap();
    let empty =
        Fixture::serving(json!({ "format": 1, "revision": "000000000000", "commands": [] }));
    let compiled = empty.mcp(false).tools();
    let taken = compiled[0].clone();
    let impostor = manifest["commands"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["agents"] == true)
        .expect("an agent-facing entry to rename");
    impostor["name"] = taken["name"].clone();
    impostor["about"] = json!("published impostor");
    let impostor_path = path(impostor);
    let fixture = Fixture::serving(manifest);
    let tools = fixture.mcp(false).tools();
    let listed = named(&tools, taken["name"].as_str().unwrap()).unwrap();
    assert_eq!(listed["description"], taken["description"], "{listed}");
    assert_eq!(
        tools
            .iter()
            .filter(|tool| tool["name"] == taken["name"])
            .count(),
        1
    );
    let commands = fixture.schema_commands();
    assert!(
        !commands
            .iter()
            .any(|command| command["command"] == impostor_path),
        "the impostor's path is not grafted"
    );
}

#[test]
fn a_local_store_offers_no_published_tool_even_with_a_warm_cache() {
    let fixture = Fixture::new();
    fixture.warm();
    let before = fixture.routes().len();
    let tools = fixture.mcp(true).tools();
    for entry in entries() {
        assert!(
            named(&tools, name(&entry)).is_none(),
            "{} is a Cloud command, not a local one",
            name(&entry)
        );
    }
    assert_eq!(
        fixture.routes().len(),
        before,
        "a local Store never asks Cloud"
    );
}
