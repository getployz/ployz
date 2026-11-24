use std::io::{BufRead, Read, Write as _};
use std::sync::mpsc::{Receiver, channel};

use ployz_agent_tools::{Answer as Answered, RequestKey};
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
        "../../../../ployz-agent-tools/tests/snapshots/agent_transcripts.json"
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
    key: Option<String>,
    bearer: String,
    via: Option<String>,
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

/// One request read off `stream`, the connection left open for the reply.
fn incoming(stream: &std::net::TcpStream) -> Seen {
    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
    let mut request = String::new();
    reader.read_line(&mut request).unwrap();
    let (mut length, mut bearer, mut approval, mut key, mut via) =
        (0, String::new(), None, None, None);
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match name.to_ascii_lowercase().as_str() {
            "content-length" => length = value.parse().unwrap(),
            "authorization" => value
                .strip_prefix("Bearer ")
                .unwrap_or(value)
                .clone_into(&mut bearer),
            "x-ployz-approval" => approval = Some(value.to_owned()),
            "x-ployz-request-key" => key = Some(value.to_owned()),
            "x-ployz-via" => via = Some(value.to_owned()),
            _ => {}
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Seen {
        path: request
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned(),
        approval,
        key,
        bearer,
        via,
        body: match body.as_slice() {
            [] => Value::Null,
            body => serde_json::from_slice(body).unwrap(),
        },
    }
}

/// A Ployz Cloud that answers `script`. With `asking`, a write without an approval gets
/// that `approval_required` refusal.
fn fake_cloud(mut script: Vec<Scripted>, asking: Option<Value>) -> Fake {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (saw, seen) = channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let Seen {
                path,
                approval,
                key,
                bearer,
                via,
                body,
            } = incoming(&stream);
            let (status, reply) = if let (Some(asking), true, None) =
                (&asking, is_write(&path), &approval)
            {
                ("409 Conflict", asking.to_string())
            } else if let Some(at) = script
                .iter()
                .position(|each| each.path == path && each.body == body)
            {
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
                key,
                bearer,
                via,
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
fn writes(seen: &[Seen]) -> Vec<&Seen> {
    seen.iter().filter(|seen| is_write(&seen.path)).collect()
}

fn fixed(place: Place) -> Locate {
    Box::new(move |store| {
        Ok(Place {
            store,
            ..place.clone()
        })
    })
}

fn on_cloud(fake: &Fake, place: Place) -> Server {
    Server {
        keys: |_, _, tool| format!("call-{tool}"),
        ..Server::new(
            Account {
                config: PathBuf::from("/nonexistent/config.toml"),
                token: Some("ployz_acme".to_owned()),
                cloud: Some(fake.url.clone()),
                organization: Acting::known(&fake.url, "ployz_acme", org()),
            },
            None,
            fixed(place),
        )
    }
}

fn org() -> OrganizationId {
    OrganizationId::parse("org").unwrap()
}

fn in_org() -> StoreAt {
    StoreAt::Cloud {
        organization: org(),
    }
}

fn who() -> Actor {
    Actor::system(org())
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
                region: None,
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
            Some(Local {
                store: Arc::new(shop()),
                who: who(),
            }),
            fixed(place),
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

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
fn request(id: u64, method: &str, mut params: Value) -> Value {
    // Every call says why it was made; these tests are about what happens next.
    if method == "tools/call" && params["arguments"]["intent"].is_null() {
        params["arguments"]["intent"] = json!("test");
    }
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
#[ignore = "agent replay, run by hand: see core/docs/RELEASE.md"]
async fn every_cloud_transcript_replays_through_ployz_mcp() {
    for transcript in transcripts()
        .iter()
        .filter(|each| each.place.store == in_org())
    {
        let fake = fake_cloud(script(transcript), None);
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
#[ignore = "agent replay, run by hand: see core/docs/RELEASE.md"]
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
async fn an_organization_token_mints_in_its_organization_asked_once() {
    let transcript = transcript("project_create", in_org());
    let organizations = Scripted {
        path: "/api/cli/organizations".to_owned(),
        body: Value::Null,
        answer: Answered::Value(json!({ "organizations": [
            { "id": "org", "slug": "acme", "name": "Acme", "current": true },
        ] })),
    };
    let mut cloud = vec![organizations];
    cloud.extend(script(&transcript));
    cloud.extend(script(&transcript));
    let fake = fake_cloud(cloud, None);
    let mut server = on_cloud(&fake, transcript.place.clone());
    server.account.organization = Acting::default();
    let mut client = Client::initialized(server, json!({})).await;
    let (result, _) = outcome(&transcript);
    for id in [2, 3] {
        let done = client
            .call(id, "project_create", transcript.input.clone())
            .await;
        assert_eq!(done["result"]["structuredContent"], result, "{done}");
    }
    let asked = fake
        .requests()
        .into_iter()
        .filter(|seen| seen.path == "/api/cli/organizations");
    assert_eq!(asked.count(), 1, "the token's Organization is asked once");
}

fn acts_in(id: &str, slug: &str) -> Scripted {
    Scripted {
        path: "/api/cli/organizations".to_owned(),
        body: Value::Null,
        answer: Answered::Value(json!({ "organizations": [
            { "id": id, "slug": slug, "name": slug, "current": true },
        ] })),
    }
}

fn store_agent_token(home: &tempfile::TempDir, fake: &Fake, token: &str, organization: &str) {
    let mcp = json!({
        "cloud": fake.url,
        "id": format!("tok_{organization}"),
        "organization": organization,
        "token": token,
    });
    std::fs::write(home.path().join("mcp.json"), mcp.to_string()).unwrap();
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_token_swapped_by_mcp_login_acts_in_its_own_organization_on_the_next_call() {
    let transcript = transcript("project_create", in_org());
    let mut cloud = vec![acts_in("org_acme", "acme"), acts_in("org_globex", "globex")];
    cloud.extend(script(&transcript));
    let fake = fake_cloud(cloud, None);
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    let here = std::env::current_dir().unwrap().canonicalize().unwrap();
    let links = json!({ here.to_str().unwrap(): {
        "organization": { "id": "org_acme", "slug": "acme" },
        "project": "shop",
        "environment": "production",
    } });
    std::fs::write(home.path().join("links.json"), links.to_string()).unwrap();
    store_agent_token(&home, &fake, "ployz_acme_agent", "acme");
    let server = Server {
        account: Account {
            config: config.clone(),
            token: None,
            cloud: None,
            organization: Acting::default(),
        },
        place: located(config),
        ..on_cloud(&fake, transcript.place.clone())
    };
    let mut client = Client::initialized(server, json!({})).await;
    let first = client
        .call(2, "project_create", transcript.input.clone())
        .await;
    assert!(
        !first.to_string().contains("is linked in Organization"),
        "the link is in the token's Organization: {first}"
    );
    store_agent_token(&home, &fake, "ployz_globex_agent", "globex");
    let second = client
        .call(3, "project_create", transcript.input.clone())
        .await;
    assert_eq!(second["result"]["isError"], true, "{second}");
    assert!(
        second
            .to_string()
            .contains("is linked in Organization acme, but `ployz mcp` acts in another"),
        "{second}"
    );
    let seen = fake.requests();
    let asked: Vec<_> = seen
        .iter()
        .filter(|seen| seen.path == "/api/cli/organizations")
        .map(|seen| seen.bearer.as_str())
        .collect();
    assert_eq!(
        asked,
        ["ployz_acme_agent", "ployz_globex_agent"],
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .filter(|seen| seen.bearer == "ployz_globex_agent")
            .all(|seen| seen.path == "/api/cli/organizations"),
        "nothing reaches the Store with the new token: {seen:?}"
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_cloud_tool_on_a_local_store_names_the_fix() {
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
async fn a_local_backend_cannot_answer_a_cloud_ask() {
    let store = shop();
    let who = who();
    let backend = Backend::Local(&store, &who);
    let servers = serde_json::from_value(json!({ "query": "servers", "only": null })).unwrap();
    let answered = backend.ask(&Ask::Cloud(servers)).await.unwrap_err();
    assert_eq!(answered.code.as_str(), "internal");
    assert_eq!(
        answered.message,
        "This needs Ployz Cloud; a local Config Store cannot answer it"
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn tools_list_serves_the_catalog_with_its_hints_and_schemas() {
    let place = transcript("deploy", in_org()).place;
    let mut client = Client::initialized(on_local(place), json!({})).await;
    client.send(&request(2, "tools/list", json!({}))).await;
    let listed = client.reply().await;
    let tools = listed["result"]["tools"].as_array().unwrap();
    let catalog = tools::catalog();
    // The catalog, plus `send_feedback`.
    assert_eq!(tools.len(), catalog.len() + 1, "{listed}");
    for def in &catalog {
        let tool = tools
            .iter()
            .find(|tool| tool["name"] == def.name)
            .unwrap_or_else(|| panic!("{} is listed", def.name));
        assert_eq!(tool["title"], def.title, "{tool}");
        assert_eq!(tool["annotations"]["readOnlyHint"], def.read_only, "{tool}");
        assert_eq!(
            tool["annotations"]["destructiveHint"], def.destructive,
            "{tool}"
        );
        assert_eq!(
            tool["annotations"]["idempotentHint"], def.idempotent,
            "{tool}"
        );
        assert_eq!(
            tool["annotations"]["openWorldHint"], def.open_world,
            "{tool}"
        );
        if def.read_only {
            assert_eq!(tool["annotations"]["idempotentHint"], true, "{tool}");
        }
        assert_eq!(tool["outputSchema"], def.output_schema, "{tool}");
        assert_eq!(
            tool["inputSchema"]["properties"]["approval"],
            Value::Null,
            "an agent never carries a yes: {tool}"
        );
    }
    let hints = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .map(|tool| tool["annotations"].clone())
            .unwrap_or_else(|| panic!("{name} is listed"))
    };
    assert_eq!(hints("volume_restore")["destructiveHint"], true);
    assert_eq!(hints("service_remove")["destructiveHint"], false);
    assert_eq!(hints("service_stop")["destructiveHint"], false);
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn the_instructions_are_the_registry_s_and_the_server_names_itself() {
    let place = transcript("deploy", in_org()).place;
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
    let server = &initialized["result"]["serverInfo"];
    assert_eq!(server["name"], "ployz", "{server}");
    assert_eq!(server["version"], env!("CARGO_PKG_VERSION"), "{server}");
    assert_eq!(server["title"], tools::TITLE, "{server}");
    assert_eq!(server["description"], tools::DESCRIPTION, "{server}");
    assert_eq!(server["websiteUrl"], "https://ployz.dev", "{server}");
    assert_eq!(
        server["icons"],
        json!([{
            "src": "https://ployz.dev/assets/logo-mark.svg",
            "mimeType": "image/svg+xml",
            "sizes": ["any"],
        }]),
        "{server}"
    );
}

/// Claude Code cuts a server's instructions past 2 KB. Only tests shorten a server's
/// deadline, so the one `Server::new` sets is the longest the sentence can name.
#[test]
fn the_instructions_fit_in_what_claude_code_keeps() {
    let sent = instructions(CALL_DEADLINE);
    assert!(
        sent.len() <= 2048,
        "the instructions are {} B: {sent}",
        sent.len()
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_line_that_is_not_json_gets_a_parse_error_and_the_server_keeps_serving() {
    let place = transcript("deploy", in_org()).place;
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
    let place = transcript("deploy", in_org()).place;
    let mut client = Client::initialized(on_local(place), json!({})).await;
    let unknown = client.call(2, "kubectl", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");
    assert_eq!(unknown["error"]["message"], "no tool named kubectl");
}

#[tokio::test]
async fn a_replayed_request_without_a_key_writes_under_the_same_id() {
    let transcript = transcript("service_add", in_org());
    let fake = fake_cloud((0..4).flat_map(|_| script(&transcript)).collect(), None);
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
                "holds_staged": false,
                "changes": [],
                "total_count": 3,
            },
        },
    }})
}

struct Approving {
    fake: Fake,
    server: Server,
    deploy: Transcript,
}

/// A deploy Cloud asks about, as it asks a person: `approval_required` with its digest.
fn approving() -> Approving {
    let deploy = transcript("deploy", in_org());
    let fake = fake_cloud(script(&deploy), Some(deploy_asks()));
    let server = on_cloud(&fake, deploy.place.clone());
    Approving {
        fake,
        server,
        deploy,
    }
}

/// The deploy wrote once, without an approval.
fn wrote_once(seen: &[Seen]) {
    let [write] = writes(seen)[..] else {
        panic!("expected one write: {seen:?}");
    };
    assert_eq!(write.approval, None, "{seen:?}");
}

/// Cloud's ask reaches the agent as the call's refusal. `ployz mcp` never answers it for a
/// person: the write goes once, with no yes and the agent's own token, and no host prompt
/// opens even when the host could show one.
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn an_ask_from_cloud_is_the_agent_s_refusal_and_is_never_answered() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let home = signed_in_home(&fake, true);
    let mut client =
        Client::initialized(without_token(server, &home), json!({ "elicitation": {} })).await;
    let done = at_once(client.call(2, "deploy", deploy.input.clone())).await;
    assert_eq!(done["id"], 2, "{done}");
    assert_eq!(done["result"]["isError"], true, "{done}");
    assert_eq!(
        done["result"]["structuredContent"]["code"], "approval_required",
        "{done}"
    );
    let seen = fake.requests();
    wrote_once(&seen);
    assert!(
        seen.iter()
            .all(|each| each.bearer == "ployz_agent" && each.via.as_deref() == Some("agent")),
        "every call carries the agent token: {seen:?}"
    );
}

fn signed_in_home(fake: &Fake, agent: bool) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let cloud = json!({
        "state": "signed_in",
        "cloud": fake.url,
        "token": "ployz_device",
        "account": { "id": "usr_ada", "email": "ada@example.com", "name": "Ada" },
        "organization": { "id": "org", "slug": "acme" },
    });
    std::fs::write(home.path().join("cloud.json"), cloud.to_string()).unwrap();
    if agent {
        let mcp = json!({
            "cloud": fake.url,
            "id": "tok_agent",
            "organization": "acme",
            "token": "ployz_agent",
        });
        std::fs::write(home.path().join("mcp.json"), mcp.to_string()).unwrap();
    }
    home
}

/// `server` acts with the agent token `signed_in_home` stored in `home`.
fn without_token(server: Server, home: &tempfile::TempDir) -> Server {
    let cloud = server.account.cloud.clone().unwrap_or_default();
    Server {
        account: Account {
            config: home.path().join("config.toml"),
            token: None,
            cloud: None,
            organization: Acting::known(&cloud, "ployz_agent", org()),
        },
        ..server
    }
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn ployz_mcp_without_its_own_token_refuses_and_names_ployz_mcp_login() {
    let Approving {
        fake,
        server,
        deploy,
    } = approving();
    let home = signed_in_home(&fake, false);
    let mut client = Client::initialized(without_token(server, &home), json!({})).await;
    let done = client.call(2, "deploy", deploy.input.clone()).await;
    assert_eq!(done["result"]["isError"], true, "{done}");
    assert!(
        done.to_string().contains("ployz mcp login"),
        "names the fix: {done}"
    );
    assert!(
        fake.requests().is_empty(),
        "the human's sign-in is never the agent's"
    );
}

/// A Ployz Cloud that refuses every bearer, as for a revoked token.
fn refusing_cloud() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            incoming(&stream);
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\n\
                 content-length: 2\r\nconnection: close\r\n\r\n{{}}"
            )
            .unwrap();
        }
    });
    url
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_refused_token_names_where_it_came_from_and_its_fix() {
    let url = refusing_cloud();
    let transcript = transcript("project_create", in_org());
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    let mcp =
        json!({ "cloud": url, "id": "tok_agent", "organization": "acme", "token": "ployz_agent" });
    std::fs::write(home.path().join("mcp.json"), mcp.to_string()).unwrap();
    let refusal = async |token: Option<&str>, cloud: Option<&str>| {
        let account = Account {
            config: config.clone(),
            token: token.map(str::to_owned),
            cloud: cloud.map(str::to_owned),
            organization: Acting::default(),
        };
        let server = Server::new(account, None, fixed(transcript.place.clone()));
        let mut client = Client::initialized(server, json!({})).await;
        let done = client
            .call(2, "project_create", transcript.input.clone())
            .await;
        assert_eq!(done["result"]["isError"], true, "{done}");
        done.to_string()
    };

    let agent = refusal(None, None).await;
    assert!(
        agent.contains("Cloud refused the agent token `ployz mcp` acts with")
            && agent.contains("Run `ployz mcp login`"),
        "names the agent token and its fix: {agent}"
    );
    assert!(
        !agent.contains("PLOYZ_TOKEN"),
        "PLOYZ_TOKEN isn't set: {agent}"
    );

    let env = refusal(Some("ployz_env"), Some(&url)).await;
    assert!(env.contains("Cloud refused PLOYZ_TOKEN"), "{env}");
    assert!(
        !env.contains("ployz mcp login"),
        "the agent token isn't in use: {env}"
    );
}

#[test]
fn published_commands_come_from_the_cloud_the_agent_acts_in() {
    let home = tempfile::tempdir().unwrap();
    let cloud = json!({
        "state": "signed_in",
        "cloud": "https://device.example",
        "token": "ployz_device",
        "account": { "id": "usr_ada", "email": "ada@example.com", "name": "Ada" },
        "organization": { "id": "org", "slug": "acme" },
    });
    std::fs::write(home.path().join("cloud.json"), cloud.to_string()).unwrap();
    let mcp = json!({
        "cloud": "https://agent.example",
        "id": "tok_agent",
        "organization": "acme",
        "token": "ployz_agent",
    });
    std::fs::write(home.path().join("mcp.json"), mcp.to_string()).unwrap();
    let config = home.path().join("config.toml");
    assert_eq!(
        publishing_setup(&config, None, None).cloud,
        "https://agent.example"
    );
    assert_eq!(
        publishing_setup(&config, None, Some("ployz_token")).cloud,
        "https://device.example",
        "PLOYZ_TOKEN acts in the Cloud the environment names"
    );
}

#[tokio::test]
async fn an_unreadable_login_file_is_named_to_the_agent_and_ignored_when_publishing() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    std::fs::write(home.path().join("mcp.json"), "{ not json").unwrap();
    let account = Account {
        config: config.clone(),
        ..Account::default()
    };
    let server = Server::new(account, None, fixed(transcript("deploy", in_org()).place));
    let Err(named) = server.agent().await else {
        panic!("a corrupt mcp.json is no credential");
    };
    assert_eq!(
        named,
        format!(
            "The sign-in stored at {}/mcp.json is unreadable.",
            home.path().display()
        )
    );
    assert_eq!(
        publishing_setup(&config, None, None).cloud,
        Setup::beside(&config, None).cloud
    );
}
/// A host that waits on a call would hang the test for the 30-minute call deadline.
async fn at_once<T>(reply: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), reply)
        .await
        .expect("the call returns without waiting for a human")
}

/// How Cloud loses a reply after taking the request.
#[derive(Clone, Copy, Debug)]
enum Lost {
    Dropped,
    Garbled,
    /// Held open with no reply, as a slow Cloud does.
    Unanswered,
}

/// A Ployz Cloud that answers a request on a path in `answers` and loses the reply to
/// any other, as `lost` says.
fn lossy_cloud(answers: HashMap<&'static str, Value>, lost: Lost) -> Fake {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (saw, seen) = channel();
    std::thread::spawn(move || {
        let mut unanswered = Vec::new();
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = incoming(&stream);
            let reply = match (answers.get(request.path.as_str()), lost) {
                (Some(answer), _) => Some(answer.to_string()),
                (None, Lost::Garbled) => Some("<html>bad gateway</html>".to_owned()),
                (None, Lost::Dropped) => None,
                (None, Lost::Unanswered) => {
                    unanswered.push(stream.try_clone().unwrap());
                    None
                }
            };
            let _ = saw.send(request);
            if let Some(reply) = reply {
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        }
    });
    Fake { url, seen }
}

/// `tool` called once as request 2 of session `s` against `cloud`, with no `request_key`.
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn refusal_on(cloud: &Fake, place: Place, tool: &str, input: Value) -> Value {
    let server = Server {
        session: "s".to_owned(),
        keys: call_key,
        ..on_cloud(cloud, place)
    };
    let mut client = Client::initialized(server, json!({})).await;
    let done = client.call(2, tool, input).await;
    assert_eq!(done["result"]["isError"], true, "{done}");
    done["result"]["structuredContent"].clone()
}

fn servers_answer() -> Value {
    json!({
        "servers": [{
            "machine": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "name": "web-2", "state": "online", "release": null, "upgrade": null,
            "draining": false, "disk": null, "namespaces": [],
        }],
        "seen": {
            "seen_by": null, "observed_at": null, "read_at": 0, "age_seconds": null,
            "from": "server", "unanswered": [],
        },
    })
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_lost_reply_to_a_write_says_it_may_have_landed_and_retries_under_the_same_key() {
    let deploy = transcript("deploy", in_org());
    let Some(Answered::Value(services)) = deploy.turns[0].answers.first().cloned() else {
        panic!("deploy reads its Services first");
    };
    let key = RequestKey::effective(None, "s:2");
    for lost in [Lost::Dropped, Lost::Garbled] {
        let cases = [
            (
                HashMap::from([("/api/cli/agent/ask", servers_answer())]),
                deploy.place.clone(),
                "server_drain",
                json!({ "server": "web-2" }),
                "/api/cli/agent/act",
            ),
            (
                HashMap::from([("/api/config/read", services.clone())]),
                deploy.place.clone(),
                "deploy",
                json!({ "services": ["web"], "message": "Raise web's log level" }),
                "/api/config/write",
            ),
        ];
        for (answers, place, tool, input, write) in cases {
            let cloud = lossy_cloud(answers, lost);
            let refusal = refusal_on(&cloud, place, tool, input).await;
            assert!(
                cloud.requests().iter().any(|seen| seen.path == write),
                "{tool} {lost:?}: Cloud took the write"
            );
            assert_eq!(refusal["writes"], "unknown", "{tool} {lost:?}: {refusal}");
            assert_eq!(refusal["retry"]["tool"], tool, "{refusal}");
            assert_eq!(
                refusal["retry"]["input"]["request_key"],
                key.as_str(),
                "{tool} {lost:?}: {refusal}"
            );
        }
    }
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_write_unanswered_at_the_deadline_may_have_landed_and_retries_under_the_same_key() {
    let place = transcript("deploy", in_org()).place;
    let answers = HashMap::from([("/api/cli/agent/ask", servers_answer())]);
    let cloud = lossy_cloud(answers, Lost::Unanswered);
    let mut server = Server {
        session: "s".to_owned(),
        keys: call_key,
        ..on_cloud(&cloud, place)
    };
    server.deadline = Duration::from_secs(1);
    let mut client = Client::initialized(server, json!({})).await;
    let done = client
        .call(2, "server_drain", json!({ "server": "web-2" }))
        .await;
    assert!(
        cloud
            .requests()
            .iter()
            .any(|seen| seen.path == "/api/cli/agent/act"),
        "Cloud took the write"
    );
    assert_eq!(done["result"]["isError"], true, "{done}");
    let refusal = &done["result"]["structuredContent"];
    assert_eq!(refusal["writes"], "unknown", "{done}");
    assert_eq!(refusal["retry"]["tool"], "server_drain", "{done}");
    assert_eq!(
        refusal["retry"]["input"]["request_key"],
        RequestKey::effective(None, "s:2").as_str(),
        "{done}"
    );
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn a_lost_reply_to_a_read_wrote_nothing() {
    let place = transcript("deploy", in_org()).place;
    let cloud = lossy_cloud(HashMap::new(), Lost::Dropped);
    let refusal = refusal_on(&cloud, place, "server_drain", json!({ "server": "web-2" })).await;
    assert_eq!(refusal["writes"], "none", "{refusal}");
    assert!(
        refusal["retry"]["input"].get("request_key").is_none(),
        "{refusal}"
    );
}

/// [`shop`] whose `production`, deployed with `web`, was removed from the Servers but
/// not yet from the Store, which only a sweep does.
#[expect(clippy::indexing_slicing, reason = "Fixed test fixtures use indexing")]
fn removed_but_unswept() -> ConfigStore {
    use ployz_core::{DeployOutcome, DeployPreview, ExecutionError, ServiceName};
    use ployz_store::{
        Admit, CreateEnvironment, CreateService, Deploy, DeploymentId, EnvironmentName,
        EnvironmentRef, Removal, Remove, RunEvidence, RunnerId, ServiceLineageId,
        SetDefaultEnvironment, SetEnvironmentProtected, Trusted, VolumeObservation,
    };

    let store = shop();
    let who = who();
    let production = || EnvironmentRef {
        project: None,
        environment: Some(EnvironmentName::parse("production").unwrap()),
    };
    store
        .write(
            &who,
            &CreateService {
                id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                environment: EnvironmentRef::default(),
                name: ServiceName::parse("web").unwrap(),
                image: Some("nginx".into()),
                template: None,
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse("00000000-0000-4000-8000-000000000004").unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
                region: None,
            },
        )
        .unwrap();
    let run = |id: &DeploymentId| {
        let runner = RunnerId::parse("runner").unwrap();
        let claimed = store.claim(id, &runner).unwrap();
        let operation = json!({ "type": "remove_container", "machine_id": "a".repeat(32),
                                "container_id": "0".repeat(64) });
        let preview: DeployPreview = serde_json::from_value(json!({
            "namespace": claimed.intent.namespace,
            "operations": [{ "index": 0, "machine_id": "a".repeat(32), "service_name": "web",
                             "operation": operation, "status": { "type": "pending" } }],
            "warnings": [], "would_remove": [], "preserved_volumes": [],
        }))
        .unwrap();
        store
            .record(id, &runner, RunEvidence::Prepared(preview))
            .unwrap();
        let outcome: DeployOutcome<ExecutionError> =
            serde_json::from_value(json!({ "type": "success", "completed": [operation] })).unwrap();
        store
            .record(
                id,
                &runner,
                RunEvidence::Executed {
                    progress: Vec::new(),
                    outcome: Box::new(outcome),
                    removed: Vec::new(),
                },
            )
            .unwrap();
    };
    let deployed = DeploymentId::parse("00000000-0000-4000-8000-000000000101").unwrap();
    store
        .write(
            &who,
            &Admit::Deploy(Deploy {
                id: deployed.clone(),
                environment: production(),
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
                versions: Vec::new(),
                recreate: Vec::new(),
            }),
        )
        .unwrap();
    run(&deployed);
    store
        .write(
            &who,
            &SetDefaultEnvironment {
                environment: EnvironmentRef {
                    project: None,
                    environment: Some(EnvironmentName::parse("staging").unwrap()),
                },
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &SetEnvironmentProtected {
                environment: production(),
                protected: false,
                seen: None,
            },
        )
        .unwrap();
    let observed = Trusted {
        volumes: Some(VolumeObservation {
            sought: Vec::new(),
            held: Vec::new(),
            unanswered: Vec::new(),
            failure: None,
        }),
        ..Trusted::default()
    };
    let remove = |version| Remove {
        project: ProjectName::parse("shop").unwrap(),
        environment: Some(EnvironmentName::parse("production").unwrap()),
        version,
        accept_volume_loss: std::collections::BTreeMap::new(),
        every_copy: std::collections::BTreeMap::new(),
    };
    let removing = match store.write_trusted(&who, &remove(None), &observed) {
        Err(refused) if refused.code == RpcErrorCode::ConfirmationRequired => {
            let version = refused.details["version"].as_str().unwrap().to_owned();
            store.write_trusted(&who, &remove(Some(version)), &observed)
        }
        sent => sent,
    }
    .unwrap();
    let Removal::Removing { left, .. } = removing else {
        panic!("a deployed Environment leaves the Servers first");
    };
    run(&left[0].deployment.as_ref().unwrap().id);
    store
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
#[tokio::test]
async fn each_local_call_sweeps_the_store_first() {
    let store = removed_but_unswept();
    let listed = |store: &ConfigStore| {
        store
            .read(&who(), &ployz_store::EnvironmentsQuery::default())
            .unwrap()
            .environments
            .into_iter()
            .map(|listing| listing.name.to_string())
            .collect::<Vec<_>>()
    };
    assert!(
        listed(&store).contains(&"production".to_owned()),
        "only a sweep deletes it"
    );
    let place = transcript("environment_show", StoreAt::Local).place;
    let server = Server {
        local: Some(Local {
            store: Arc::new(store),
            who: who(),
        }),
        ..on_local(place)
    };
    let mut client = Client::initialized(server, json!({})).await;
    let done = client
        .call(
            2,
            "environment_show",
            json!({ "environment": "production" }),
        )
        .await;
    assert_eq!(done["result"]["isError"], true, "{done}");
    assert_eq!(
        done["result"]["structuredContent"]["code"], "not_found",
        "{done}"
    );
}

// #3473: a Cloud command's refusal never repeats a secret's text it was sent.
#[test]
fn a_cloud_command_s_refusal_never_repeats_a_secret_s_text() {
    let error = RpcError::invalid_argument("This input does not fit".to_owned())
        .with_details(json!({ "field": "role", "writes": "none" }));
    let retry = tools::Call::new(
        "org_member_role",
        json!({ "role": 7, "credential": { "secret": "SENTINEL-3473-do-not-echo" } }),
    );
    let refused = json!(command_refusal(error, retry));
    assert!(!refused.to_string().contains("SENTINEL-3473"), "{refused}");
    assert!(refused.get("retry").is_none(), "{refused}");
    assert_eq!(refused.get("field"), Some(&json!("role")), "{refused}");
}

#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
mod published {
    use std::path::Path;

    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/published-manifest.json");

    /// A Ployz Cloud that publishes `manifest` and answers every run with `run`. While
    /// `down`, every request fails as a Cloud that cannot answer does. Each manifest
    /// takes `slow` to send.
    struct Publisher {
        manifest: Value,
        run: (&'static str, Value),
        down: bool,
        slow: Duration,
        /// How long a run takes to answer.
        lag: Duration,
        /// The revision replies name, when it is not the manifest's own.
        named: Option<&'static str>,
    }

    fn publisher() -> Arc<Mutex<Publisher>> {
        Arc::new(Mutex::new(Publisher {
            manifest: serde_json::from_str(FIXTURE).unwrap(),
            run: ("200 OK", json!({ "output": {} })),
            down: false,
            slow: Duration::ZERO,
            lag: Duration::ZERO,
            named: None,
        }))
    }

    fn fake_publisher(state: Arc<Mutex<Publisher>>) -> Fake {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (saw, seen) = channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let seen = incoming(&stream);
                let (status, reply, revision, slow, lag) = {
                    let state = state.lock().unwrap();
                    let revision = state.named.map_or_else(
                        || state.manifest["revision"].as_str().unwrap().to_owned(),
                        str::to_owned,
                    );
                    let (status, reply) = if state.down {
                        ("503 Service Unavailable", "down".to_owned())
                    } else if seen.path == "/api/cli/manifest" {
                        ("200 OK", state.manifest.to_string())
                    } else if seen.path.starts_with("/api/cli/run/") {
                        (state.run.0, state.run.1.to_string())
                    } else if seen.path == "/api/cli/organizations" {
                        (
                            "200 OK",
                            json!({ "organizations": [
                                { "id": "org", "slug": "acme", "name": "acme", "current": true },
                            ] })
                            .to_string(),
                        )
                    } else {
                        (
                            "500 Internal Server Error",
                            json!({ "error": { "code": "internal", "message": format!("unscripted {}", seen.path) } })
                                .to_string(),
                        )
                    };
                    let slow = (seen.path == "/api/cli/manifest").then_some(state.slow);
                    let lag = seen.path.starts_with("/api/cli/run/").then_some(state.lag);
                    (status, reply, revision, slow, lag)
                };
                if let Some(lag) = lag {
                    // real-wait: holds the fake Cloud's run reply past the server's deadline on a real socket; that wait emits no event
                    std::thread::sleep(lag);
                }
                if let Some(slow) = slow {
                    // real-wait: holds the fake Cloud's manifest reply on a real socket so a second call reaches the in-process reread lock; that wait emits no event
                    std::thread::sleep(slow);
                }
                let _ = saw.send(seen);
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\n\
                     x-ployz-manifest: {revision}\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
        });
        Fake { url, seen }
    }

    async fn on_published(fake: &Fake, home: &Path) -> Server {
        let place = transcript("deploy", in_org()).place;
        let setup = Setup::beside(&home.join("config.yaml"), Some(fake.url.clone()));
        on_cloud(fake, place).publishing(setup).await
    }

    fn entries() -> Vec<Value> {
        serde_json::from_str::<Value>(FIXTURE).unwrap()["commands"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn agent_facing() -> Vec<Value> {
        entries()
            .into_iter()
            .filter(|entry| entry["agents"] == true)
            .collect()
    }

    fn name(entry: &Value) -> &str {
        entry["name"].as_str().unwrap()
    }

    /// One value of each input property, as its schema allows.
    fn arguments(entry: &Value) -> Value {
        let properties = entry["input"]["properties"].as_object().unwrap();
        properties
            .iter()
            .map(|(name, schema)| {
                let value = match schema["type"].as_str().unwrap() {
                    "string" => schema["enum"]
                        .as_array()
                        .and_then(|choices| choices.first().cloned())
                        .unwrap_or_else(|| json!("typed")),
                    "integer" => json!(3),
                    "boolean" => json!(true),
                    "array" => json!(["one", "two"]),
                    other => panic!("{other} is outside the flat subset"),
                };
                (name.clone(), value)
            })
            .collect()
    }

    async fn listed(client: &mut Client, id: u64) -> Vec<Value> {
        client.send(&request(id, "tools/list", json!({}))).await;
        client.reply().await["result"]["tools"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn named<'a>(tools: &'a [Value], name: &str) -> Option<&'a Value> {
        tools.iter().find(|tool| tool["name"] == name)
    }

    fn cached_names(home: &Path) -> Vec<String> {
        let dir = home.join("published");
        let file = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "json"))
            .expect("one cache file");
        let cached: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        cached["manifest"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| name(entry).to_owned())
            .collect()
    }

    #[tokio::test]
    async fn a_cloud_store_lists_each_agent_facing_published_command_with_its_schemas() {
        let fake = fake_publisher(publisher());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let tools = listed(&mut client, 2).await;
        let (entries, agent_facing) = (entries(), agent_facing());
        assert!(
            !agent_facing.is_empty() && agent_facing.len() < entries.len(),
            "the fixture has entries of both kinds"
        );
        assert_eq!(tools.len(), tools::catalog().len() + agent_facing.len() + 1);
        for entry in &entries {
            let Some(tool) = named(&tools, name(entry)) else {
                assert_eq!(entry["agents"], false, "{entry} is listed");
                continue;
            };
            assert_eq!(entry["agents"], true, "{tool} is not for agents");
            let about = entry["about"].as_str().unwrap_or_default();
            let said = if entry["gate"] == "ask" {
                format!(
                    "{}. {}",
                    about.trim_end_matches('.'),
                    Gate::Cloud.sentence()
                )
            } else {
                about.to_owned()
            };
            assert_eq!(tool["description"], said.as_str());
            let mut input = entry["input"].clone();
            // Every tool takes `intent`, required.
            input["properties"]["intent"] = tool["inputSchema"]["properties"]["intent"].clone();
            assert_eq!(input["properties"]["intent"]["type"], "string", "{tool}");
            let mut required = input["required"].as_array().cloned().unwrap_or_default();
            required.push(json!("intent"));
            input["required"] = json!(required);
            assert_eq!(tool["inputSchema"], input, "{tool}");
            assert_eq!(tool["outputSchema"], entry["output"], "{tool}");
            assert_eq!(
                tool["annotations"]["readOnlyHint"],
                entry["kind"] == "read",
                "{tool}"
            );
            assert_eq!(
                tool["annotations"]["destructiveHint"],
                entry["removes"] == true,
                "{tool}"
            );
            // A Cloud that predates a title and these hints leaves them out.
            assert_eq!(tool.get("title"), entry.get("title"), "{tool}");
            let idempotent = entry
                .get("idempotent")
                .cloned()
                .unwrap_or_else(|| json!(entry["kind"] == "read"));
            assert_eq!(tool["annotations"]["idempotentHint"], idempotent, "{tool}");
            let open_world = entry.get("open_world").cloned().unwrap_or(json!(false));
            assert_eq!(tool["annotations"]["openWorldHint"], open_world, "{tool}");
        }
        assert_eq!(cached_names(home.path()).len(), entries.len());
    }

    #[tokio::test]
    async fn a_published_call_posts_its_input_under_the_call_s_key_and_returns_the_output() {
        let publisher = publisher();
        publisher.lock().unwrap().run = ("200 OK", json!({ "output": { "answer": 42 } }));
        let fake = fake_publisher(publisher);
        let home = tempfile::tempdir().unwrap();
        let server = Server {
            keys: call_key,
            ..on_published(&fake, home.path()).await
        };
        let key =
            RequestKey::effective(None, &call_key(&server.session, &RequestId::Number(2), ""));
        let mut client = Client::initialized(server, json!({})).await;
        let entry = agent_facing().into_iter().next().unwrap();
        let arguments = arguments(&entry);
        let done = client.call(2, name(&entry), arguments.clone()).await;
        assert_eq!(
            done["result"]["structuredContent"],
            json!({ "answer": 42 }),
            "{done}"
        );
        assert_ne!(done["result"]["isError"], true, "{done}");
        let seen = fake.requests();
        let run = seen
            .iter()
            .find(|seen| seen.path.starts_with("/api/cli/run/"))
            .expect("one run");
        assert_eq!(run.path, format!("/api/cli/run/{}", name(&entry)));
        assert_eq!(run.body, json!({ "input": arguments }));
        assert_eq!(run.key.as_deref(), Some(key.as_str()));
        assert!(RequestKey::parse(key.as_str()).is_ok());
        assert_eq!(run.bearer, "ployz_acme");
        assert_eq!(run.approval, None);
        assert_eq!(run.via.as_deref(), Some("agent"));
    }

    #[tokio::test]
    async fn a_reply_naming_a_newer_revision_rereads_the_list_and_tells_the_host() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let mut agent_facing = agent_facing().into_iter();
        let (kept, dropped) = (agent_facing.next().unwrap(), agent_facing.next().unwrap());
        {
            let mut state = publisher.lock().unwrap();
            state.manifest["revision"] = json!("later");
            state.manifest["commands"]
                .as_array_mut()
                .unwrap()
                .retain(|entry| name(entry) != name(&dropped));
        }
        client
            .send(&request(
                2,
                "tools/call",
                json!({ "name": name(&kept), "arguments": arguments(&kept) }),
            ))
            .await;
        let changed = client.reply().await;
        assert_eq!(
            changed["method"], "notifications/tools/list_changed",
            "{changed}"
        );
        let done = client.reply().await;
        assert_eq!(done["id"], 2, "{done}");
        assert_ne!(done["result"]["isError"], true, "{done}");
        let tools = listed(&mut client, 3).await;
        assert!(named(&tools, name(&kept)).is_some());
        assert!(named(&tools, name(&dropped)).is_none(), "{tools:?}");
        assert!(!cached_names(home.path()).contains(&name(&dropped).to_owned()));
        let paths: Vec<String> = fake.requests().into_iter().map(|seen| seen.path).collect();
        assert_eq!(
            paths
                .iter()
                .filter(|path| *path == "/api/cli/manifest")
                .count(),
            2,
            "{paths:?}"
        );
    }

    fn manifest_reads(fake: &Fake) -> usize {
        fake.requests()
            .iter()
            .filter(|seen| seen.path == "/api/cli/manifest")
            .count()
    }

    #[tokio::test]
    async fn a_compiled_tool_whose_reply_names_a_newer_revision_rereads_the_list() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        publisher.lock().unwrap().manifest["revision"] = json!("compiled-later");
        let shown = transcript("environment_show", in_org());
        client
            .send(&request(
                2,
                "tools/call",
                json!({ "name": shown.tool, "arguments": shown.input }),
            ))
            .await;
        let changed = client.reply().await;
        assert_eq!(
            changed["method"], "notifications/tools/list_changed",
            "{changed}"
        );
        assert_eq!(client.reply().await["id"], 2);
        assert_eq!(manifest_reads(&fake), 2);
    }

    #[tokio::test]
    async fn a_revision_whose_reread_failed_is_not_read_again() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = agent_facing().into_iter().next().unwrap();
        {
            let mut state = publisher.lock().unwrap();
            state.manifest["revision"] = json!("unreadable");
            state.down = true;
        }
        for id in [2, 3] {
            let done = client.call(id, name(&entry), arguments(&entry)).await;
            assert_eq!(done["id"], id, "{done}");
        }
        assert_eq!(manifest_reads(&fake), 2);
    }

    #[tokio::test]
    async fn a_revision_the_reread_does_not_reach_is_not_reread_again() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = agent_facing().into_iter().next().unwrap();
        {
            let mut state = publisher.lock().unwrap();
            state.manifest["revision"] = json!("behind");
            state.named = Some("ahead");
        }
        let mut frames = Vec::new();
        for id in [2, 3] {
            client
                .send(&request(
                    id,
                    "tools/call",
                    json!({ "name": name(&entry), "arguments": arguments(&entry) }),
                ))
                .await;
            loop {
                let frame = client.reply().await;
                let answered = frame["id"] == id;
                frames.push(frame);
                if answered {
                    break;
                }
            }
        }
        assert_eq!(manifest_reads(&fake), 2, "{frames:?}");
    }

    #[tokio::test]
    async fn calls_that_see_a_new_revision_together_reread_it_once() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = agent_facing().into_iter().next().unwrap();
        {
            let mut state = publisher.lock().unwrap();
            state.manifest["revision"] = json!("together");
            state.slow = Duration::from_millis(300);
        }
        for id in [2, 3] {
            client
                .send(&request(
                    id,
                    "tools/call",
                    json!({ "name": name(&entry), "arguments": arguments(&entry) }),
                ))
                .await;
        }
        let mut frames = Vec::new();
        while frames
            .iter()
            .filter(|frame: &&Value| frame["id"].is_u64())
            .count()
            < 2
        {
            frames.push(client.reply().await);
        }
        let notices = frames
            .iter()
            .filter(|frame| frame["method"] == "notifications/tools/list_changed")
            .count();
        assert_eq!(notices, 1, "{frames:?}");
        assert_eq!(manifest_reads(&fake), 2);
    }

    #[tokio::test]
    async fn the_host_is_told_the_tool_list_can_change() {
        let fake = fake_publisher(publisher());
        let home = tempfile::tempdir().unwrap();
        let mut client = Client::connect(on_published(&fake, home.path()).await);
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
        let ready = client.reply().await;
        assert_eq!(
            ready["result"]["capabilities"]["tools"]["listChanged"], true,
            "{ready}"
        );
    }

    #[tokio::test]
    async fn a_reply_naming_the_held_revision_costs_no_reread_and_no_notice() {
        let fake = fake_publisher(publisher());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = agent_facing().into_iter().next().unwrap();
        let done = client.call(2, name(&entry), arguments(&entry)).await;
        assert_eq!(done["id"], 2, "{done}");
        let paths: Vec<String> = fake.requests().into_iter().map(|seen| seen.path).collect();
        assert_eq!(
            paths
                .iter()
                .filter(|path| *path == "/api/cli/manifest")
                .count(),
            1,
            "{paths:?}"
        );
    }

    #[tokio::test]
    async fn unknown_command_drops_the_tool_from_the_list_and_the_cache() {
        let publisher = publisher();
        publisher.lock().unwrap().run = (
            "404 Not Found",
            json!({ "error": { "code": "unknown_command", "message": "Ployz Cloud no longer has it" } }),
        );
        let fake = fake_publisher(publisher);
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = agent_facing().into_iter().next().unwrap();
        client
            .send(&request(
                2,
                "tools/call",
                json!({ "name": name(&entry), "arguments": arguments(&entry) }),
            ))
            .await;
        let changed = client.reply().await;
        assert_eq!(
            changed["method"], "notifications/tools/list_changed",
            "{changed}"
        );
        let done = client.reply().await;
        assert_eq!(done["result"]["isError"], true, "{done}");
        assert_eq!(
            done["result"]["structuredContent"]["code"], "unknown_command",
            "{done}"
        );
        let tools = listed(&mut client, 3).await;
        assert!(named(&tools, name(&entry)).is_none(), "{tools:?}");
        assert!(!cached_names(home.path()).contains(&name(&entry).to_owned()));
        let gone = client.call(4, name(&entry), arguments(&entry)).await;
        assert_eq!(gone["error"]["code"], -32602, "{gone}");
    }

    /// A gated published command, called as an agent without an approval, and Cloud's
    /// refusal of it.
    async fn gated_refused_with(refusal: Value) -> (Fake, Value) {
        let publisher = publisher();
        publisher.lock().unwrap().run = ("409 Conflict", json!({ "error": refusal }));
        let fake = fake_publisher(publisher);
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = org_member_role();
        assert_eq!(entry["gate"], "ask", "{entry}");
        let refused = at_once(client.call(2, name(&entry), arguments(&entry))).await;
        (fake, refused)
    }

    /// The published path relays Cloud's ask the same way: one run, no yes.
    #[tokio::test]
    async fn a_published_ask_is_the_agent_s_refusal_and_is_never_answered() {
        let (fake, refused) = gated_refused_with(json!({
            "code": "approval_required",
            "message": "A human must approve this role change of ada before it runs.",
            "details": { "approval": "digest", "effects": [],
                "operation": { "verb": "role_change", "name": "ada" }, "organization": "acme" },
        }))
        .await;
        assert_eq!(refused["id"], 2, "{refused}");
        let result = &refused["result"];
        assert_eq!(result["isError"], true, "{refused}");
        assert_eq!(
            result["structuredContent"]["code"], "approval_required",
            "{refused}"
        );
        let seen = fake.requests();
        let runs: Vec<_> = seen
            .iter()
            .filter(|seen| seen.path.starts_with("/api/cli/run/"))
            .collect();
        let [run] = runs[..] else {
            panic!("one run: {seen:?}");
        };
        assert_eq!(run.approval, None, "{seen:?}");
    }

    fn org_member_role() -> Value {
        agent_facing()
            .into_iter()
            .find(|entry| name(entry) == "org_member_role")
            .unwrap()
    }

    #[tokio::test]
    async fn a_published_refusal_reaches_the_agent_as_a_tool_s_with_the_call_to_repeat() {
        let (_fake, refused) = gated_refused_with(json!({
            "code": "invalid_argument",
            "message": "Expected \"owner\" | \"admin\" | \"member\"",
            "details": null,
            "field": "role",
            "expected": { "type": "string", "enum": ["owner", "admin", "member"] },
            "writes": "none",
        }))
        .await;
        let entry = org_member_role();
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert_eq!(
            refused["result"]["structuredContent"],
            json!({
                "code": "invalid_argument",
                "message": "Expected \"owner\" | \"admin\" | \"member\"",
                "field": "role",
                "expected": { "type": "string", "enum": ["owner", "admin", "member"] },
                "retry": { "tool": "org_member_role", "input": arguments(&entry) },
                "writes": "none",
                "next": [],
            }),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn a_refused_call_s_retry_drops_keys_it_does_not_take() {
        let publisher = publisher();
        publisher.lock().unwrap().run = (
            "409 Conflict",
            json!({ "error": {
                "code": "conflict",
                "message": "An Organization needs an owner. Make someone else an owner first.",
                "details": null,
                "writes": "unknown",
            } }),
        );
        let fake = fake_publisher(publisher);
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = org_member_role();
        let mut sent = arguments(&entry);
        sent["ghp_s3cr3tTokenThatMustNotEcho"] = json!(true);
        let refused = at_once(client.call(2, name(&entry), sent)).await;
        assert_eq!(
            refused["result"]["structuredContent"]["retry"],
            json!({ "tool": "org_member_role", "input": arguments(&entry) }),
            "{refused}"
        );
        assert!(
            !refused
                .to_string()
                .contains("ghp_s3cr3tTokenThatMustNotEcho"),
            "{refused}"
        );
    }

    #[tokio::test]
    async fn a_refusal_from_a_cloud_that_predates_writes_says_writes_are_unknown() {
        let (_fake, refused) = gated_refused_with(json!({
            "code": "not_found",
            "message": "No member named ada.",
            "details": { "approval_id": "apr_7" },
        }))
        .await;
        let content = &refused["result"]["structuredContent"];
        assert_eq!(content["writes"], "unknown", "{refused}");
        assert_eq!(
            content["details"],
            json!({ "approval_id": "apr_7" }),
            "{refused}"
        );
        assert_eq!(content["retry"]["tool"], "org_member_role", "{refused}");
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_published_call_past_the_deadline_says_where_to_look() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        let mut server = on_published(&fake, home.path()).await;
        server.deadline = Duration::from_secs(1);
        let mut client = Client::initialized(server, json!({})).await;
        publisher.lock().unwrap().lag = Duration::from_secs(3);
        let entry = agent_facing()
            .into_iter()
            .find(|entry| name(entry) == "server_upgrade_policy_show")
            .unwrap();
        let done = client.call(2, name(&entry), arguments(&entry)).await;
        assert_eq!(done["result"]["isError"], true, "{done}");
        assert_eq!(
            done["result"]["content"][0]["text"],
            "ployz mcp stopped `server_upgrade_policy_show` after 1 seconds. What it started \
             may still be running; check with `server_show`.",
            "{done}"
        );
    }

    #[tokio::test]
    async fn a_published_command_that_is_not_for_agents_is_no_tool() {
        let fake = fake_publisher(publisher());
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let entry = entries()
            .into_iter()
            .find(|entry| entry["agents"] == false)
            .unwrap();
        let refused = client.call(2, name(&entry), arguments(&entry)).await;
        assert_eq!(refused["error"]["code"], -32602, "{refused}");
        assert_eq!(
            refused["error"]["message"],
            format!("no tool named {}", name(&entry))
        );
        assert!(
            fake.requests()
                .iter()
                .all(|seen| seen.path == "/api/cli/manifest"),
            "nothing ran"
        );
    }

    #[tokio::test]
    async fn a_typed_published_command_is_refused_before_cloud_is_asked() {
        let publisher = publisher();
        let typed = {
            let mut state = publisher.lock().unwrap();
            let entry = state.manifest["commands"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry["typed"] == "organization")
                .expect("the fixture carries a typed entry");
            entry["agents"] = json!(true);
            entry.clone()
        };
        let fake = fake_publisher(publisher);
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let refused = client.call(2, name(&typed), arguments(&typed)).await;
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        let text = refused["result"]["content"][0]["text"].as_str().unwrap();
        let path = typed["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|segment| segment.as_str().unwrap())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains(&format!("ployz {path}")), "{text}");
        assert!(
            fake.requests()
                .iter()
                .all(|seen| seen.path == "/api/cli/manifest"),
            "nothing ran"
        );
    }

    #[tokio::test]
    async fn a_cloud_that_cannot_answer_at_start_lists_the_cached_commands() {
        let publisher = publisher();
        let fake = fake_publisher(publisher.clone());
        let home = tempfile::tempdir().unwrap();
        drop(on_published(&fake, home.path()).await);
        assert_eq!(cached_names(home.path()).len(), entries().len());
        publisher.lock().unwrap().down = true;
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let tools = listed(&mut client, 2).await;
        assert_eq!(
            tools.len(),
            tools::catalog().len() + agent_facing().len() + 1
        );
    }

    #[tokio::test]
    async fn a_cloud_without_published_commands_lists_the_catalog_alone() {
        let fake = fake_cloud(Vec::new(), None);
        let home = tempfile::tempdir().unwrap();
        let mut client =
            Client::initialized(on_published(&fake, home.path()).await, json!({})).await;
        let tools = listed(&mut client, 2).await;
        assert_eq!(tools.len(), tools::catalog().len() + 1);
        assert!(!home.path().join("published").exists());
    }
}

mod tool_facts {
    use super::*;

    const SERVER: &[&str] = &[
        "server_show",
        "server_add",
        "server_logs",
        "server_metrics",
        "server_drain",
        "server_set",
        "server_upgrade",
        "server_clean",
        "server_remove",
        "server_forget",
    ];
    const REGION: &[&str] = &["region_list", "region_rename"];
    const VOLUME: &[&str] = &[
        "volume_add",
        "volume_update",
        "volume_remove",
        "volume_show",
        "volume_mirror",
        "volume_mirror_remove",
        "volume_move",
        "volume_release",
        "volume_restore",
    ];
    const SERVICE: &[&str] = &[
        "service_inspect",
        "service_add",
        "service_update",
        "service_rename",
        "service_remove",
        "service_status",
        "service_logs",
        "service_metrics",
        "service_restart",
        "service_start",
        "service_stop",
    ];
    const DEPLOYMENT: &[&str] = &[
        "docs",
        "environment_show",
        "project_create",
        "project_rename",
        "domain_check",
        "domain_add",
        "domain_update",
        "domain_remove",
        "config_write",
        "config_rename",
        "config_remove",
        "review",
        "discard",
        "history",
        "deploy",
        "deployment_list",
        "deployment_show",
        "deployment_retry",
        "deployment_start",
        "deployment_cancel",
        "environment_pr",
        "environment_protect",
        "environment_region",
        "environment_setup",
        "github_ls",
        "github_connect",
        "environment_remove",
        "project_remove",
    ];

    fn status_of(tool: &str) -> &'static str {
        let family = [
            (SERVER, "server_show"),
            (REGION, "region_list"),
            (VOLUME, "volume_show"),
            (SERVICE, "service_status"),
            (DEPLOYMENT, "deployment_show"),
        ];
        family
            .into_iter()
            .find_map(|(tools, status)| tools.contains(&tool).then_some(status))
            .unwrap_or_else(|| panic!("{tool} is in no family"))
    }

    #[test]
    fn the_table_names_every_tool_once() {
        let named = [SERVER, REGION, VOLUME, SERVICE, DEPLOYMENT].concat();
        let catalog = tools::catalog();
        let mut catalogued: Vec<&str> = catalog.iter().map(|def| def.name).collect();
        let mut named_sorted = named.clone();
        catalogued.sort_unstable();
        named_sorted.sort_unstable();
        assert_eq!(named_sorted, catalogued);
    }

    #[test]
    fn a_stopped_call_names_the_read_that_shows_what_it_started() {
        for def in tools::catalog() {
            assert_eq!(
                overdue(def.name, Duration::from_secs(90)),
                format!(
                    "ployz mcp stopped `{}` after 90 seconds. What it started may still be \
                     running; check with `{}`.",
                    def.name,
                    status_of(def.name),
                ),
                "{}",
                def.name
            );
        }
    }
}

#[test]
fn only_a_call_that_reached_for_cloud_may_have_landed() {
    for (name, error) in crate::cloud::account::testing::every_login_error(400) {
        let landed = ["Unreachable", "Unanswered", "Status", "Reply"].contains(&name);
        assert_eq!(may_have_landed(&error), landed, "{name}");
    }
    for status in [0, 200, 400, 404, 429, 499, 500, 503, 599] {
        let error = LoginError::Status {
            status,
            body: String::new(),
        };
        assert!(may_have_landed(&error), "HTTP {status}");
    }
}
