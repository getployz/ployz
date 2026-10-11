use std::io::{BufRead, Read, Write as _};
use std::sync::mpsc::{Receiver, channel};

use ployz_store::tools::Answer as Answered;
use ployz_store::{
    CreateProject, EnvironmentId, OrganizationId, ProjectId, ProjectName, SealingKey,
};
use serde::Deserialize;
use tokio::io::{AsyncWriteExt, BufReader as AsyncBufReader};

use super::*;

#[derive(Deserialize)]
struct Transcript {
    tool: String,
    input: Value,
    place: Place,
    turns: Vec<Turn>,
}

#[derive(Deserialize)]
struct Turn {
    step: Step,
    #[serde(default)]
    answers: Vec<Answered>,
}

fn transcripts() -> Vec<Transcript> {
    serde_json::from_str(include_str!(
        "../../../../ployz-store/tests/snapshots/agent_transcripts.json"
    ))
    .unwrap()
}

fn transcript(tool: &str, store: StoreAt) -> Transcript {
    transcripts()
        .into_iter()
        .find(|each| each.tool == tool && each.place.store == store)
        .unwrap()
}

/// One request Cloud expects, by path and body, and what it answers.
struct Scripted {
    path: String,
    body: Value,
    answer: Answered,
}

fn asked(ask: &Ask) -> (String, Value) {
    match ask {
        Ask::Store(query) => ("/api/config/read".to_owned(), json!(query)),
        Ask::Cloud(query) => ("/api/cli/agent/ask".to_owned(), json!({ "ask": query })),
    }
}

fn written(write: &Write) -> (String, Value) {
    match write {
        Write::Store(command) => ("/api/config/write".to_owned(), json!(command)),
        Write::Act(act) => ("/api/cli/agent/act".to_owned(), json!({ "act": act })),
    }
}

/// Every request the transcript's steps make of Cloud, answered as recorded.
fn script(transcript: &Transcript) -> Vec<Scripted> {
    transcript
        .turns
        .iter()
        .flat_map(|turn| {
            let requests = match &turn.step {
                Step::Read { asks } | Step::Wait { then: asks, .. } => {
                    asks.iter().map(asked).collect()
                }
                Step::Write { write } => vec![written(write)],
                Step::Done { .. } => Vec::new(),
            };
            requests
                .into_iter()
                .zip(turn.answers.iter().cloned())
                .map(|((path, body), answer)| Scripted { path, body, answer })
        })
        .collect()
}

fn outcome(transcript: &Transcript) -> (Value, bool) {
    let Some(Step::Done { result }) = transcript.turns.last().map(|turn| &turn.step) else {
        panic!("{} ends in a result", transcript.tool);
    };
    match result {
        ToolResult::Ok(value) => (value.clone(), false),
        ToolResult::Refused(refusal) => (json!(refusal), true),
    }
}

#[derive(Debug)]
struct Seen {
    path: String,
    approval: Option<String>,
    bearer: String,
    body: Value,
}

struct Fake {
    url: String,
    seen: Receiver<Seen>,
}

impl Fake {
    fn requests(&self) -> Vec<Seen> {
        self.seen.try_iter().collect()
    }
}

fn is_write(path: &str) -> bool {
    matches!(path, "/api/config/write" | "/api/cli/agent/act")
}

/// A Ployz Cloud that answers `script` and records each decision on an approval. With
/// `asking`, a write without an approval gets that `approval_required` refusal, and one
/// with an approval waits `slow` before its answer.
fn fake_cloud(mut script: Vec<Scripted>, asking: Option<Value>, slow: Duration) -> Fake {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (saw, seen) = channel();
    std::thread::spawn(move || {
        let mut decided = HashMap::new();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let (mut length, mut bearer, mut approval) = (0, String::new(), None);
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() {
                    break;
                }
                let header = header.to_ascii_lowercase();
                if let Some(value) = header.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
                if let Some(value) = header.strip_prefix("authorization: bearer ") {
                    value.trim().clone_into(&mut bearer);
                }
                if let Some(value) = header.strip_prefix("x-ployz-approval:") {
                    approval = Some(value.trim().to_owned());
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            let path = request
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let (status, reply) = if path.starts_with("/api/cli/approvals/") {
                decide(&mut decided, &path, &body)
            } else if let (Some(asking), true, None) = (&asking, is_write(&path), &approval) {
                ("409 Conflict", asking.to_string())
            } else if let Some(at) = script
                .iter()
                .position(|each| each.path == path && each.body == body)
            {
                if approval.is_some() {
                    std::thread::sleep(slow);
                }
                match script.remove(at).answer {
                    Answered::Value(value) => ("200 OK", value.to_string()),
                    Answered::Error(error) => (
                        "422 Unprocessable Entity",
                        json!({ "error": error }).to_string(),
                    ),
                }
            } else {
                (
                    "500 Internal Server Error",
                    json!({ "error": { "code": "internal", "message": format!("unscripted {path} {body}") } })
                        .to_string(),
                )
            };
            let _ = saw.send(Seen {
                path,
                approval,
                bearer,
                body,
            });
            write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                 content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    Fake { url, seen }
}

fn decide(
    decided: &mut HashMap<String, Value>,
    path: &str,
    decision: &Value,
) -> (&'static str, String) {
    if !cloud_takes(decision) {
        return (
            "422 Unprocessable Entity",
            r#"{"error":{"code":"invalid_request","message":"not a decision"}}"#.to_owned(),
        );
    }
    if decided
        .get(path)
        .is_some_and(|first: &Value| first.get("approve") != decision.get("approve"))
    {
        return (
            "409 Conflict",
            r#"{"error":{"code":"conflict","message":"This approval was already decided elsewhere (approved)."}}"#
                .to_owned(),
        );
    }
    decided.insert(path.to_owned(), decision.clone());
    ("200 OK", "{}".to_owned())
}

/// Cloud's `ApprovalDecision`: an approval names a digest, and a rejection's reason is
/// a string or absent.
fn cloud_takes(decision: &Value) -> bool {
    match (decision.get("approve"), decision.get("reject")) {
        (Some(approve), None) => approve
            .get("digest")
            .and_then(Value::as_str)
            .is_some_and(|digest| !digest.is_empty()),
        (None, Some(reject)) => reject
            .as_object()
            .is_some_and(|reject| reject.get("reason").is_none_or(Value::is_string)),
        _ => false,
    }
}

fn decisions(seen: &[Seen]) -> Vec<Value> {
    seen.iter()
        .filter(|seen| seen.path.starts_with("/api/cli/approvals/"))
        .map(|seen| seen.body.clone())
        .collect()
}

fn writes(seen: &[Seen]) -> Vec<&Seen> {
    seen.iter().filter(|seen| is_write(&seen.path)).collect()
}

fn on_cloud(fake: &Fake, place: Place) -> Server {
    Server {
        keys: |_, _, tool| format!("call-{tool}"),
        ..Server::new(
            Account {
                config: PathBuf::from("/nonexistent/config.toml"),
                token: Some("ployz_acme".to_owned()),
                cloud: Some(fake.url.clone()),
            },
            Store::Cloud,
            place,
        )
    }
}

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

fn shop() -> ConfigStore {
    let store = ConfigStore::open(
        "sqlite::memory:",
        SealingKey::new(b"test-encryption-secret").unwrap(),
    )
    .unwrap();
    store
        .write(
            &who(),
            &CreateProject {
                id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse("00000000-0000-4000-8000-000000000002")
                    .unwrap(),
            },
        )
        .unwrap();
    store
}

fn on_local(place: Place) -> Server {
    Server {
        keys: |_, _, tool| format!("call-{tool}"),
        ..Server::new(
            Account::default(),
            Store::Local(Arc::new(shop()), who()),
            place,
        )
    }
}

struct Client {
    lines: tokio::io::Lines<AsyncBufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
    server: tokio::task::JoinHandle<()>,
}

impl Client {
    fn connect(server: Server) -> Self {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            let (read, write) = tokio::io::split(server_io);
            server
                .serve(Lines::new(read, write))
                .await
                .expect("the handshake completes")
                .waiting()
                .await
                .expect("the server stops cleanly");
        });
        let (read, write) = tokio::io::split(client_io);
        Self {
            lines: AsyncBufReader::new(read).lines(),
            write,
            server,
        }
    }

    async fn initialized(server: Server, capabilities: Value) -> Self {
        let mut client = Self::connect(server);
        client
            .send(&request(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": capabilities,
                    "clientInfo": { "name": "test", "version": "0" },
                }),
            ))
            .await;
        client.reply().await;
        client
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await;
        client
    }

    async fn send(&mut self, frame: &Value) {
        self.write
            .write_all(format!("{frame}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn reply(&mut self) -> Value {
        let line = self.lines.next_line().await.unwrap().expect("a reply");
        serde_json::from_str(&line).expect("a JSON-RPC frame")
    }

    async fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        self.send(&request(
            id,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        ))
        .await;
        self.reply().await
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn replays(transcript: &Transcript, server: Server) {
    let mut client = Client::initialized(server, json!({})).await;
    let done = client
        .call(2, &transcript.tool, transcript.input.clone())
        .await;
    let (result, refused) = outcome(transcript);
    assert_eq!(
        done["result"]["structuredContent"], result,
        "{} at {:?}: {done}",
        transcript.tool, transcript.place.store
    );
    assert_eq!(
        done["result"]["isError"], refused,
        "{}: {done}",
        transcript.tool
    );
}

#[tokio::test]
async fn every_cloud_transcript_replays_through_ployz_mcp() {
    for transcript in transcripts()
        .iter()
        .filter(|each| each.place.store == StoreAt::Cloud)
    {
        let fake = fake_cloud(script(transcript), None, Duration::ZERO);
        replays(transcript, on_cloud(&fake, transcript.place.clone())).await;
        let seen = fake.requests();
        let expected = script(transcript);
        assert_eq!(seen.len(), expected.len(), "{}: {seen:?}", transcript.tool);
        assert!(
            seen.iter()
                .all(|seen| seen.bearer == "ployz_acme" && seen.approval.is_none()),
            "{}: {seen:?}",
            transcript.tool
        );
    }
}

#[tokio::test]
async fn every_local_transcript_replays_against_a_local_store() {
    for transcript in transcripts()
        .iter()
        .filter(|each| each.place.store == StoreAt::Local)
    {
        replays(transcript, on_local(transcript.place.clone())).await;
    }
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_cloud_tool_on_a_local_store_is_not_here_and_names_the_fix() {
    let transcript = transcript("server_remove", StoreAt::Local);
    let mut client = Client::initialized(on_local(transcript.place.clone()), json!({})).await;
    let done = client
        .call(2, "server_remove", transcript.input.clone())
        .await;
    assert_eq!(done["result"]["isError"], true, "{done}");
    let refusal = &done["result"]["structuredContent"];
    assert_eq!(refusal["code"], "not_here", "{done}");
    assert_eq!(refusal["writes"], "none", "{done}");
    assert_eq!(
        refusal["details"]["fix"],
        json!({ "fix": "use_cloud" }),
        "{done}"
    );
}

#[tokio::test]
async fn a_local_backend_answers_a_cloud_ask_not_here() {
    let store = shop();
    let who = who();
    let backend = Backend::Local(&store, &who);
    let servers = serde_json::from_value(json!({ "query": "servers", "only": null })).unwrap();
    let answered = backend.ask(&Ask::Cloud(servers)).await.unwrap_err();
    assert_eq!(answered.code.as_str(), "not_here");
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn tools_list_serves_the_catalog_with_its_hints_and_schemas() {
    let place = transcript("deploy", StoreAt::Cloud).place;
    let mut client = Client::initialized(on_local(place), json!({})).await;
    client.send(&request(2, "tools/list", json!({}))).await;
    let listed = client.reply().await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let catalog = tools::catalog();
    assert_eq!(tools.len(), catalog.len(), "{listed}");
    for def in &catalog {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == def.name)
            .unwrap_or_else(|| panic!("{} is listed", def.name));
        assert_eq!(tool["annotations"]["readOnlyHint"], def.read_only, "{tool}");
        assert_eq!(
            tool["annotations"]["destructiveHint"],
            def.gate != Gate::Never,
            "{tool}"
        );
        assert_eq!(tool["outputSchema"], def.output_schema, "{tool}");
        assert_eq!(
            tool["inputSchema"]["properties"]["approval"]["type"] == "string",
            def.gate != Gate::Never,
            "{tool}"
        );
    }
}

#[tokio::test]
async fn the_instructions_are_the_registry_s() {
    let place = transcript("deploy", StoreAt::Cloud).place;
    let mut client = Client::connect(on_local(place));
    client
        .send(&request(
            1,
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            }),
        ))
        .await;
    let initialized = client.reply().await;
    let instructions = initialized
        .pointer("/result/instructions")
        .and_then(Value::as_str)
        .unwrap();
    assert!(instructions.starts_with(INSTRUCTIONS), "{instructions}");
    assert!(instructions.contains("30 minutes"), "{instructions}");
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_line_that_is_not_json_gets_a_parse_error_and_the_server_keeps_serving() {
    let place = transcript("deploy", StoreAt::Cloud).place;
    let mut client = Client::initialized(on_local(place), json!({})).await;
    client.write.write_all(b"{not json\n").await.unwrap();
    let parse = client.reply().await;
    assert_eq!(parse["error"]["code"], -32700, "{parse}");
    assert_eq!(parse["id"], Value::Null, "{parse}");
    client.send(&json!(["not", "a", "message"])).await;
    let invalid = client.reply().await;
    assert_eq!(invalid["error"]["code"], -32600, "{invalid}");
    client
        .send(&json!({ "jsonrpc": "2.0", "method": "notifications/unknown" }))
        .await;
    client.send(&request(2, "tools/list", json!({}))).await;
    let listed = client.reply().await;
    assert_eq!(listed["id"], 2, "{listed}");
    assert!(listed["result"]["tools"].is_array(), "{listed}");
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn an_unknown_tool_is_an_invalid_params_error() {
    let place = transcript("deploy", StoreAt::Cloud).place;
    let mut client = Client::initialized(on_local(place), json!({})).await;
    let unknown = client.call(2, "kubectl", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    assert_eq!(unknown["error"]["message"], "no tool named kubectl");
}

#[tokio::test]
async fn a_replayed_request_without_a_key_writes_under_the_same_id() {
    let transcript = transcript("service_add", StoreAt::Cloud);
    let fake = fake_cloud(
        (0..4).flat_map(|_| script(&transcript)).collect(),
        None,
        Duration::ZERO,
    );
    let unkeyed = |fake: &Fake| Server {
        keys: call_key,
        ..on_cloud(fake, transcript.place.clone())
    };
    let mut client = Client::initialized(unkeyed(&fake), json!({})).await;
    for id in [2, 2, 3] {
        client
            .call(id, &transcript.tool, transcript.input.clone())
            .await;
    }
    let mut restarted = Client::initialized(unkeyed(&fake), json!({})).await;
    restarted
        .call(2, &transcript.tool, transcript.input.clone())
        .await;
    let seen = fake.requests();
    let bodies: Vec<&Value> = writes(&seen).iter().map(|seen| &seen.body).collect();
    let [first, replayed, next, after_restart] = bodies[..] else {
        panic!("four writes: {bodies:?}");
    };
    assert_eq!(first, replayed, "a replay keeps its id");
    assert_ne!(first, next, "another request gets its own id");
    assert_ne!(first, after_restart, "a restarted server reuses no id");
}

fn deploy_asks() -> Value {
    json!({ "error": {
        "code": "approval_required",
        "message": "A human must approve this first: this deploy removes Service worker",
        "details": {
            "approval_id": "apr_1",
            "approval": "3:abc",
            "effects": [
                { "kind": "removes_service", "name": "worker", "node": "svc_1", "path": "worker" },
            ],
            "diff": {
                "environment": {
                    "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                    "project": "shop",
                    "name": "production",
                    "revision": 3,
                },
                "version": "3:abc",
                "saved": 2,
                "published": false,
                "changes": [],
                "total_count": 3,
            },
        },
    }})
}

const REVIEW: &str =
    "This deploy destroys 1 thing:\n  \u{2715} removes Service worker\n  + 2 other changes";

struct Approving {
    fake: Fake,
    server: Server,
    deploy: Transcript,
}

fn approving_with(slow: Duration) -> Approving {
    let deploy = transcript("deploy", StoreAt::Cloud);
    // A second call on the same server reads again before it writes.
    let mut twice = script(&deploy);
    twice.extend(script(&deploy));
    let fake = fake_cloud(twice, Some(deploy_asks()), slow);
    let server = on_cloud(&fake, deploy.place.clone());
    Approving {
        fake,
        server,
        deploy,
    }
}

fn approving() -> Approving {
    approving_with(Duration::ZERO)
}

/// The deploy wrote twice, the second time with the approval and the same command.
fn resent(seen: &[Seen]) {
    let [first, second] = writes(seen)[..] else {
        panic!("expected two writes: {seen:?}");
    };
    assert_eq!(first.approval, None, "{seen:?}");
    assert_eq!(second.approval.as_deref(), Some("apr_1"), "{seen:?}");
    assert_eq!(first.body, second.body, "{seen:?}");
}

/// The deploy wrote once, without an approval.
fn wrote_once(seen: &[Seen]) {
    let [write] = writes(seen)[..] else {
        panic!("expected one write: {seen:?}");
    };
    assert_eq!(write.approval, None, "{seen:?}");
}

async fn eliciting(server: Server) -> Client {
    Client::initialized(server, json!({ "elicitation": {} })).await
}

async fn reply_to(client: &mut Client, asking: &Value, result: Value) -> Value {
    client
        .send(&json!({ "jsonrpc": "2.0", "id": asking.get("id"), "result": result }))
        .await;
    client.reply().await
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_host_that_elicits_asks_the_human_and_an_approval_resends_the_write() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = eliciting(server).await;
    client
        .send(&request(
            2,
            "tools/call",
            json!({ "name": "deploy", "arguments": deploy.input }),
        ))
        .await;
    let asking = client.reply().await;
    assert_eq!(asking["method"], "elicitation/create", "{asking}");
    assert_eq!(
        asking["params"]["message"],
        format!("Approve this deploy to production?\n{REVIEW}")
    );
    let schema = &asking["params"]["requestedSchema"];
    assert_eq!(
        schema["properties"]["approve"]["type"], "boolean",
        "{schema}"
    );
    assert_eq!(schema["properties"]["reason"]["type"], "string", "{schema}");
    assert_eq!(
        schema["properties"]["reason"]["maxLength"], 1024,
        "{schema}"
    );
    assert_eq!(schema["properties"]["name"], Value::Null, "{schema}");
    assert_eq!(schema["required"], json!(["approve"]), "{schema}");
    let done = reply_to(
        &mut client,
        &asking,
        json!({ "action": "accept", "content": { "approve": true } }),
    )
    .await;
    assert_eq!(done["id"], 2, "{done}");
    let (result, refused) = outcome(&deploy);
    assert_eq!(done["result"]["structuredContent"], result, "{done}");
    assert_eq!(done["result"]["isError"], refused, "{done}");
    let seen = fake.requests();
    resent(&seen);
    assert_eq!(
        decisions(&seen),
        [json!({ "approve": { "digest": "3:abc" } })]
    );
    let decided = seen
        .iter()
        .find(|seen| seen.path.starts_with("/api/cli/approvals/"))
        .unwrap();
    assert_eq!(decided.path, "/api/cli/approvals/apr_1");
    assert_eq!(decided.bearer, "ployz_acme");
}

fn removal_asks() -> Value {
    json!({ "error": {
        "code": "approval_required",
        "message": "A human must approve this first: this removes Project trial",
        "details": {
            "approval_id": "apr_1",
            "approval": "7:def",
            "effects": [],
            "operation": { "verb": "remove", "name": "trial" },
        },
    }})
}

async fn remove_trial(content: Value) -> (Value, Value, Vec<Seen>) {
    let removal = transcript("project_remove", StoreAt::Cloud);
    let fake = fake_cloud(script(&removal), Some(removal_asks()), Duration::ZERO);
    let mut client = eliciting(on_cloud(&fake, removal.place.clone())).await;
    client
        .send(&request(
            2,
            "tools/call",
            json!({ "name": "project_remove", "arguments": removal.input }),
        ))
        .await;
    let asking = client.reply().await;
    let done = reply_to(
        &mut client,
        &asking,
        json!({ "action": "accept", "content": content }),
    )
    .await;
    (asking, done, fake.requests())
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_removal_asks_the_human_to_type_its_name() {
    let (asking, done, seen) = remove_trial(json!({ "approve": true, "name": " trial " })).await;
    assert_eq!(
        asking["params"]["message"],
        "Approve this removal of trial?\nThis removal destroys 0 things:"
    );
    let name = &asking["params"]["requestedSchema"]["properties"]["name"];
    assert_eq!(name["type"], "string", "{asking}");
    assert_eq!(name["description"], "Type trial to approve", "{asking}");
    assert_eq!(
        done["result"]["structuredContent"]["code"], "not_found",
        "{done}"
    );
    resent(&seen);
    assert_eq!(
        decisions(&seen),
        [json!({ "approve": { "digest": "7:def" } })]
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_removal_with_the_wrong_name_typed_decides_nothing_and_writes_nothing() {
    let (_, done, seen) = remove_trial(json!({ "approve": true, "name": "shop" })).await;
    assert_eq!(done["result"]["isError"], true, "{done}");
    assert_eq!(
        done["result"]["content"][0]["text"],
        "The human did not type `trial`, so the answer was not recorded and nothing was \
         removed. The approval of this removal of trial stays pending in Ployz Cloud; call \
         the tool again to ask again."
    );
    assert!(decisions(&seen).is_empty(), "{seen:?}");
    wrote_once(&seen);
}

async fn unticked(content: Value) -> (Value, Vec<Value>) {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = eliciting(server).await;
    client
        .send(&request(
            2,
            "tools/call",
            json!({ "name": "deploy", "arguments": deploy.input }),
        ))
        .await;
    let asking = client.reply().await;
    let denied = reply_to(
        &mut client,
        &asking,
        json!({ "action": "accept", "content": content }),
    )
    .await;
    let seen = fake.requests();
    wrote_once(&seen);
    (denied, decisions(&seen))
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_human_who_unticks_approve_denies_it_in_cloud_with_their_reason() {
    let (denied, decided) =
        unticked(json!({ "approve": false, "reason": "  the worker still drains " })).await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(
        denied["result"]["content"][0]["text"],
        "The human denied this deploy: the worker still drains. Nothing was deployed; do \
         not retry it unless they ask."
    );
    assert_eq!(
        decided,
        [json!({ "reject": { "reason": "the worker still drains" } })]
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_human_who_unticks_approve_without_a_reason_denies_it_in_cloud() {
    let (denied, decided) = unticked(json!({ "approve": false, "reason": " " })).await;
    assert_eq!(
        denied["result"]["content"][0]["text"],
        "The human denied this deploy. Nothing was deployed; do not retry it unless they ask."
    );
    assert_eq!(decided, [json!({ "reject": {} })]);
}

#[tokio::test]
async fn a_reason_is_cut_to_what_cloud_keeps() {
    let (_, decided) =
        unticked(json!({ "approve": false, "reason": "\u{1F600}".repeat(600) })).await;
    let reason = decided
        .first()
        .and_then(|decided| decided.pointer("/reject/reason"))
        .and_then(Value::as_str)
        .unwrap();
    assert_eq!(reason.encode_utf16().count(), 1024);
}

/// The `_meta` Claude Code 2.1.294 puts on every request: it speaks 2026-07-28, which
/// has no `initialize`, so its capabilities arrive per request.
fn claude_code() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": { "name": "claude-code", "version": "2.1.294" },
        "io.modelcontextprotocol/clientCapabilities": {
            "roots": { "listChanged": true },
            "elicitation": { "form": {}, "url": {} },
        },
    })
}

async fn ask_as_claude_code(client: &mut Client, arguments: &Value) -> Value {
    client
        .send(&request(
            1,
            "server/discover",
            json!({ "_meta": claude_code() }),
        ))
        .await;
    let discovered = client.reply().await;
    assert!(
        discovered
            .pointer("/result/supportedVersions")
            .and_then(Value::as_array)
            .unwrap()
            .contains(&json!("2026-07-28")),
        "{discovered}"
    );
    client
        .send(&request(
            2,
            "tools/call",
            json!({ "_meta": claude_code(), "name": "deploy", "arguments": arguments }),
        ))
        .await;
    client.reply().await
}

async fn answer_on(
    client: &mut Client,
    tool: &str,
    arguments: &Value,
    asking: &Value,
    answer: Value,
) -> Value {
    client
        .send(&request(
            3,
            "tools/call",
            json!({
                "_meta": claude_code(),
                "name": tool,
                "arguments": arguments,
                "inputResponses": { "approval": answer },
                "requestState": asking.pointer("/result/requestState"),
            }),
        ))
        .await;
    client.reply().await
}

fn approve() -> Value {
    json!({ "action": "accept", "content": { "approve": true } })
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_host_without_initialize_gets_the_dialog_in_the_result_and_its_approval_resends_the_write()
 {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = Client::connect(server);
    let asking = ask_as_claude_code(&mut client, &deploy.input).await;
    assert_eq!(asking["id"], 2, "{asking}");
    assert_eq!(asking["result"]["resultType"], "input_required", "{asking}");
    let dialog = &asking["result"]["inputRequests"]["approval"];
    assert_eq!(dialog["method"], "elicitation/create", "{asking}");
    assert_eq!(dialog["params"]["mode"], "form", "{asking}");
    assert_eq!(
        dialog["params"]["message"],
        format!("Approve this deploy to production?\n{REVIEW}")
    );
    assert_eq!(
        dialog["params"]["requestedSchema"]["required"],
        json!(["approve"])
    );
    wrote_once(&fake.requests());
    let done = answer_on(&mut client, "deploy", &deploy.input, &asking, approve()).await;
    assert_eq!(done["id"], 3, "{done}");
    let (result, _) = outcome(&deploy);
    assert_eq!(done["result"]["structuredContent"], result, "{done}");
    let seen = fake.requests();
    let resend = writes(&seen);
    assert_eq!(resend.len(), 1, "{seen:?}");
    assert_eq!(resend[0].approval.as_deref(), Some("apr_1"), "{seen:?}");
    assert_eq!(
        decisions(&seen),
        [json!({ "approve": { "digest": "3:abc" } })]
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn an_answer_after_the_deadline_decides_nothing_and_writes_nothing() {
    let Approving {
        fake,
        mut server,
        deploy,
    } = approving();
    server.deadline = Duration::from_secs(1);
    let mut client = Client::connect(server);
    let asking = ask_as_claude_code(&mut client, &deploy.input).await;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let late = answer_on(&mut client, "deploy", &deploy.input, &asking, approve()).await;
    assert_eq!(late["result"]["isError"], true, "{late}");
    assert_eq!(
        late["result"]["content"][0]["text"],
        "The human answered more than 1 seconds after this deploy to production was called, \
         so the answer was not recorded and nothing was deployed. The approval of this \
         deploy to production stays pending in Ployz Cloud; call the tool again to ask again."
    );
    let seen = fake.requests();
    assert!(decisions(&seen).is_empty(), "{seen:?}");
    wrote_once(&seen);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn an_answered_call_keeps_the_deadline_of_the_call_that_asked() {
    let Approving {
        fake,
        mut server,
        deploy,
    } = approving_with(Duration::from_secs(5));
    server.deadline = Duration::from_secs(3);
    let mut client = Client::connect(server);
    let asking = ask_as_claude_code(&mut client, &deploy.input).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let stopped = answer_on(&mut client, "deploy", &deploy.input, &asking, approve()).await;
    assert_eq!(
        stopped["result"]["content"][0]["text"],
        "ployz mcp stopped `deploy` after 3 seconds. What it started may still be running; \
         check with `deployment_show`."
    );
    assert_eq!(
        decisions(&fake.requests()),
        [json!({ "approve": { "digest": "3:abc" } })]
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn an_answer_on_another_call_is_refused_and_keeps_the_dialog() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = Client::connect(server);
    let asking = ask_as_claude_code(&mut client, &deploy.input).await;
    let mut other = deploy.input.clone();
    other["message"] = json!("Something else");
    for (tool, arguments) in [("server_show", json!({})), ("deploy", other)] {
        let refused = answer_on(&mut client, tool, &arguments, &asking, approve()).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
        assert_eq!(
            refused["error"]["message"],
            "this approval was asked by another call; answer it by calling `deploy` again \
             with the same arguments"
        );
    }
    let seen = fake.requests();
    assert!(decisions(&seen).is_empty(), "{seen:?}");
    wrote_once(&seen);
    let done = answer_on(&mut client, "deploy", &deploy.input, &asking, approve()).await;
    assert_eq!(
        done["result"]["structuredContent"],
        outcome(&deploy).0,
        "{done}"
    );
    assert_eq!(
        decisions(&fake.requests()),
        [json!({ "approve": { "digest": "3:abc" } })]
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_replayed_or_unknown_answer_is_refused_and_writes_nothing() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = Client::connect(server);
    let asking = ask_as_claude_code(&mut client, &deploy.input).await;
    answer_on(
        &mut client,
        "deploy",
        &deploy.input,
        &asking,
        json!({ "action": "decline" }),
    )
    .await;
    let unknown = json!({ "result": { "requestState": "apr_1" } });
    for asking in [&asking, &unknown] {
        let refused = answer_on(&mut client, "deploy", &deploy.input, asking, approve()).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
        assert_eq!(
            refused["error"]["message"],
            "no approval waits on this request state"
        );
    }
    let seen = fake.requests();
    assert_eq!(decisions(&seen), [json!({ "reject": {} })]);
    wrote_once(&seen);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn two_calls_asking_about_one_approval_each_keep_their_dialog() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = Client::connect(server);
    let first = ask_as_claude_code(&mut client, &deploy.input).await;
    let second = ask_as_claude_code(&mut client, &deploy.input).await;
    assert_ne!(
        first["result"]["requestState"], second["result"]["requestState"],
        "{second}"
    );
    for asking in [&first, &second] {
        let done = answer_on(&mut client, "deploy", &deploy.input, asking, approve()).await;
        assert_eq!(
            done["result"]["structuredContent"],
            outcome(&deploy).0,
            "{done}"
        );
    }
    assert_eq!(decisions(&fake.requests()).len(), 2);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[test]
fn a_held_dialog_past_its_deadline_is_dropped_when_another_is_held() {
    let deploy = transcript("deploy", StoreAt::Cloud);
    let held = |until| {
        let (_, state) = tools::start("deploy", deploy.input.clone(), &deploy.place, "call");
        let read = deploy.turns[0]
            .answers
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        let (Step::Write { write }, state) = tools::resume(state, read) else {
            panic!("deploy writes after its read");
        };
        Held {
            tool: "deploy".to_owned(),
            input: deploy.input.clone(),
            state,
            write,
            asked: Asked::of(&serde_json::from_value(deploy_asks()["error"].clone()).unwrap())
                .unwrap(),
            until,
        }
    };
    let waiting = Waiting::default();
    let abandoned = waiting.hold(held(Instant::now()));
    let fresh = waiting.hold(held(Instant::now() + Duration::from_secs(60)));
    assert!(waiting.take(&abandoned, "deploy", &deploy.input).is_err());
    assert!(waiting.take(&fresh, "deploy", &deploy.input).is_ok());
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_declined_dialog_denies_the_approval_in_cloud() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = Client::connect(server);
    let asking = ask_as_claude_code(&mut client, &deploy.input).await;
    let denied = answer_on(
        &mut client,
        "deploy",
        &deploy.input,
        &asking,
        json!({ "action": "decline" }),
    )
    .await;
    assert_eq!(denied["result"]["isError"], true, "{denied}");
    assert_eq!(
        denied["result"]["content"][0]["text"],
        "The human denied this deploy. Nothing was deployed; do not retry it unless they ask."
    );
    let seen = fake.requests();
    assert_eq!(decisions(&seen), [json!({ "reject": {} })]);
    wrote_once(&seen);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_closed_form_denies_the_approval_in_cloud() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = eliciting(server).await;
    client
        .send(&request(
            2,
            "tools/call",
            json!({ "name": "deploy", "arguments": deploy.input }),
        ))
        .await;
    let asking = client.reply().await;
    let closed = reply_to(&mut client, &asking, json!({ "action": "cancel" })).await;
    assert_eq!(closed["result"]["isError"], true, "{closed}");
    assert_eq!(
        closed["result"]["content"][0]["text"],
        "The human denied this deploy. Nothing was deployed; do not retry it unless they ask."
    );
    assert_eq!(decisions(&fake.requests()), [json!({ "reject": {} })]);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_host_without_elicitation_gets_a_tool_error_naming_where_to_approve() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = Client::initialized(server, json!({})).await;
    let refused = client.call(2, "deploy", deploy.input.clone()).await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let text = refused["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text,
        format!(
            "A human must approve this deploy to production first, and this agent cannot ask \
             them. Show them what it destroys, then ask them to approve this deploy to \
             production in the Ployz Cloud sidebar. Once they approve, call this tool again \
             with `approval` set to `apr_1`.\n{REVIEW}"
        )
    );
    let (for_the_human, argument) = text.split_once("Once they approve").unwrap();
    assert!(!for_the_human.contains("apr_1"), "{for_the_human}");
    assert!(argument.contains("`approval` set to `apr_1`"), "{argument}");
    let mut approved = deploy.input.clone();
    approved["approval"] = json!("apr_1");
    let done = client.call(3, "deploy", approved).await;
    assert_eq!(
        done["result"]["structuredContent"],
        outcome(&deploy).0,
        "{done}"
    );
    let seen = fake.requests();
    resent(&seen);
    assert!(decisions(&seen).is_empty(), "{seen:?}");
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_denial_after_someone_else_approved_says_so_and_claims_nothing() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let mut client = eliciting(server).await;
    let mut answers = Vec::new();
    for (id, approve) in [(2, true), (3, false)] {
        client
            .send(&request(
                id,
                "tools/call",
                json!({ "name": "deploy", "arguments": deploy.input }),
            ))
            .await;
        let asking = client.reply().await;
        answers.push(
            reply_to(
                &mut client,
                &asking,
                json!({ "action": "accept", "content": { "approve": approve } }),
            )
            .await,
        );
    }
    let refused = &answers[1];
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(
        refused["result"]["content"][0]["text"],
        "This approval was already decided elsewhere (approved). This answer was not \
         recorded, and nothing was deployed by this call."
    );
    let seen = fake.requests();
    assert_eq!(
        decisions(&seen),
        [
            json!({ "approve": { "digest": "3:abc" } }),
            json!({ "reject": {} })
        ]
    );
    let approved: Vec<_> = writes(&seen)
        .into_iter()
        .filter(|write| write.approval.is_some())
        .collect();
    assert_eq!(approved.len(), 1, "{seen:?}");
}
