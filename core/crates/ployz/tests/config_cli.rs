//! Authoring through the Config Store: the JSON contract of `project new`, `env new`,
//! `service add`, `get`, `set`, `unset`, `diff`, `deploy --skip-deploy`, `discard`, `schema` and
//! `explain`, and Setting path completion. Store cases run twice: on the hidden
//! in-process Store (`PLOYZ_STORE=sqlite:PATH`) and over HTTPS to a Cloud that hosts
//! the Store behind `/api/config/{read,write}`, as `PLOYZ_TOKEN`. `link`, `status`
//! and `ctx` show how a directory link and overrides pick the scope.
#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;

use ployz_core::config::{ClientSealedValue, ServiceGitAccess};
use ployz_core::{Keypair, RpcError, RpcErrorCode, SealedIdentity};
use ployz_store::{
    Actor, AuthorizedRepository, ClusterDomain, ClusterDomainStatus, Command as StoreCommand,
    ConfigStore, DomainEvidence, Hostname, OrganizationId, Permission, Principal, Removal,
    RunnerId, SealingKey, Trusted, Written,
};
use serde_json::{Value, json};

#[allow(dead_code, reason = "This file's Engine answers only the Server list.")]
#[path = "connect/support.rs"]
mod engine;

/// Where the CLI's Config Store is.
enum Target {
    Local(tempfile::TempDir),
    Cloud { url: String, token: &'static str },
}

/// A new empty Store, reached both ways.
fn targets() -> [Target; 2] {
    [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud(),
            token: "ployz_alice",
        },
    ]
}

/// A `ployz` process with its own home, away from any ambient Store or sign-in.
fn isolated(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ployz"));
    command
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("PLOYZ_CONFIG", home.join("config.yaml"))
        .env_remove("PLOYZ_STORE")
        .env_remove("PLOYZ_TOKEN")
        .env("PLOYZ_CLOUD_URL", "http://127.0.0.1:9")
        .env_remove("PLOYZ_PROJECT")
        .env_remove("PLOYZ_ENV");
    command
}

impl Target {
    /// A `ployz` process pointed at this Store.
    fn command(&self, home: &std::path::Path) -> Command {
        let mut command = isolated(home);
        match self {
            Self::Local(dir) => {
                let store = dir.path().join("store.db");
                command.env("PLOYZ_STORE", format!("sqlite:{}", store.display()));
            }
            Self::Cloud { url, token } => {
                command
                    .env("PLOYZ_CLOUD_URL", url)
                    .env("PLOYZ_TOKEN", token);
            }
        }
        command
    }

    /// The fake Cloud's URL, which keys what it took in [`REQUESTS`].
    fn url(&self) -> &str {
        match self {
            Self::Local(_) => panic!("a Local Store takes no requests"),
            Self::Cloud { url, .. } => url,
        }
    }
}

/// A fake Cloud whose Cluster seals, so a secret can be set there.
fn cloud_target() -> Target {
    Target::Cloud {
        url: fake_cloud(),
        token: "ployz_alice",
    }
}

/// `local`'s Store opened in this process, beside the `ployz` processes that open it.
fn local_store(local: &Target) -> ConfigStore {
    let Target::Local(dir) = local else {
        unreachable!("a local Store")
    };
    ConfigStore::open(
        &format!("sqlite:{}", dir.path().join("store.db").display()),
        SealingKey::from_file(&dir.path().join("store.db.key")).unwrap(),
    )
    .unwrap()
}

/// Run `ployz --json ARGS` against `target`; returns (exit code, stdout JSON).
fn ployz(target: Option<&Target>, args: &[&str]) -> (Option<i32>, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut command = target.map_or_else(
        || isolated(home.path()),
        |target| target.command(home.path()),
    );
    let output = command.arg("--json").args(args).output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

fn ok(store: &Target, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(0), "{args:?}: {json}");
    json
}

/// A refusal, exiting 2 when the command must change and 1 otherwise.
fn error(store: &Target, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(store), args);
    let exit = match json["error"]["code"].as_str() {
        Some("invalid_argument" | "confirmation_required" | "ambiguous") => 2,
        _ => 1,
    };
    assert_eq!(code, Some(exit), "{args:?}: {json}");
    json.get("error").cloned().unwrap()
}

fn failed(store: &Target, args: &[&str], exit: i32) -> Value {
    let (code, json) = ployz(Some(store), args);
    assert_eq!(code, Some(exit), "{args:?}: {json}");
    json.get("error").cloned().unwrap()
}

const UNREACHABLE_SERVER: [&str; 4] = ["--connect", "tcp://127.0.0.1:1", "--ssh-timeout", "1"];

/// Run `ployz --json ARGS` where no Server answers, so its Deployment ends not executed: exit 1.
fn no_server_answers(store: &Target, args: &[&str]) -> Value {
    let (code, json) = ployz(Some(store), &[args, &UNREACHABLE_SERVER].concat());
    assert_eq!(code, Some(1), "{args:?}: {json}");
    json
}

/// Cloud's `/api/config` contract over one in-memory Store: the bearer
/// `ployz_<org>` acts in Organization `<org>`; any other caller is refused 401.
/// Its GitHub: installation 7 grants `acme/web`, with branches `main` and `dev`.
/// Its worker runs each admitted Deployment with Cloud's runner, on a Cluster
/// whose only Server never answers.
fn fake_cloud() -> String {
    fake_cloud_with_dispatch(dispatch)
}

fn fake_cloud_with_dispatch(worker: fn(&std::sync::Arc<ConfigStore>, &Written)) -> String {
    cloud(worker, std::sync::Arc::default(), |_| evidence(), ready())
}

/// What Cloud's worker does with each write the Store took.
type Worker = fn(&std::sync::Arc<ConfigStore>, &Written);
/// What Cloud observed before it sent a write.
type Observe = fn(&StoreCommand) -> Trusted;

fn cloud(
    worker: Worker,
    approvals: std::sync::Arc<Approvals>,
    observe: Observe,
    sealing: Option<SealingState>,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let store = std::sync::Arc::new(
        ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap(),
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            match serve(
                &store,
                &approvals,
                stream.unwrap(),
                worker,
                observe,
                &sealing,
            ) {
                Ok(()) => {}
                // A CLI stopped by Ctrl-C hangs up mid-request.
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => {}
                Err(error) => panic!("{error}"),
            }
        }
    });
    url
}

/// Cloud's worker: run an admitted Deployment in the background.
fn dispatch(store: &std::sync::Arc<ConfigStore>, written: &Written) {
    let Written::Deployment(admitted) = written else {
        return;
    };
    let (store, id) = (std::sync::Arc::clone(store), admitted.id.clone());
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let unreachable = ployz::context::Connection::tcp("127.0.0.1:1".parse().unwrap());
        // Its Deployment ends not executed, or was replaced before it started.
        let _ = runtime.block_on(ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            store,
            id,
            RunnerId::parse("cloud-worker").unwrap(),
            vec![unreachable],
            Ok(Default::default()),
        ));
    });
}

fn serve(
    store: &std::sync::Arc<ConfigStore>,
    approvals: &Approvals,
    mut stream: TcpStream,
    worker: Worker,
    observe: Observe,
    sealing: &Option<SealingState>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let method = request.split(' ').next().unwrap_or_default().to_owned();
    let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
    let (mut length, mut organization, mut approval) = (0, None, None);
    loop {
        let mut header = String::new();
        reader.read_line(&mut header)?;
        if header.trim().is_empty() {
            break;
        }
        let header = header.to_ascii_lowercase();
        if let Some(value) = header.strip_prefix("content-length:") {
            length = value.trim().parse().unwrap();
        }
        if let Some(value) = header.strip_prefix("x-ployz-approval:") {
            approval = Some(value.trim().to_owned());
        }
        if let Some(value) = header.strip_prefix("authorization: bearer ployz_") {
            // Cloud names who acts: here, the token's Organization.
            organization = Some(Actor {
                organization: OrganizationId::parse(value.trim()).unwrap(),
                principal: Some(Principal::parse(value.trim()).unwrap()),
            });
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    REQUESTS.lock().unwrap().push((
        format!("http://{}", stream.local_addr()?),
        path.clone(),
        String::from_utf8_lossy(&body).into_owned(),
    ));
    let (status, reply) = match (organization, path.as_str()) {
        (None, _) => (401, json!({ "code": "UNAUTHORIZED" })),
        (Some(who), "/api/config/read") => answer(store.read_trusted(
            &who,
            &serde_json::from_slice::<ployz_store::Query>(&body).unwrap(),
            &evidence(),
        )),
        (Some(_), "/api/config/write")
            if let Some(refused) = approvals.gate(&mut body, approval.as_deref()) =>
        {
            refused
        }
        (Some(_), decide) if decide.starts_with("/api/cli/approvals/") => {
            let id = decide.trim_start_matches("/api/cli/approvals/");
            approvals.answer(&method, id, &body)
        }
        (Some(who), "/api/config/write") => {
            let command: StoreCommand = serde_json::from_slice(&body).unwrap();
            let written = store.write_trusted(&who, &command, &observe(&command));
            if let (
                StoreCommand::Admit(_) | StoreCommand::Start(_) | StoreCommand::Remove(_),
                Ok(written),
            ) = (&command, &written)
            {
                worker(store, written);
            }
            answer(written)
        }
        (Some(_), upload) if upload.starts_with("/api/config/upload/") => {
            let id = upload.trim_start_matches("/api/config/upload/").to_owned();
            UPLOADS.lock().unwrap().insert(id.clone(), body);
            (200, json!({ "uploaded": id }))
        }
        (Some(who), "/api/config/sealing") if let Some(state) = sealing => {
            match state {
                SealingState::NoServer => (200, json!({ "state": "no_server" })),
                SealingState::Ready(public_key) => {
                    let query: ployz_store::SealingTargetsQuery =
                        serde_json::from_slice(&body).unwrap();
                    match store.read_trusted(&who, &query, &evidence()) {
                        Ok(targets) => (
                            200,
                            json!({
                                "state": "ready",
                                "publicKey": public_key,
                                "targets": ployz_store::View::SealingTargets(targets),
                            }),
                        ),
                        // Cloud answers a Store's not_found with 404, as it does any read.
                        Err(error) if error.code == RpcErrorCode::NotFound => {
                            (404, json!({ "error": error }))
                        }
                        Err(error) => (409, json!({ "error": error })),
                    }
                }
            }
        }
        // A Cloud from before the CLI sealed answers the unknown route as it does any.
        (Some(_), "/api/config/sealing") => (
            404,
            json!({ "_tag": "PublicError", "code": "NOT_FOUND", "message": "Not found." }),
        ),
        (Some(who), "/api/cli/organizations") => {
            let id = who.organization.as_str();
            let organization = json!({ "id": id, "slug": id, "name": id, "current": true });
            (200, json!({ "organizations": [organization] }))
        }
        (Some(_), "/api/cli/github") => (
            200,
            json!({
                "connect_url": "https://cloud.example/api/github/connect?organization=acme",
                "ready": true,
                "installations": [{
                    "id": 7, "account": "acme", "account_type": "Organization", "repositories": 1,
                    "connected_by": "Ada", "connected_at": "2026-10-14T00:00:00.000Z",
                }],
                "repositories": [{ "repository": "acme/web", "private": true, "default_branch": "main", "installation": 7 }],
            }),
        ),
        (Some(_), "/api/cli/github/branches?repository=acme%2Fweb") => (
            200,
            json!({ "repository": "acme/web", "access": "connected", "default_branch": "main", "branches": ["dev", "main"] }),
        ),
        (
            Some(_),
            "/api/cli/github/tree?match=**%2F*.rs&path=src&ref=dev&repository=acme%2Fweb",
        ) => (
            200,
            json!({ "repository": "acme/web", "ref": "dev", "paths": ["src/main.rs"], "truncated": false }),
        ),
        (Some(_), "/api/cli/github/file?path=Dockerfile&repository=acme%2Fweb") => (
            200,
            json!({ "repository": "acme/web", "ref": "main", "path": "Dockerfile", "size": 12, "content": "FROM alpine\n" }),
        ),
        (Some(_), "/api/cli/github/file?path=src&repository=acme%2Fweb") => (
            422,
            json!({ "error": { "code": "invalid_argument", "message": "src is a directory: list it with github tree.", "details": null } }),
        ),
        (Some(_), route)
            if route.starts_with("/api/cli/github/") && route.contains("acme%2Fnope") =>
        {
            (
                404,
                json!({ "error": {
                "code": "not_found",
                "message": "No repository by that name that this Organization can read.",
                "details": { "valid_children": ["acme/web"] },
            } }),
            )
        }
        (Some(_), "/api/cli/github/acme") => (
            200,
            json!({
                "disconnected": { "id": 7, "account": "acme" },
                "uninstall_url": "https://github.com/organizations/acme/settings/installations/7",
            }),
        ),
        (Some(who), "/api/cli/server-access")
            if who.organization.as_str() == "linus"
                || runtime_refusal(who.organization.as_str()).is_some() =>
        {
            (
                403,
                json!({ "error": {
                    "code": "forbidden",
                    "message": "Server access needs an admin. Ask an owner or admin of acme.",
                    "details": { "organization": "acme" },
                } }),
            )
        }
        (Some(_), "/api/cli/server-access") => (
            200,
            json!({ "connections": [{ "tcp": engine() }], "unreachable": [] }),
        ),
        (Some(who), "/api/cli/agent/ask")
            if let Some(refused) = runtime_refusal(who.organization.as_str()) =>
        {
            refused
        }
        (Some(_), "/api/cli/agent/ask") => (200, servers_read(&body)),
        (Some(_), route) if let Some(route) = route.strip_prefix("/api/cli/") => {
            approvals.run(&method, route, &body, approval.as_deref())
        }
        (Some(_), _) => (404, json!({ "code": "NOT_FOUND" })),
    };
    let reply = reply.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    )
}

/// How this Cloud refuses every runtime read in Organization `organization`, which it
/// also refuses Server access. `stagehand` acts with a deploy token held to staging,
/// which Cloud refuses each Namespace no Deployment has fixed, its own included.
fn runtime_refusal(organization: &str) -> Option<(u16, Value)> {
    let (status, code, message, details) = match organization {
        "stagehand" => (
            403,
            "forbidden",
            "This token can deploy only to staging.",
            json!({ "environments": ["staging"] }),
        ),
        "serverless" => (
            409,
            "conflict",
            "This Organization has no servers yet. Add one from a terminal; staging still works without one.",
            json!({ "fix": { "fix": "connect_servers" } }),
        ),
        "unanswered" => (
            503,
            "unavailable",
            "Your servers aren't answering. Try again once they are.",
            json!({ "fix": { "fix": "connect_servers" } }),
        ),
        "unpermitted" => (
            403,
            "forbidden",
            "This token's grant doesn't include servers:read.",
            json!({ "permission": "servers:read" }),
        ),
        _ => return None,
    };
    let error = json!({ "code": code, "message": message, "details": details });
    Some((status, json!({ "error": error })))
}

/// What this Cloud's Servers answer when Cloud reads them: `web-1` has one log line
/// for runtime Service `web` and nothing for any other, `web-2` does not answer for
/// `worker` or for containers, and no Server has a metric reading.
fn servers_read(body: &[u8]) -> Value {
    let ask: Value = serde_json::from_slice(body).unwrap();
    let seen = json!({ "read_at": 0, "from": "server", "unanswered": [] });
    match ask["ask"]["query"].as_str() {
        Some("logs") if ask["ask"]["service"] == "worker" => json!({
            "servers": [],
            "seen": { "read_at": 0, "from": "server", "unanswered": ["web-2"] },
            "notes": ["web-2 did not answer: timed out"],
        }),
        Some("logs") => {
            let servers = if ask["ask"]["service"] == "web" {
                let text =
                    json!({ "status": "shown", "lines": ["ready"], "cut": false, "redacted": 0 });
                json!([{ "server": "web-1", "text": text }])
            } else {
                json!([])
            };
            json!({ "servers": servers, "seen": seen, "notes": [] })
        }
        Some("metrics") if ask["ask"]["subject"]["service"] == "worker" => json!({
            "since": 0, "until": 0, "metrics": [], "oom_kills": 0, "exits": 0, "restarts": 0,
            "events": [], "notes": ["web-2 did not answer: timed out"],
            "seen": { "read_at": 0, "from": "server", "unanswered": ["web-2"] },
        }),
        Some("metrics") => json!({
            "since": 0, "until": 0, "metrics": [], "oom_kills": 0, "exits": 0, "restarts": 0,
            "events": [], "notes": [], "seen": seen,
        }),
        Some("containers") => json!({
            "containers": [],
            "servers": ["web-1", "web-2"],
            "seen": { "read_at": 0, "from": "server", "unanswered": ["web-2"] },
        }),
        other => panic!("this Cloud reads no {other:?}"),
    }
}

/// What this Cloud knows: its GitHub, its one Server, and its Cluster
/// Domain `acme.ployz.app`, ready.
fn evidence() -> Trusted {
    Trusted {
        domains: DomainEvidence {
            cluster_domain: Some(ClusterDomain {
                name: Hostname::parse("acme.ployz.app").unwrap(),
                status: ClusterDomainStatus::Ready,
            }),
            ..DomainEvidence::default()
        },
        servers: Some(1),
        // Cloud sends what the token's member holds: here, everything.
        permissions: Some(Permission::ALL.iter().copied().collect()),
        // Every write over `/api/config/write` is a CLI's.
        from_cli: true,
        ..github()
    }
}

/// Every upload the fake Cloud took, by Deployment ID.
static UPLOADS: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Every request a fake Cloud took: the Cloud's URL, the path and the body.
static REQUESTS: std::sync::Mutex<Vec<(String, String, String)>> =
    std::sync::Mutex::new(Vec::new());

/// How a fake Cloud answers `/api/config/sealing`. `None`: a Cloud without the route.
#[derive(Clone)]
enum SealingState {
    NoServer,
    /// The Organization seals on the client, to this key.
    Ready(String),
}

/// The Cluster key every Cloud seals to unless a test brings its own to open with.
static KEYPAIR: std::sync::LazyLock<Keypair> = std::sync::LazyLock::new(Keypair::generate);

fn ready() -> Option<SealingState> {
    Some(SealingState::Ready(KEYPAIR.public().to_string()))
}

/// A Cloud answering `/api/config/sealing` with `state`.
fn sealing_cloud(state: Option<SealingState>) -> Target {
    Target::Cloud {
        url: cloud(dispatch, std::sync::Arc::default(), |_| evidence(), state),
        token: "ployz_alice",
    }
}

/// The bodies of every request to `path` that `cloud` took so far.
fn requests_to(cloud: &Target, path: &str) -> Vec<Value> {
    REQUESTS
        .lock()
        .unwrap()
        .iter()
        .filter(|(url, to, _)| url == cloud.url() && to == path)
        .map(|(_, _, body)| serde_json::from_str(body).unwrap())
        .collect()
}

/// No request `cloud` took carried `plaintext`.
fn cloud_never_saw(cloud: &Target, plaintext: &str) {
    for (url, path, body) in REQUESTS.lock().unwrap().iter() {
        assert!(
            url != cloud.url() || !body.contains(plaintext),
            "{path} carried {plaintext}: {body}"
        );
    }
}

/// Every value sealed in the CLI that `cloud`'s writes carried, by variable name.
fn sealed_in_writes(cloud: &Target) -> std::collections::BTreeMap<String, ClientSealedValue> {
    fn collect(value: &Value, into: &mut std::collections::BTreeMap<String, ClientSealedValue>) {
        match value {
            Value::Object(fields) => {
                if fields.contains_key("sealedFor") {
                    let sealed: ClientSealedValue = serde_json::from_value(value.clone()).unwrap();
                    into.insert(sealed.value.key.clone(), sealed);
                } else {
                    fields.values().for_each(|field| collect(field, into));
                }
            }
            Value::Array(items) => items.iter().for_each(|item| collect(item, into)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    let mut sealed = std::collections::BTreeMap::new();
    for body in requests_to(cloud, "/api/config/write") {
        collect(&body, &mut sealed);
    }
    sealed
}

/// What `sealed` was authored as, in the fake Cloud's Organization.
fn authored(sealed: &ClientSealedValue) -> SealedIdentity {
    let ployz_core::SealedScope::Service(service) = sealed.value.scope else {
        panic!("the CLI seals for one Service");
    };
    SealedIdentity {
        organization: "alice".into(),
        environment: sealed.sealed_for,
        service,
    }
}

fn github() -> Trusted {
    Trusted {
        repositories: vec![AuthorizedRepository {
            repository: ployz_store::RepositoryName::parse("acme/web").unwrap(),
            repository_id: ployz_store::RepositoryId::parse(11).unwrap(),
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: ployz_store::BranchName::parse("main").unwrap(),
            branches: vec![ployz_store::BranchName::parse("dev").unwrap()],
        }],
        ..Trusted::default()
    }
}

#[derive(Default)]
struct Approvals {
    rows: Vec<(&'static str, &'static str)>,
    deletes_volume: Option<&'static str>,
    asking: std::sync::Mutex<usize>,
    calls: std::sync::Mutex<Vec<(String, String, Value)>>,
    runs: std::sync::Mutex<Vec<String>>,
    bodies: std::sync::Mutex<Vec<Value>>,
    /// Each Store command the gate weighed, as Cloud received it.
    gated: std::sync::Mutex<Vec<Value>>,
    ran: std::sync::Condvar,
}

impl Approvals {
    fn of(rows: &[(&'static str, &'static str)]) -> Self {
        Self {
            rows: rows.to_vec(),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<(String, String, Value)> {
        self.calls.lock().unwrap().clone()
    }

    fn status(&self, id: &str) -> &'static str {
        let calls = self.calls.lock().unwrap();
        let posted = calls.iter().rev().find_map(|(method, call, decision)| {
            (method == "POST" && call == id).then(|| match decision.get("approve") {
                Some(_) => "approved",
                None => "denied",
            })
        });
        let polled = calls
            .iter()
            .any(|(method, call, _)| method == "GET" && call == id);
        let decided = self.rows.iter().find(|row| row.0 == id).unwrap().1;
        posted.unwrap_or(if polled { decided } else { "pending" })
    }

    fn gate(&self, body: &mut Vec<u8>, approval: Option<&str>) -> Option<(u16, Value)> {
        let command: Value = serde_json::from_slice(body).unwrap();
        let deploy = command["command"] == "admit" && command["admit"] == "deploy";
        let asks = ["publish", "sync", "take", "set_pr_plan", "set_build_order"]
            .iter()
            .any(|asking| command["command"] == *asking);
        if self.rows.is_empty() || !(deploy || asks) {
            return None;
        }
        self.gated.lock().unwrap().push(command.clone());
        if let (Some(volume), true) = (self.deletes_volume, deploy) {
            if command["accept_volume_loss"] != json!([volume]) || command["version"] != REVIEWED {
                let error = json!({
                    "code": "confirmation_required",
                    "message": format!("This Deploy permanently deletes the data of {volume}"),
                    "details": { "accept": [volume], "version": REVIEWED },
                });
                return Some((409, json!({ "error": error })));
            }
            *body = without_accepted_loss(command);
        }
        let mut asking = self.asking.lock().unwrap();
        match approval.map(|id| (id, self.status(id))) {
            Some((_, "approved")) => return None,
            Some((id, "denied")) => {
                let error = denied(
                    id,
                    "A human denied the request to save production: the worker still drains",
                );
                return Some((403, json!({ "error": error })));
            }
            Some((_, "superseded")) => *asking += 1,
            _ => {}
        }
        Some((409, json!({ "error": asked(self.rows[*asking].0) })))
    }

    fn runs(&self) -> Vec<String> {
        self.runs.lock().unwrap().clone()
    }

    fn bodies(&self) -> Vec<Value> {
        self.bodies.lock().unwrap().clone()
    }

    fn wait_for_run(&self, run: &str) {
        let runs = self.runs.lock().unwrap();
        let (runs, waited) = self
            .ran
            .wait_timeout_while(runs, std::time::Duration::from_secs(30), |runs| {
                !runs.iter().any(|seen| seen == run)
            })
            .unwrap();
        assert!(!waited.timed_out(), "never ran {run}: {runs:?}");
    }

    fn run(&self, method: &str, route: &str, body: &[u8], approval: Option<&str>) -> (u16, Value) {
        let carried = approval
            .map(|id| format!(" approved by {id}"))
            .unwrap_or_default();
        self.runs
            .lock()
            .unwrap()
            .push(format!("{method} {route}{carried}"));
        self.ran.notify_all();
        let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        self.bodies.lock().unwrap().push(body.clone());
        let segments: Vec<&str> = route.split('/').collect();
        match (method, segments.as_slice()) {
            ("POST", ["servers", BUSY_SERVER, "drain"]) => (202, json!({ "id": "drn_running" })),
            ("GET", ["server-drains", "drn_running"]) => (200, json!({ "state": "running" })),
            ("POST", ["servers", server, "drain"]) => self.operation(
                approval,
                json!({ "verb": "drain", "name": server }),
                json!({ "kind": "removes_service", "name": "shop/worker", "node": "svc_1", "path": "worker" }),
                "drn_1",
            ),
            ("GET", ["server-drains", "drn_1"]) => {
                let report = json!({
                    "server": cloud_server(),
                    "services_role": "turned_off",
                    "services": [],
                    "stopped": null,
                    "remaining": { "kind": "observed", "services": [], "unchosen": [] },
                });
                (200, json!({ "state": "finished", "report": report }))
            }
            ("POST", ["servers", _, "upgrade"]) if body["channel"] != "stable" => {
                let message = "This Organization upgrades along stable";
                let details = json!({ "channel": "stable" });
                let error =
                    json!({ "code": "channel_mismatch", "message": message, "details": details });
                (409, json!({ "error": error }))
            }
            ("POST", ["servers", OFFLINE_SERVER, "upgrade"]) => {
                let message = "This Server isn't online and idle, so it can't take an Upgrade now.";
                (503, json!({ "error": { "code": "unavailable", "message": message } }))
            }
            ("POST", ["servers", OLD_CLOUD_SERVER, "upgrade"]) => (
                409,
                json!({ "_tag": "PublicError", "code": "CONFLICT", "message": "Your servers aren't answering. Try again once they are." }),
            ),
            ("POST", ["servers", BUSY_SERVER, "upgrade"]) => (202, json!({ "id": "upg_busy" })),
            ("GET", ["server-upgrades", "upg_busy"]) => (
                200,
                json!({ "state": "ended", "code": "busy", "message": "The Server was busy with a build or another Upgrade, or went offline, so it took no Upgrade." }),
            ),
            ("POST", ["servers", _, "upgrade"]) => (202, json!({ "id": "upg_1" })),
            ("GET", ["server-upgrades", "upg_1"]) => (
                200,
                json!({
                    "state": "finished",
                    "outcome": "succeeded",
                    "from_version": "0.2.0",
                    "target_version": "0.3.0",
                    "message": null,
                }),
            ),
            ("GET", ["namespaces", namespace, "clean"]) => {
                let volume = json!({
                    "machine_id": CLOUD_SERVER,
                    "name": format!("{namespace}_data"),
                    "label": "used by web at /data on web-1",
                });
                (200, json!({ "namespace": namespace, "volumes": [volume] }))
            }
            ("POST", ["namespaces", "slow", "clean"]) => (202, json!({ "id": "cln_slow" })),
            ("GET", ["namespace-cleanups", "cln_slow"]) => (200, json!({ "state": "running" })),
            ("POST", ["namespaces", namespace, "clean"]) => self.operation(
                approval,
                json!({ "verb": "clean", "name": namespace }),
                json!({ "kind": "deletes_volume", "name": "data", "node": "a/data", "path": "volumes/a/data" }),
                "cln_1",
            ),
            ("GET", ["namespace-cleanups", "cln_1"]) => {
                (200, json!({ "state": "finished", "volumes": ["data"] }))
            }
            _ => (404, json!({ "code": "NOT_FOUND" })),
        }
    }

    fn operation(
        &self,
        approval: Option<&str>,
        operation: Value,
        effect: Value,
        started: &str,
    ) -> (u16, Value) {
        if let Some(id) = approval.filter(|id| self.status(id) == "denied") {
            let message = format!(
                "A human denied the request to {} {}: declined",
                operation["verb"].as_str().unwrap(),
                operation["name"].as_str().unwrap()
            );
            let error = denied(id, &message);
            return (403, json!({ "error": error }));
        }
        let approved = approval.is_some_and(|id| self.status(id) == "approved");
        if self.rows.is_empty() || approved {
            return (202, json!({ "id": started }));
        }
        let error = json!({
            "code": "approval_required",
            "message": format!("A human must approve this first: this {} touches {}", operation["verb"], operation["name"]),
            "details": {
                "approval_id": self.rows[0].0,
                "approval": format!("{}:abc", operation["verb"].as_str().unwrap()),
                "effects": [effect],
                "operation": operation,
            },
        });
        (409, json!({ "error": error }))
    }

    fn answer(&self, method: &str, id: &str, body: &[u8]) -> (u16, Value) {
        let decision = serde_json::from_slice(body).unwrap_or(Value::Null);
        let status = self.status(id);
        self.calls
            .lock()
            .unwrap()
            .push((method.to_owned(), id.to_owned(), decision));
        (200, json!({ "approval": { "id": id, "status": status } }))
    }
}

/// Cloud's `approval_denied` refusal keeps the approval in `details` and out of the message.
fn denied(id: &str, message: &str) -> Value {
    json!({
        "code": "approval_denied",
        "message": message,
        "details": { "approval": { "id": id, "status": "denied" } },
    })
}

fn asked(id: &str) -> Value {
    json!({
        "code": "approval_required",
        "message": "A human must approve this first: this publish removes Service worker",
        "details": {
            "approval_id": id,
            "approval": "3:abc",
            "effects": [{ "kind": "removes_service", "name": "worker", "node": "svc_1", "path": "worker" }],
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
    })
}

const REVIEWED: &str = "3:1:0.1";

/// `command` as the real Store must see it: the fake Cloud already confirmed the
/// loss, and the Store has no Applied State that deletes a Volume.
fn without_accepted_loss(mut command: Value) -> Vec<u8> {
    command["accept_volume_loss"] = json!([]);
    command["version"] = Value::Null;
    serde_json::to_vec(&command).unwrap()
}

fn approving(rows: &[(&'static str, &'static str)]) -> (Target, std::sync::Arc<Approvals>) {
    serving(Approvals::of(rows))
}

fn serving(approvals: Approvals) -> (Target, std::sync::Arc<Approvals>) {
    let approvals = std::sync::Arc::new(approvals);
    let target = Target::Cloud {
        url: cloud(
            dispatch,
            std::sync::Arc::clone(&approvals),
            |_| evidence(),
            ready(),
        ),
        token: "ployz_alice",
    };
    ok(&target, &["project", "new", "shop"]);
    ok(&target, &["service", "add", "web", "--image", "nginx:1"]);
    (target, approvals)
}

fn approve_in_cloud(target: &Target, id: &str) {
    let Target::Cloud { url, token } = target else {
        unreachable!("approvals live in Cloud")
    };
    let body = json!({ "approve": { "digest": "3:abc" } }).to_string();
    let mut stream = TcpStream::connect(url.trim_start_matches("http://")).unwrap();
    write!(
        stream,
        "POST /api/cli/approvals/{id} HTTP/1.1\r\nauthorization: Bearer {token}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
}

const CLOUD_SERVER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OFFLINE_SERVER: &str = "cccccccccccccccccccccccccccccccc";
const BUSY_SERVER: &str = "dddddddddddddddddddddddddddddddd";
const OLD_CLOUD_SERVER: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

/// The Engine Cloud's Server access dials: it names every Server above.
fn engine() -> std::net::SocketAddr {
    static ENGINE: std::sync::OnceLock<std::net::SocketAddr> = std::sync::OnceLock::new();
    *ENGINE.get_or_init(|| {
        let (sent, address) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let mut service = engine::DiscoveryService::new(engine::test_description());
                service.machines = [
                    ('a', "web-1"),
                    ('c', "web-2"),
                    ('d', "web-3"),
                    ('e', "web-4"),
                ]
                .into_iter()
                .map(|(hex, name)| engine::machine(hex, name))
                .collect();
                let (address, server) = engine::serve_discovery(service).await;
                sent.send(address).unwrap();
                server.await
            })
        });
        address.recv().unwrap()
    })
}

fn cloud_server() -> ployz_core::Machine {
    ployz_core::Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: false,
        accepts_ingress: true,
        id: ployz_core::MachineId::parse(CLOUD_SERVER).unwrap(),
        name: ployz_core::MachineName::parse("web-1").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: ployz_core::WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: Vec::new(),
        runtime: Default::default(),
        build_concurrency: None,
    }
}

fn running(rows: &[(&'static str, &'static str)]) -> (Target, std::sync::Arc<Approvals>) {
    let approvals = std::sync::Arc::new(Approvals::of(rows));
    let target = Target::Cloud {
        url: cloud(
            dispatch,
            std::sync::Arc::clone(&approvals),
            |_| evidence(),
            ready(),
        ),
        token: "ployz_alice",
    };
    (target, approvals)
}

fn person(target: &Target, home: &std::path::Path, args: &[&str]) -> (Option<i32>, String) {
    let output = target
        .command(home)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn answer<T: serde::Serialize>(result: Result<T, RpcError>) -> (u16, Value) {
    match result {
        Ok(value) => (200, serde_json::to_value(value).unwrap()),
        Err(error) => (409, json!({ "error": error })),
    }
}

#[test]
fn an_agent_creates_an_image_service_and_reads_its_settings() {
    for store in &targets() {
        let created = ok(store, &["project", "new", "shop"]);
        assert_eq!(created.pointer("/project/name"), Some(&json!("shop")));
        assert_eq!(
            created.pointer("/environment/name"),
            Some(&json!("production"))
        );

        let added = ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        assert_eq!(added.pointer("/service/name"), Some(&json!("web")));
        assert_eq!(
            added.get("staged"),
            Some(&json!([
                "web.cpuLimit",
                "web.healthcheck",
                "web.image",
                "web.maxRetries",
                "web.memLimit",
                "web.preDeployCommand",
                "web.privateDns",
                "web.replicas",
                "web.restartPolicy",
                "web.startCommand",
                "web.updateOrder"
            ]))
        );
        assert!(added.get("immediate").is_none());

        let set = ok(
            store,
            &[
                "set",
                "web.replicas=3",
                "web.startCommand=nginx -g 'daemon off;'",
            ],
        );
        assert_eq!(
            set.get("staged"),
            Some(&json!(["web.replicas", "web.startCommand"]))
        );
        assert_eq!(set.pointer("/environment/revision"), Some(&json!(3)));

        let got = ok(store, &["get", "web"]);
        assert_eq!(
            got.pointer("/settings").unwrap().as_array().unwrap().len(),
            13
        );
        // Read beside a Service, its domains show even when it has none.
        assert_eq!(got.get("domains"), Some(&json!([])));
        // Service names are lowercase, so `WEB` reads Service `web` with its domains.
        assert_eq!(ok(store, &["get", "WEB"]).get("domains"), Some(&json!([])));
        assert_eq!(
            got.pointer("/settings/8"),
            Some(&json!({ "path": "web.replicas", "value": 3, "default": 1, "apply": "staged" }))
        );
        assert_eq!(
            got.get("values"),
            Some(&json!({
                "image": "nginx:1",
                "maxRetries": 10,
                "privateDns": "web",
                "replicas": 3,
                "restartPolicy": "unless-stopped",
                "startCommand": "nginx -g 'daemon off;'",
            }))
        );

        ok(store, &["unset", "web.replicas"]);
        let got = ok(store, &["get", "web.replicas"]);
        assert_eq!(got.pointer("/settings/0/value"), Some(&json!(1)));
        assert_eq!(got.get("domains"), None);
    }
}

#[test]
fn a_service_takes_an_update_order_and_unset_leaves_it_to_each_deploy() {
    let (code, schema) = ployz(None, &["schema", "web.updateOrder"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        schema.get("enum"),
        Some(&json!(["start-first", "stop-first", "rolling"]))
    );
    for store in &targets() {
        shop_with_web(store);
        assert_eq!(setting(store, "web.updateOrder"), Value::Null);
        let set = ok(store, &["set", "web.updateOrder=rolling"]);
        assert_eq!(set.get("staged"), Some(&json!(["web.updateOrder"])));
        assert_eq!(setting(store, "web.updateOrder"), json!("rolling"));
        let refused = error(store, &["set", "web.updateOrder=blue-green"]);
        assert_eq!(refused["code"], "invalid_argument", "{refused}");
        assert_eq!(
            refused["message"],
            "updateOrder: Expected start-first, stop-first or rolling"
        );
        ok(store, &["unset", "web.updateOrder"]);
        assert_eq!(setting(store, "web.updateOrder"), Value::Null);
    }
}

fn shop_with_web(store: &Target) {
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "nginx:1"]);
}

fn setting(store: &Target, path: &str) -> Value {
    ok(store, &["get", path])["settings"][0]["value"].clone()
}

#[test]
fn a_service_name_over_63_characters_is_refused_with_the_whole_rule() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    shop_with_web(&store);
    let refused = failed(&store, &["deploy", &"a".repeat(64)], 2);
    assert_eq!(
        refused["message"],
        "Expected Service names: each up to 63 lowercase letters, digits and -, starting and ending with a letter or digit"
    );
}

#[test]
fn a_healthcheck_is_set_by_path_or_patch_and_refuses_control_characters() {
    for store in &targets() {
        shop_with_web(store);
        ok(store, &["set", "web.healthcheck=/up"]);
        assert_eq!(
            setting(store, "web.healthcheck"),
            json!({ "path": "/up", "timeoutSeconds": 300 })
        );
        let patch = json!({"healthcheck":{"command":"pg_isready","timeoutSeconds":60}}).to_string();
        ok(store, &["set", "web", "--patch", &patch]);
        assert_eq!(
            setting(store, "web.healthcheck"),
            json!({ "command": "pg_isready", "timeoutSeconds": 60 })
        );
        ok(store, &["set", "web.healthcheck=/up"]);
        assert_eq!(
            setting(store, "web.healthcheck"),
            json!({ "path": "/up", "timeoutSeconds": 60 })
        );
        let refused = failed(store, &["set", "web.healthcheck=pg_isready"], 2);
        assert_eq!(
            refused["message"],
            "healthcheck: text sets an HTTP path like /health; a command takes an object like {\"command\":\"pg_isready\",\"timeoutSeconds\":60}, and an HTTP check's timeout one like {\"path\":\"/health\",\"timeoutSeconds\":60}"
        );
        let next = refused["details"]["next"].as_str().unwrap();
        let words = shell_words::split(next).unwrap();
        assert_eq!(words[0], "ployz", "{next}");
        ok(
            store,
            &words[1..].iter().map(String::as_str).collect::<Vec<_>>(),
        );
        assert_eq!(
            setting(store, "web.healthcheck"),
            json!({ "command": "pg_isready", "timeoutSeconds": 60 })
        );
        ok(
            store,
            &[
                "set",
                r#"web.healthcheck={"path":"/health","timeoutSeconds":60}"#,
            ],
        );
        assert_eq!(
            setting(store, "web.healthcheck"),
            json!({ "path": "/health", "timeoutSeconds": 60 })
        );
        ok(store, &["unset", "web.healthcheck"]);
        assert_eq!(setting(store, "web.healthcheck"), Value::Null);
        let before_invalid = ok(store, &["diff"]);
        for path in [
            "/private\u{1}probe",
            "/private\tprobe",
            "/private\nprobe",
            "/private\u{85}probe",
        ] {
            let patch = json!({"cpuLimit":1, "healthcheck":{"path":path}}).to_string();
            let refused = error(store, &["set", "web", "--patch", &patch]);
            assert_eq!(refused["code"], "invalid_argument");
            assert!(!refused.to_string().contains("private"));
            assert_eq!(ok(store, &["diff"]), before_invalid);
        }
        ok(store, &["set", "web.healthcheck=/café?escaped=%0A"]);
        assert_eq!(
            setting(store, "web.healthcheck")["path"],
            "/café?escaped=%0A"
        );
    }
}

#[test]
fn unsetting_private_dns_restores_the_service_name() {
    for store in &targets() {
        shop_with_web(store);
        ok(store, &["set", "web.privateDns=front"]);
        assert_eq!(setting(store, "web.privateDns"), json!("front"));
        ok(store, &["unset", "web.privateDns"]);
        assert_eq!(setting(store, "web.privateDns"), json!("web"));
    }
}

#[test]
fn a_path_given_twice_is_refused_and_changes_nothing() {
    for store in &targets() {
        shop_with_web(store);
        let refused = failed(store, &["set", "web.replicas=2", "web.replicas=3"], 2);
        assert_eq!(refused["code"], "invalid_argument");
        assert_eq!(
            refused["message"],
            "web.replicas is changed twice in one edit; change it once"
        );
        assert_eq!(setting(store, "web.replicas"), json!(1));
    }
}

#[test]
fn an_env_file_that_repeats_a_key_imports_its_last_value() {
    for store in &targets() {
        shop_with_web(store);
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "LOG_LEVEL=info\nOTHER=ok\nlog_level=debug\n").unwrap();
        let path = file.path().to_str().unwrap();
        let imported = ok(store, &["set", "web", "--from-env-file", path]);
        assert_eq!(
            imported["staged"],
            json!(["web.env.LOG_LEVEL", "web.env.OTHER"])
        );
        assert_eq!(
            ok(store, &["get", "web"])["values"]["env"],
            json!({ "LOG_LEVEL": "debug", "OTHER": "ok" })
        );
    }
}

/// A teammate's history message or Config file name can't render reversed: each prints a
/// bidi override as its escape, as `get` and `diff` do.
#[test]
fn a_bidi_override_in_a_message_or_config_file_name_prints_escaped() {
    for store in &targets() {
        let human = |args: &[&str]| {
            let home = tempfile::tempdir().unwrap();
            let output = store.command(home.path()).args(args).output().unwrap();
            assert!(output.status.success(), "{output:?}");
            String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap()
        };
        let file = tempfile::NamedTempFile::new().unwrap();
        let from = file.path().to_str().unwrap();
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
        let put = human(&["config", "put", "app", "a\u{202E}b.txt", "--from", from]);
        ok(
            store,
            &["deploy", "--skip-deploy", "--message", "fix \u{202E}evil"],
        );
        std::fs::write(file.path(), "changed").unwrap();
        ok(
            store,
            &["config", "put", "app", "a\u{202E}b.txt", "--from", from],
        );
        ok(store, &["deploy", "--skip-deploy"]);
        let saved = ok(store, &["history"]);
        assert!(saved.to_string().contains("fix \u{202E}evil"), "{saved}");
        for shown in [
            put,
            human(&["config", "ls"]),
            human(&["config", "inspect", "app"]),
            human(&["history"]),
        ] {
            assert!(!shown.contains('\u{202E}'), "{shown}");
        }
        assert!(human(&["config", "ls"]).contains(r"a\u{202e}b.txt"));
        let preview = human(&["history", "restore", "1", "--preview"]);
        assert!(!preview.contains('\u{202E}'), "{preview}");
        assert!(preview.contains(r"a\u{202e}b.txt"), "{preview}");
        let history = human(&["history"]);
        assert!(history.contains(r"fix \u{202e}evil"), "{history}");
        assert!(history.contains(r"a\u{202e}b.txt"), "{history}");
    }
}

#[test]
fn a_store_error_prints_a_bidi_override_escaped_and_json_keeps_it_raw() {
    for store in &targets() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let from = file.path().to_str().unwrap();
        ok(store, &["project", "new", "shop"]);
        ok(store, &["config", "add", "app"]);
        ok(
            store,
            &["config", "put", "app", "a\u{202E}b.txt", "--from", from],
        );
        for args in [
            &["config", "rm-file", "app", "a\u{202E}b.txt.missing"][..],
            &["config", "put", "app", "a\u{202E}b.txt/x", "--from", from],
        ] {
            let refused = error(store, args);
            let message = refused["message"].as_str().unwrap();
            assert!(message.contains("a\u{202E}b.txt"), "{refused}");
            let home = tempfile::tempdir().unwrap();
            let output = store.command(home.path()).args(args).output().unwrap();
            assert!(!output.status.success(), "{output:?}");
            let shown = String::from_utf8(output.stdout).unwrap()
                + &String::from_utf8(output.stderr).unwrap();
            assert!(!shown.contains('\u{202E}'), "{shown}");
            assert!(shown.contains(r"error: Config app has"), "{shown}");
            assert!(shown.contains(r"a\u{202e}b.txt"), "{shown}");
        }
    }
}

#[test]
fn an_env_value_prints_on_one_escaped_line_for_humans() {
    for store in &targets() {
        shop_with_web(store);
        let text = "alpha\nweb.env.SPOOF = true\t\u{1b}[31mcafé";
        ok(store, &["set", &format!("web.env.DISPLAY={text}")]);
        assert_eq!(setting(store, "web.env.DISPLAY"), text);
        let home = tempfile::tempdir().unwrap();
        let human = store
            .command(home.path())
            .args(["get", "web.env.DISPLAY"])
            .output()
            .unwrap();
        assert!(human.status.success());
        let human = String::from_utf8(human.stdout).unwrap();
        assert_eq!(human.lines().count(), 1);
        assert!(!human.contains('\u{1b}'));
        assert!(!human.contains('\t'));
        assert!(human.contains(r#""alpha\nweb.env.SPOOF = true\t\u001b[31mcafé""#));
    }
}

#[test]
fn unsetting_the_image_leaves_an_empty_service() {
    for store in &targets() {
        shop_with_web(store);
        ok(store, &["unset", "web.image"]);
        let listed = ok(store, &["service", "ls"]);
        assert_eq!(listed.pointer("/services/0/source"), Some(&json!("empty")));
    }
}

#[test]
fn an_agent_adds_lists_renames_and_removes_services() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let empty = ok(store, &["service", "add", "worker"]);
        assert_eq!(empty.pointer("/next"), Some(&json!("ployz deploy")));

        let renamed = ok(store, &["service", "rename", "web", "frontend"]);
        assert_eq!(
            renamed.get("service"),
            Some(
                &json!({ "id": renamed["service"]["id"], "name": "frontend", "private_dns": "web" })
            )
        );
        assert_eq!(renamed.get("staged"), Some(&json!(["frontend"])));
        assert_eq!(renamed.get("next"), Some(&json!("ployz deploy")));
        let taken = error(store, &["service", "rename", "worker", "web"]);
        assert_eq!(taken["code"], json!("conflict"));

        let listed = ok(store, &["service", "ls"]);
        assert_eq!(
            listed["services"]
                .as_array()
                .unwrap()
                .iter()
                .map(|service| (&service["name"], &service["source"], &service["change"]))
                .collect::<Vec<_>>(),
            [
                (&json!("frontend"), &json!("image"), &json!("create")),
                (&json!("worker"), &json!("empty"), &json!("create")),
            ]
        );
        let inspected = ok(store, &["service", "inspect", "frontend"]);
        assert_eq!(inspected["private_dns"], json!("web"));
        // The name stays, but unset would now restore the new one.
        let private = &ok(store, &["get", "frontend.privateDns"])["settings"][0];
        assert_eq!(
            (&private["value"], &private["default"]),
            (&json!("web"), &json!("frontend"))
        );
        let explained = ok(store, &["explain", "frontend.privateDns"]);
        assert_eq!(
            explained.pointer("/schema/default"),
            Some(&json!("frontend"))
        );
        assert_eq!(inspected["values"]["image"], json!("nginx:1"));
        assert_eq!(inspected["lineage"], inspected["id"]);

        let removed = ok(store, &["service", "rm", "worker"]);
        assert_eq!(removed.get("staged"), Some(&json!(["worker"])));
        let missing = error(store, &["service", "inspect", "worker"]);
        assert_eq!(missing["code"], json!("not_found"));
        assert_eq!(
            ok(store, &["service", "ls"])["services"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn the_typed_name_alone_removes_a_project_and_an_environment_that_ran_services() {
    fn succeeding(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        if let Written::Deployment(admitted) = written {
            succeed(store, &admitted.id);
        }
        if let Written::Removal(Removal::Removing { left, .. }) = written {
            for deployment in left.iter().filter_map(|left| left.deployment.as_ref()) {
                succeed(store, &deployment.id);
            }
        }
    }
    let targets = [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud_with_dispatch(succeeding),
            token: "ployz_alice",
        },
    ];
    for store in &targets {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["env", "new", "staging"]);
        for environment in ["production", "staging"] {
            ok(
                store,
                &[
                    "service",
                    "add",
                    "web",
                    "--image",
                    "nginx:1",
                    "--env",
                    environment,
                ],
            );
            applied(store, environment);
        }

        // Without a terminal the refusal names the command to run: the name, no version.
        let asked = error(store, &["project", "rm", "shop"]);
        assert_eq!(asked["code"], "confirmation_required", "{asked}");
        assert_eq!(
            asked["details"]["retry"],
            json!("ployz project rm shop --confirm shop")
        );
        // A stale `--expect-version` still binds, so a script that passes one is refused.
        let stale = error(
            store,
            &[
                "project",
                "rm",
                "shop",
                "--confirm",
                "shop",
                "--expect-version",
                "00000000-0000-4000-8000-000000000000:0000000000000000",
            ],
        );
        assert_eq!(stale["code"], "confirmation_required", "{stale}");

        match store {
            // No Server is enrolled in process, so the removal Deployment the name
            // admitted can't take it off: it got past the confirmation in one call.
            Target::Local(_) => {
                // A Ployz config typed on the command line carries into the command
                // that finishes the removal.
                let home = tempfile::tempdir().unwrap();
                let config = home.path().join("my ployz").join("config.yaml");
                let config = config.to_str().unwrap();
                let output = store
                    .command(home.path())
                    .args(["--json", "--ployz-config", config])
                    .args(["env", "rm", "staging", "--confirm", "shop/staging"])
                    .output()
                    .unwrap();
                let admitted: Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(output.status.code(), Some(3), "{admitted}");
                assert_eq!(admitted["environment"]["name"], "staging", "{admitted}");
                assert_eq!(admitted["removal"], "removing", "{admitted}");
                // The removal Deployment that didn't apply, whole, under `deployment`;
                // `id` stays the removal's.
                let deployment = &admitted["deployment"];
                assert_eq!(deployment["remove"], true, "{admitted}");
                assert_eq!(deployment["status"], "failed", "{admitted}");
                assert_eq!(deployment["outcome"]["type"], "not_executed", "{admitted}");
                assert!(deployment["nodes"].is_array(), "{admitted}");
                assert_eq!(admitted.get("status"), None, "{admitted}");
                assert_ne!(admitted["id"], deployment["id"], "{admitted}");
                assert_eq!(admitted["deployments"][0]["id"], deployment["id"]);
                assert_eq!(admitted["left"][0]["deployment"]["id"], deployment["id"]);
                // A removal that didn't apply still names what the name confirmed.
                assert_eq!(admitted["effects"][0]["name"], "web", "{admitted}");
                let next = shell_words::join([
                    "ployz",
                    "--ployz-config",
                    config,
                    "env",
                    "rm",
                    "staging",
                    "--confirm",
                    "shop/staging",
                ]);
                assert_eq!(admitted["next"], next.as_str(), "{admitted}");
                let stderr = String::from_utf8(output.stderr).unwrap();
                assert!(stderr.contains(&format!("retry: {next}")), "{stderr}");
                let (code, admitted) =
                    ployz(Some(store), &["project", "rm", "shop", "--confirm", "shop"]);
                assert_eq!(code, Some(3), "{admitted}");
                assert_eq!(admitted["project"]["name"], "shop", "{admitted}");
                assert_eq!(
                    admitted["environments"],
                    json!(["production", "staging"]),
                    "{admitted}"
                );
                assert_eq!(admitted["removal"], "removing", "{admitted}");
                assert_eq!(admitted["deployment"]["remove"], true, "{admitted}");
                assert_eq!(admitted["deployments"][0]["remove"], true, "{admitted}");
                assert_eq!(
                    admitted["next"], "ployz project rm shop --confirm shop",
                    "{admitted}"
                );
            }
            // This fake Cloud never sweeps, so `--detach` returns once the removal is
            // under way rather than waiting for the sweep.
            Target::Cloud { .. } => {
                let removing = ok(
                    store,
                    &[
                        "env",
                        "rm",
                        "staging",
                        "--confirm",
                        "shop/staging",
                        "--detach",
                    ],
                );
                assert_eq!(removing["deployments"][0]["remove"], true, "{removing}");
                // `--json` names what the name confirmed, as the stderr note does.
                assert_eq!(removing["effects"][0]["kind"], "removes_service");
                assert_eq!(removing["effects"][0]["name"], "web", "{removing}");
                let removing = ok(
                    store,
                    &["project", "rm", "shop", "--confirm", "shop", "--detach"],
                );
                assert_eq!(removing["deployments"][0]["remove"], true, "{removing}");
                assert_eq!(removing["effects"][0]["name"], "web", "{removing}");
            }
        }
    }
}

#[test]
fn a_deployed_service_staged_for_removal_still_inspects() {
    fn succeeding(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        if let Written::Deployment(admitted) = written {
            succeed(store, &admitted.id);
        }
    }
    let targets = [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud_with_dispatch(succeeding),
            token: "ployz_alice",
        },
    ];
    for store in &targets {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        applied(store, "production");
        ok(store, &["service", "rm", "web"]);

        let inspected = ok(store, &["service", "inspect", "web"]);
        assert_eq!(inspected["change"], json!("delete"), "{inspected}");
        assert_eq!(inspected["values"]["image"], json!("nginx:1"));
        assert_eq!(inspected["settings"], json!([]));
        assert_eq!(inspected["domains"], json!([]));
        let home = tempfile::tempdir().unwrap();
        let human = store
            .command(home.path())
            .args(["service", "inspect", "web"])
            .output()
            .unwrap();
        let human = String::from_utf8(human.stdout).unwrap();
        assert!(human.contains("nginx:1"), "{human}");
    }
}

#[test]
fn a_new_services_diff_and_plan_say_the_image_it_runs() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(
        &store,
        &["service", "add", "web", "--image", "traefik/whoami:v1.10.0"],
    );
    ok(&store, &["set", "web.env.PORT=80"]);
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let change = "web (create)\n  web.image: null → \"traefik/whoami:v1.10.0\"\n  web.env.PORT: null → \"80\"\n";
    for args in [&["diff"][..], &["deploy", "--plan"]] {
        let shown = human(args);
        assert!(shown.starts_with(change), "{args:?}: {shown}");
    }
}

/// A Store with Service `web` staged, and a home whose context's one Server,
/// `machine-1`, refuses every connection.
fn no_server_answering() -> (Target, tempfile::TempDir) {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(
        &store,
        &["service", "add", "web", "--image", "nginx:alpine"],
    );
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.yaml"),
        "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - tcp: 127.0.0.1:1\n        machine_id: 0123456789abcdef0123456789abcdef\n        name: machine-1\n",
    )
    .unwrap();
    (store, home)
}

/// Run `ployz ARGS` from `home`; returns (stdout, stderr).
fn run_in(store: &Target, home: &tempfile::TempDir, args: &[&str]) -> (String, String) {
    run_in_exiting(store, home, args, 0)
}

/// [`run_in`], expecting exit `code`.
fn run_in_exiting(
    store: &Target,
    home: &tempfile::TempDir,
    args: &[&str],
    code: i32,
) -> (String, String) {
    let output = store.command(home.path()).args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(code), "{args:?}: {output:?}");
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn status_names_the_server_that_isnt_answering() {
    let (store, home) = no_server_answering();
    // No Server answered, so the result is partial.
    let (status, _) = run_in_exiting(&store, &home, &["--json", "status"], 3);
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["attention"][0],
        json!({
            "reason": "no_server_answers",
            "message": "Server machine-1 isn't answering. Check that its Ployz daemon is running.",
            "servers": ["machine-1"],
        }),
        "{status}"
    );
    let (shown, _) = run_in_exiting(&store, &home, &["status"], 3);
    assert!(
        shown.contains("needs attention = Server machine-1 isn't answering."),
        "{shown}"
    );

    // With no Server to ask, status has nothing to say about Servers.
    let unasked = ok(&store, &["status"]);
    assert_eq!(unasked["attention"], json!([]), "{unasked}");
}

#[test]
fn status_leaves_a_local_socket_to_this_servers_row() {
    let (store, home) = no_server_answering();
    let socket = home.path().join("missing.sock");
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - unix: {}\n        machine_id: 0123456789abcdef0123456789abcdef\n        name: machine-1\n",
            socket.display()
        ),
    )
    .unwrap();
    // No row, but status warns it couldn't check, so the result is partial.
    let (status, _) = run_in_exiting(&store, &home, &["--json", "status"], 3);
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["attention"], json!([]), "{status}");

    let connect = format!("unix://{}", socket.display());
    let (status, _) = run_in_exiting(
        &store,
        &home,
        &["--json", "status", "--connect", &connect],
        3,
    );
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["attention"], json!([]), "{status}");
}

#[test]
fn status_leaves_a_local_socket_cloud_hands_out_alone() {
    let (store, home) = no_server_answering();
    std::fs::remove_file(home.path().join("config.yaml")).unwrap();
    let socket = home.path().join("missing.sock");
    let access = json!({
        "connections": [{ "machine_id": "0123456789abcdef0123456789abcdef", "unix": socket }],
        "unreachable": [],
        "names": { "0123456789abcdef0123456789abcdef": "machine-1" },
    });
    let (cloud, _) = server_access(access, std::time::Duration::ZERO);
    let output = store
        .command(home.path())
        .env("PLOYZ_TOKEN", "ployz_alice")
        .env("PLOYZ_CLOUD_URL", &cloud)
        .args(["--json", "status"])
        .output()
        .unwrap();
    // It warns that it couldn't check the Server, so the result is partial.
    assert_eq!(output.status.code(), Some(3), "{output:?}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["attention"], json!([]), "{status}");
}

/// A Cloud that answers every `/api/cli/server-access` with `access` after `delay`, as
/// one setting up a holder on each Server first does, and counts the asks.
fn server_access(
    access: Value,
    delay: std::time::Duration,
) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let access = access.to_string();
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let cloud = format!("http://{}", listener.local_addr().unwrap());
    let counted = asked.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line.starts_with("POST /api/cli/server-access ") {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            // real-wait: a slow Cloud reply is what the CLI process must wait out.
            std::thread::sleep(delay);
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{access}",
                access.len()
            )
            .unwrap();
        }
    });
    (cloud, asked)
}

#[test]
fn status_asks_cloud_for_server_access_once() {
    let access = json!({ "connections": [{ "tcp": engine() }], "unreachable": [] });
    // A returning credential, then a first use that waits on Cloud's holder setup.
    for delay in [0, 7].map(std::time::Duration::from_secs) {
        let (store, home) = no_server_answering();
        std::fs::remove_file(home.path().join("config.yaml")).unwrap();
        let (cloud, asked) = server_access(access.clone(), delay);
        let output = store
            .command(home.path())
            .env("PLOYZ_TOKEN", "ployz_alice")
            .env("PLOYZ_CLOUD_URL", &cloud)
            .args(["--json", "status"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{delay:?}: {output:?}");
        assert_eq!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "{delay:?}: {output:?}"
        );
    }
}

#[test]
fn status_tries_each_silent_server_once_within_its_bound() {
    let (store, home) = no_server_answering();
    // Each accepts and holds the connection, never answering.
    let silent = || {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let accepted = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = accepted.clone();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming() {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(stream.unwrap());
            }
        });
        (address, accepted)
    };
    let (one, two) = (silent(), silent());
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - tcp: {}\n        machine_id: 0123456789abcdef0123456789abcdef\n        name: machine-1\n      - tcp: {}\n        machine_id: 1123456789abcdef0123456789abcdef\n        name: machine-2\n",
            one.0, two.0
        ),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let (status, _) = run_in_exiting(&store, &home, &["--json", "status"], 3);
    let elapsed = started.elapsed();
    let status: Value = serde_json::from_str(&status).unwrap();
    assert_eq!(
        status["attention"],
        json!([{
            "reason": "no_server_answers",
            "message": "No Server is answering. Check that the Ployz daemon is running on your Servers.",
            "servers": [],
        }]),
        "{status}"
    );
    // Each silent Server's 5s connect confirm, and slack for the process.
    assert!(elapsed < std::time::Duration::from_secs(12), "{elapsed:?}");
    for (address, accepted) in [one, two] {
        let accepted = accepted.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(accepted, 1, "{address}: {accepted} connections");
    }
}

#[test]
fn status_keeps_a_down_server_past_a_slow_first_use_and_routes_read() {
    let (store, home) = no_server_answering();
    std::fs::remove_file(home.path().join("config.yaml")).unwrap();
    // web-2 is Down, and web-1 serves one In copy of web, so status reads its routes.
    let (sent, engine) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let mut service = engine::DiscoveryService::new(engine::test_description());
            let mut down = engine::machine('c', "web-2");
            down.membership = ployz_core::MembershipObservation::Down;
            service.machines = vec![engine::machine('a', "web-1"), down];
            service.routes_delay = std::time::Duration::from_secs(9);
            let copy =
                ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
                    rotation: Some(ployz_core::CopyRotation::In),
                    r#gen: None,
                    container_id: ployz_core::ContainerId::parse("1".repeat(64)).unwrap(),
                    display_name: "web-1".into(),
                    created_at_unix_nanos: 1,
                    machine_id: engine::machine_id('a'),
                    namespace: ployz_core::Namespace::parse("shop-production").unwrap(),
                    kind: ployz_core::ContainerKind::ServiceContainer,
                    runtime: ployz_core::ContainerRuntimeObservation::Running {
                        health: ployz_core::HealthObservation::Healthy,
                    },
                    effective_healthcheck: None,
                    resolved_spec: serde_json::from_value(json!({
                        "service_id": "b".repeat(32),
                        "name": "web",
                        "mode": { "mode": "replicated", "replicas": 1 },
                        "container": { "image": "nginx:alpine", "pull_policy": "missing" }
                    }))
                    .unwrap(),
                    address: Some(ployz_core::ContainerAddress("10.210.10.2".parse().unwrap())),
                    labels: Default::default(),
                    stopped: false,
                    run: None,
                })
                .unwrap();
            service.listed_containers.lock().unwrap().push(copy);
            let (address, server) = engine::serve_discovery(service).await;
            sent.send(address).unwrap();
            server.await
        })
    });
    let access = json!({ "connections": [{ "tcp": engine.recv().unwrap() }], "unreachable": [] });
    // A first use that waits on Cloud's holder setup; together over 15s, each stage
    // inside its own timeout.
    let (cloud, asked) = server_access(access, std::time::Duration::from_secs(7));
    let output = store
        .command(home.path())
        .env("PLOYZ_TOKEN", "ployz_alice")
        .env("PLOYZ_CLOUD_URL", &cloud)
        .args(["--json", "status"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        status["attention"],
        json!([{
            "reason": "machine_down",
            "message": "Down. Its copies serve again once it's back.",
            "machine": "web-2",
        }]),
        "{status}"
    );
    assert_eq!(status.get("warnings"), None, "{status}");
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn the_plan_warns_the_deploy_cant_run_while_no_server_answers() {
    let (store, home) = no_server_answering();
    let warning = "Server machine-1 isn't answering, so this deploy can't run until it does. \
                   Check that its Ployz daemon is running.";
    let (planned, _) = run_in(&store, &home, &["--json", "deploy", "--plan"]);
    let planned: Value = serde_json::from_str(&planned).unwrap();
    assert_eq!(planned["warnings"], json!([warning]), "{planned}");
    let (_, said) = run_in(&store, &home, &["deploy", "--plan"]);
    assert!(said.starts_with(&format!("! {warning}\n")), "{said}");

    // With no Server to ask, the plan has nothing to warn of.
    let unasked = ok(&store, &["deploy", "--plan"]);
    assert_eq!(unasked.get("warnings"), None, "{unasked}");
}

#[test]
fn discarding_a_new_services_image_points_at_discarding_the_service() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(
        &store,
        &["service", "add", "web", "--image", "traefik/whoami:v1.10.0"],
    );
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap()
    };
    let row = "web.image: null → \"traefik/whoami:v1.10.0\"";

    let refused = human(&["discard", "web.image"]);
    assert!(
        refused.contains(
            "web.image is what the new service web runs; it has no Discard of its own. Discard the service: ployz discard web"
        ),
        "{refused}"
    );
    assert!(human(&["diff"]).contains(row));
    let nothing = human(&["discard", "web.env.PORT"]);
    assert!(
        nothing.contains("Nothing staged at web.env.PORT in shop/production; nothing to discard."),
        "{nothing}"
    );

    human(&["discard", "web"]);
    let diff = human(&["diff"]);
    assert!(!diff.contains(row), "{diff}");
    assert_eq!(ok(&store, &["diff"]).get("changes"), Some(&json!([])));
}

#[test]
fn removing_a_referenced_service_stages_and_prints_the_broken_reference() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "redis", "--image", "redis:7"]);
    ok(&store, &["service", "add", "sentry", "--image", "sentry:1"]);
    ok(
        &store,
        &["set", "sentry.env.REDIS=${{ redis.PLOYZ_PRIVATE_DOMAIN }}"],
    );
    ok(&store, &["deploy", "--skip-deploy"]);
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap()
    };
    let line = "sentry.env.REDIS references redis, which was removed; deploys skip it until you change it: ployz set 'sentry.env.REDIS=VALUE'";
    let removed = human(&["service", "rm", "redis"]);
    assert!(removed.contains(line), "{removed}");
    let diff = human(&["diff"]);
    assert!(diff.contains(line), "{diff}");
    let status = ok(&store, &["status"]);
    assert_eq!(
        status["attention"][0]["reason"],
        json!("broken_reference"),
        "{status}"
    );
}

#[test]
fn a_broken_reference_in_an_invisible_file_name_prints_escaped() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "host=${{ redis.PLOYZ_PRIVATE_DOMAIN }}").unwrap();
    let from = file.path().to_str().unwrap();
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "redis", "--image", "redis:7"]);
    ok(&store, &["service", "add", "sentry", "--image", "sentry:1"]);
    ok(
        &store,
        &["config", "add", "app", "--mount", "sentry:/etc/app"],
    );
    ok(
        &store,
        &["config", "put", "app", "a\u{202E}b.txt", "--from", from],
    );
    ok(&store, &["deploy", "--skip-deploy"]);
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap() + &String::from_utf8(output.stderr).unwrap()
    };
    // The name shows as stored, and the fix command still pastes to that file.
    let line = r"app/a\u{202e}b.txt references redis, which was removed; deploys skip it until you change it: ployz config put app $'a\xe2\x80\xaeb.txt' --from PATH";
    let removed = human(&["service", "rm", "redis"]);
    assert!(removed.contains(line), "{removed}");
    let diff = human(&["diff"]);
    assert!(diff.contains(line), "{diff}");
    let status = human(&["status"]);
    assert!(
        status.contains(r"app/a\u{202e}b.txt references redis"),
        "{status}"
    );
    for shown in [removed, diff, status] {
        assert!(!shown.contains('\u{202E}'), "{shown}");
    }
    let json = ok(&store, &["status"]);
    assert_eq!(
        json["attention"][0]["message"],
        json!("app/a\u{202E}b.txt references redis, which was removed"),
        "{json}"
    );
}

#[test]
fn an_agent_renames_a_project() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["project", "new", "blog"]);
        let renamed = ok(store, &["project", "rename", "shop", "store"]);
        assert_eq!(renamed["project"]["name"], json!("store"));
        assert_eq!(renamed["links"], json!(0));
        let listed = ok(store, &["project", "ls"]);
        assert_eq!(listed["projects"][1]["name"], json!("store"));
        let taken = error(store, &["project", "rename", "blog", "store"]);
        assert_eq!(taken["code"], json!("conflict"));
        let missing = error(store, &["project", "rename", "nope", "other"]);
        assert_eq!(missing["code"], json!("not_found"));
    }
}

#[test]
fn an_agent_adds_checks_and_removes_domains() {
    for store in &targets() {
        // Only Cloud reserves the Cluster Domain a generated domain lives under; the local
        // Store refuses one (`standalone_refuses_a_generated_domain`), so there web's
        // first domain is a hostname it owns.
        let cloud = matches!(store, Target::Cloud { .. });
        let (domain, hostname) = if cloud {
            ("web", "web.acme.ployz.app")
        } else {
            ("web.example.com", "web.example.com")
        };
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);

        let add = if cloud {
            ["domain", "add", "web", "--port", "8080"].to_vec()
        } else {
            ["domain", "add", "web", hostname, "--port", "8080"].to_vec()
        };
        let added = ok(store, &add);
        let (kind, prefix) = if cloud {
            ("generated", json!("web"))
        } else {
            ("custom", json!(null))
        };
        assert_eq!(added["domain"]["kind"], json!(kind));
        assert_eq!(added["domain"]["prefix"], prefix);
        assert_eq!(added["domain"]["hostname"], json!(hostname));
        assert_eq!(added["staged"], json!(["web"]));
        assert_eq!(added["next"], json!("ployz deploy"));
        let again = ok(store, &add);
        assert_eq!((&again["staged"], again.get("next")), (&json!([]), None));

        if cloud {
            let set = ok(store, &["domain", "set", "web", "Shop"]);
            assert_eq!(set["domain"]["prefix"], json!("shop"));
            assert_eq!(set["staged"], json!(["web"]));
            assert_eq!(set["next"], json!("ployz deploy"));
            ok(store, &["service", "add", "api", "--image", "api:1"]);
            ok(store, &["domain", "add", "api"]);
            let taken = error(store, &["domain", "set", "api", "shop"]);
            assert_eq!(taken["code"], json!("conflict"));
            ok(store, &["domain", "set", "web", "web"]);
        } else {
            let none = error(store, &["domain", "set", "web", "shop"]);
            assert_eq!(none["code"], json!("not_found"));
        }
        let label = failed(store, &["domain", "set", "web", "not.one-label"], 2);
        assert!(!label["message"].as_str().unwrap().contains("not.one-label"));

        assert_eq!(
            ok(store, &["domain", "add", "web", "App.Example.com"])["domain"]["hostname"],
            json!("app.example.com")
        );
        let usage = failed(store, &["domain", "add", "web", "not a host"], 2);
        assert!(!usage["message"].as_str().unwrap().contains("not a host"));

        // `SERVICE.domains` reads like a Setting; the domain commands change it.
        let got = ok(store, &["get", "web.domains"]);
        assert_eq!(got["settings"], json!([]));
        let mut hostnames = got["domains"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["prefix"].as_str().or(row["hostname"].as_str()))
            .collect::<Vec<_>>();
        hostnames.sort_unstable();
        assert_eq!(hostnames, [Some("app.example.com"), Some(domain)]);
        let explained = ok(store, &["explain", "web.domains"]);
        assert_eq!(explained["path"], json!("web.domains"));
        assert_eq!(explained["schema"]["readOnly"], json!(true));
        assert_eq!(explained["example"], json!("ployz domain add web"));
        let edits = [
            &["set", "web.domains=shop"][..],
            &["unset", "web.domains"],
            &["discard", "web.domains"],
        ];
        for edit in edits {
            let refused = error(store, edit);
            assert_eq!(refused["code"], json!("invalid_argument"), "{edit:?}");
            assert_eq!(
                refused["details"]["next"],
                json!("ployz domain add web"),
                "{edit:?}"
            );
        }
        // web's first domain reads as its full hostname.
        let added_line = format!("added {hostname} on port 8080");
        let reads = [
            ok(store, &["diff"]),
            ok(store, &["deploy", "--plan"]),
            ok(store, &["service", "inspect", "web"]),
        ];
        for read in reads {
            let changes = read["domain_changes"].as_array().unwrap();
            for change in ["added app.example.com", &added_line] {
                let line = json!({ "path": "web.domains", "change": change });
                assert!(changes.contains(&line), "{read}");
            }
        }
        let planned = ok(store, &["deploy", "--plan"]);
        let unchecked = planned["ingress_unchecked"]
            .as_array()
            .is_some_and(|causes| !causes.is_empty());
        assert_eq!(
            (planned["ingress"].is_array(), unchecked),
            (cloud, !cloud),
            "{planned}"
        );
        let unreached = ok(
            store,
            &[&["deploy", "--plan"][..], &UNREACHABLE_SERVER].concat(),
        );
        let cause = unreached["ingress_unchecked"][0]
            .as_str()
            .unwrap_or_default();
        assert!(
            cause.starts_with(
                "Ployz couldn't reach your Server's daemon: Couldn't open a connection to the Server: "
            ) && cause.contains("refused"),
            "{unreached}"
        );

        let listed = ok(store, &["domain", "ls"]);
        let first = &listed["domains"][0];
        assert_eq!(first["status"], json!("setting_up"));
        assert_eq!(first["action"], json!({ "type": "deploy" }));
        assert_eq!(listed["next"], json!("ployz deploy"));
        let checked = ok(store, &["domain", "check", domain]);
        assert_eq!(
            checked["domain"]["reason"],
            json!("Live after your next deploy")
        );
        let missing = error(store, &["domain", "check", "nope"]);
        assert_eq!(missing["code"], json!("not_found"));

        let removed = ok(store, &["domain", "rm", domain]);
        assert_eq!(removed["domain"]["port"], json!(8080));
        assert_eq!(removed["next"], json!("ployz deploy"));
    }
}

#[test]
fn domain_add_explains_invalid_custom_hostnames() {
    let long_label = format!("{}.example.com", "a".repeat(64));
    let long_name = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(62)
    );
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx"]);
        for (host, reason) in [
            ("1.2.3.4", "IP address"),
            ("127.1", "IP address"),
            ("1.2.3.4.5", "IP address"),
            ("-foo.com", "start or end with a hyphen"),
            ("foo..com", "empty label"),
            ("foo.-bar.com", "start or end with a hyphen"),
            ("2001:db8::1", "IP address"),
            ("[::1]", "IP address"),
            ("ployz.app", "reserved"),
            ("a.ployz.app", "reserved"),
            ("xn--zz.com", "punycode"),
            ("*.example.com", "Add each hostname"),
            (long_label.as_str(), "label is too long"),
            (long_name.as_str(), "hostname is too long"),
            ("https://x.example.com", "scheme"),
            ("host.com:8080", "port"),
            ("x.example.com/path", "path"),
            ("under_score.example.com", "underscore"),
        ] {
            let refused = failed(store, &["domain", "add", "web", "--", host], 2);
            assert_eq!(
                refused["code"],
                json!("invalid_argument"),
                "{host}: {refused}"
            );
            assert!(
                refused["message"].as_str().unwrap().contains(reason),
                "{host}: {refused}"
            );
        }
        assert_eq!(ok(store, &["get", "web.domains"])["domains"], json!([]));
    }
}

#[test]
fn domain_add_normalizes_a_trailing_root_dot() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx"]);
        for (typed, stored) in [
            ("App.Example.com.", "app.example.com"),
            ("пример.рф.", "xn--e1afmkfd.xn--p1ai"),
            ("xn--e1afmkfd.xn--p1ai", "xn--e1afmkfd.xn--p1ai"),
        ] {
            assert_eq!(
                ok(store, &["domain", "add", "web", typed])["domain"]["hostname"],
                json!(stored)
            );
        }
    }
}

#[test]
fn standalone_refuses_a_generated_domain() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let store = &store;
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "nginx:1"]);

    // Without Ployz Cloud no Cluster Domain is ever reserved, so a generated domain
    // could never deploy: it's refused before anything is staged, naming a custom one.
    for add in [
        &["domain", "add", "web"][..],
        &["domain", "add", "web", "--prefix", "shop", "--port", "8080"],
    ] {
        let refused = error(store, add);
        assert_eq!(refused["code"], json!("unsupported"), "{add:?}");
        assert_eq!(
            refused["message"],
            json!(
                "Generated domains need Ployz Cloud, which reserves the Cluster Domain they live \
                 under: give web a hostname you own instead"
            ),
            "{add:?}"
        );
        assert_eq!(
            refused["details"]["next"],
            json!("ployz domain add web HOST"),
            "{add:?}"
        );
    }
    assert_eq!(ok(store, &["get", "web.domains"])["domains"], json!([]));
    // A Service that isn't there is named missing, not given a hostname.
    let missing = error(store, &["domain", "add", "nope"]);
    assert_eq!(missing["code"], json!("not_found"), "{missing}");
    let home = tempfile::tempdir().unwrap();
    let (code, stderr) = person(store, home.path(), &["domain", "add", "web"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("next: ployz domain add web HOST"),
        "{stderr}"
    );

    let custom = ok(store, &["domain", "add", "web", "app.example.com"]);
    assert_eq!(custom["domain"]["hostname"], json!("app.example.com"));

    // A context connection that knows its Server names it, and says why in a few words.
    let home = tempfile::tempdir().unwrap();
    let machine = "0123456789abcdef0123456789abcdef";
    std::fs::write(
        home.path().join("config.yaml"),
        format!(
            "current_context: lab\ncontexts:\n  lab:\n    connections:\n      - tcp: 127.0.0.1:1\n        machine_id: {machine}\n"
        ),
    )
    .unwrap();
    let output = store
        .command(home.path())
        .args(["--json", "deploy", "--plan"])
        .output()
        .unwrap();
    let planned: Value = serde_json::from_slice(&output.stdout).unwrap();
    let cause = planned["ingress_unchecked"][0].as_str().unwrap_or_default();
    assert!(
        cause.starts_with(&format!("Server {machine} isn't answering ("))
            && cause.contains("refused")
            && !cause.contains("retry")
            && !cause.contains("Machine"),
        "{planned}"
    );
}

#[test]
fn a_standalone_deploy_names_the_generated_domains_to_remove() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let Target::Local(dir) = &store else {
        unreachable!("a local Store")
    };
    let store = &store;
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "nginx:1"]);
    ok(store, &["service", "add", "api", "--image", "api:1"]);
    // Generated domains staged before the CLI refused them on a local Store.
    let local = ConfigStore::open(
        &format!("sqlite:{}", dir.path().join("store.db").display()),
        SealingKey::from_file(&dir.path().join("store.db.key")).unwrap(),
    )
    .unwrap();
    let who = Actor::system(OrganizationId::parse("local").unwrap());
    for (service, prefix) in [("web", "web"), ("web", "shop")] {
        local
            .write_trusted(
                &who,
                &ployz_store::AddDomain {
                    environment: ployz_store::EnvironmentRef::default(),
                    service: ployz_core::ServiceName::parse(service).unwrap(),
                    hostname: None,
                    prefix: Some(ployz_core::DomainPrefix::parse(prefix).unwrap()),
                    port: None,
                },
                &Trusted::default(),
            )
            .unwrap();
    }
    drop(local);
    // A generated domain reads under a placeholder until a Cluster Domain is known.
    let changes = ok(store, &["diff"])["domain_changes"].clone();
    assert!(
        changes
            .as_array()
            .unwrap()
            .contains(&json!({ "path": "web.domains", "change": "added web.<cluster domain>" })),
        "{changes}"
    );

    // Every Deploy refuses, even of another Service, and names what removes them.
    let rm = "ployz domain rm web --project shop --env production \
              && ployz domain rm shop --project shop --env production";
    for deploy in [&["deploy"][..], &["deploy", "api"]] {
        let refused = error(store, deploy);
        assert_eq!(refused["code"], json!("unsupported"), "{deploy:?}");
        assert_eq!(
            refused["message"],
            json!(
                "production has generated domains (web, shop), which need the Cluster Domain \
                 Ployz Cloud reserves at a Deploy: deploy through Ployz Cloud, or remove them"
            ),
            "{deploy:?}"
        );
        assert_eq!(refused["details"]["domains"], json!(["web", "shop"]));
        assert_eq!(refused["details"]["next"], json!(rm), "{deploy:?}");
    }

    ok(store, &["domain", "rm", "web"]);
    ok(store, &["domain", "rm", "shop"]);
    let deployed = no_server_answers(store, &["deploy"]);
    assert_ne!(
        deployed["error"]["code"],
        json!("unsupported"),
        "{deployed}"
    );
}

#[test]
fn adding_a_custom_domain_tells_a_person_the_dns_records_to_create() {
    for store in &targets() {
        let cloud = matches!(store, Target::Cloud { .. });
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let home = tempfile::tempdir().unwrap();
        let (code, stderr) = person(
            store,
            home.path(),
            &["domain", "add", "web", "app.example.com"],
        );
        assert_eq!(code, Some(0), "{stderr}");
        let record = "Add DNS for app.example.com: CNAME app acme.ployz.app\n";
        // Only Cloud knows the Cluster Domain the CNAME points at.
        assert_eq!(stderr.contains(record), cloud, "{stderr}");
        if cloud {
            assert!(
                stderr.find(record) < stderr.find("ployz deploy"),
                "the records come before the deploy that needs them: {stderr}"
            );
        }
        // Waiting on a deploy, the domain still lists the records, as the dashboard offers them.
        let (code, listed) = person(store, home.path(), &["domain", "ls"]);
        assert_eq!(code, Some(0), "{listed}");
        assert_eq!(listed.contains(record), cloud, "{listed}");
    }
}

#[test]
fn an_agent_lists_moves_the_default_and_removes_environments_without_servers() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "new", "staging"]);
        let listed = ok(store, &["env", "ls"]);
        assert_eq!(listed["environments"][0]["name"], json!("production"));
        assert_eq!(listed["environments"][0]["default"], json!(true));

        // Branches of production run a default Setup Command until it is cleared.
        let set = ok(store, &["env", "setup", "--setup", "web=pnpm db:seed"]);
        assert_eq!(
            set["environments"][0]["branch_setup"],
            json!([{ "service": "web", "command": "pnpm db:seed" }])
        );
        let cleared = ok(store, &["env", "setup", "--clear"]);
        assert_eq!(cleared["environments"][0]["branch_setup"], json!([]));
        failed(store, &["env", "setup"], 2);

        let unconfirmed = error(store, &["env", "rm", "production"]);
        assert_eq!(unconfirmed["code"], json!("confirmation_required"));
        assert_eq!(unconfirmed["details"]["services"], json!(["web"]));
        assert_eq!(
            unconfirmed["details"]["retry"],
            json!("ployz env rm production --confirm shop/production")
        );
        let mistyped = error(
            store,
            &["env", "rm", "production", "--confirm", "production"],
        );
        assert_eq!(
            mistyped["details"]["retry"],
            json!("ployz env rm production --confirm shop/production")
        );
        let default = error(
            store,
            &["env", "rm", "production", "--confirm", "shop/production"],
        );
        assert_eq!(default["code"], json!("unsupported"));
        assert_eq!(
            default["details"]["next"],
            json!("ployz env default ENV --project shop")
        );

        let moved = ok(store, &["env", "default", "staging"]);
        assert_eq!(moved["environments"][1]["default"], json!(true));
        let protected = error(
            store,
            &["env", "rm", "production", "--confirm", "shop/production"],
        );
        assert_eq!(protected["code"], json!("unsupported"));
        assert_eq!(
            protected["details"]["next"],
            json!("ployz env protect production --off --project shop")
        );
        let unprotected = ok(store, &["env", "protect", "production", "--off"]);
        assert_eq!(unprotected["environments"][0]["protected"], json!(false));
        // Nothing of production ever ran, so it goes without a Server.
        let removed = ok(
            store,
            &["env", "rm", "production", "--confirm", "shop/production"],
        );
        assert_eq!(removed["environment"]["name"], json!("production"));
        assert_eq!(removed["removal"], json!("removed"));
        assert_eq!(removed["deployments"], json!([]));
        let listed = ok(store, &["env", "ls"]);
        assert_eq!(listed["environments"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn an_environment_cloud_forgets_and_sweeps_first_still_says_it_was_left_on_old_servers() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        if let Written::Deployment(admitted) = written
            && !admitted.remove
        {
            succeed(store, &admitted.id);
        }
        // Before the CLI reads its removal Deployment, Cloud loses the last Server,
        // admits the removal again as Forgotten and sweeps the Environment away.
        if let Written::Removal(Removal::Removing { .. }) = written {
            let organization = OrganizationId::parse("alice").unwrap();
            let who = Actor::system(organization.clone());
            let none_left = Trusted {
                servers: Some(0),
                ..Trusted::default()
            };
            store
                .system(
                    &organization,
                    &ployz_store::SystemEvent::ClusterForgotten,
                    &none_left,
                )
                .unwrap();
            let forgotten = store
                .write_trusted(
                    &who,
                    &ployz_store::Admit::Remove(ployz_store::AdmitRemoval {
                        id: ployz_store::DeploymentId::parse(uuid::Uuid::new_v4().to_string())
                            .unwrap(),
                        environment: ployz_store::EnvironmentRef {
                            project: Some(ployz_store::ProjectName::parse("shop").unwrap()),
                            environment: Some(
                                ployz_store::EnvironmentName::parse("staging").unwrap(),
                            ),
                        },
                    }),
                    &none_left,
                )
                .unwrap();
            assert_eq!(forgotten.outcome, Some(ployz_store::Outcome::Forgotten));
            store
                .system(
                    &organization,
                    &ployz_store::SystemEvent::Sweep(ployz_store::Sweep { now: 0 }),
                    &none_left,
                )
                .unwrap();
        }
    }
    let cloud = Target::Cloud {
        url: fake_cloud_with_dispatch(worker),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["env", "new", "staging"]);
    ok(
        &cloud,
        &[
            "service",
            "add",
            "web",
            "--image",
            "nginx:alpine",
            "--env",
            "staging",
        ],
    );
    ok(&cloud, &["deploy", "--env", "staging"]);

    // The typed name confirms removing what ran: no version to pass.
    let removed = ok(
        &cloud,
        &["env", "rm", "staging", "--confirm", "shop/staging"],
    );
    assert_eq!(removed["removal"], "removed", "{removed}");
    assert_eq!(
        removed["residuals"],
        json!([{ "environment": "staging", "services": ["web"] }]),
        "{removed}"
    );
}

#[test]
fn a_project_removal_on_cloud_takes_off_what_it_can_and_leaves_the_rest_to_cloud() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        if let Written::Deployment(admitted) = written {
            // Keep the second authored Deployment queued, so production can't come off yet.
            if admitted.number != Some(2) || admitted.remove {
                succeed(store, &admitted.id);
            }
        }
        if let Written::Removal(Removal::Removing { left, .. }) = written {
            for deployment in left.iter().filter_map(|left| left.deployment.as_ref()) {
                succeed(store, &deployment.id);
            }
        }
    }
    let cloud = Target::Cloud {
        url: fake_cloud_with_dispatch(worker),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["env", "new", "before-root"]);
    for environment in ["production", "before-root"] {
        ok(
            &cloud,
            &[
                "service",
                "add",
                "web",
                "--image",
                "nginx:alpine",
                "--env",
                environment,
            ],
        );
        ok(&cloud, &["deploy", "--env", environment]);
    }
    ok(&cloud, &["set", "web.cpuLimit=0.2", "--env", "production"]);
    ok(&cloud, &["deploy", "--env", "production", "--detach"]);

    // What ran is destructive to remove, and the typed name alone confirms it.
    let removed = ok(&cloud, &["project", "rm", "shop", "--confirm", "shop"]);
    assert_eq!(removed["removal"], "removing", "{removed}");
    assert_eq!(
        removed["deployments"].as_array().unwrap().len(),
        1,
        "{removed}"
    );
    assert_eq!(removed["deployments"][0]["status"], "applied");
    assert_eq!(removed["deployments"][0]["remove"], true);
    let production = removed["left"]
        .as_array()
        .unwrap()
        .iter()
        .find(|left| left["environment"]["name"] == "production")
        .unwrap();
    assert_eq!(production["deployment"], json!(null), "{removed}");
    // Cloud finishes it: the Project stays listed until production comes off.
    assert_eq!(
        ok(&cloud, &["project", "ls"])["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

/// Deployment #1 queued while Cloud had a Server; the last one left before its worker
/// claimed it. Starting it refuses at once, as a Deploy does, instead of following a
/// Deployment nothing will run.
#[test]
fn starting_a_queued_deployment_with_no_server_left_refuses_at_once() {
    fn unclaimed(_: &std::sync::Arc<ConfigStore>, _: &Written) {}
    fn none_left_to_start(command: &StoreCommand) -> Trusted {
        let servers = if matches!(command, StoreCommand::Start(_)) {
            0
        } else {
            1
        };
        Trusted {
            servers: Some(servers),
            ..evidence()
        }
    }
    let cloud = Target::Cloud {
        url: cloud(
            unclaimed,
            std::sync::Arc::default(),
            none_left_to_start,
            ready(),
        ),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(
        &cloud,
        &["service", "add", "web", "--image", "nginx:alpine"],
    );
    ok(&cloud, &["deploy", "--detach"]);

    let home = tempfile::tempdir().unwrap();
    let mut started = cloud
        .command(home.path())
        .args(["--json", "deployment", "start", "1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while started.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= deadline {
            started.kill().unwrap();
            panic!("deployment start was still following a Deployment no Server can run");
        }
        // real-wait: polls the CLI child, which signals nothing before it exits.
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let output = started.wait_with_output().unwrap();
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(1), "{json}");
    assert_eq!(json["error"]["code"], json!("conflict"), "{json}");
    assert_eq!(
        json["error"]["message"],
        json!(
            "This Organization has no servers yet, so nothing deployed. Add one, then deploy again."
        )
    );
    assert_eq!(
        json["error"]["details"]["next"],
        json!("ployz server add"),
        "{json}"
    );
}

#[test]
fn an_agent_lists_and_removes_a_project_without_servers() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "new", "staging"]);
        ok(store, &["project", "new", "blog"]);
        let listed = ok(store, &["project", "ls"]);
        assert_eq!(listed["projects"][1]["name"], json!("shop"));
        assert_eq!(
            listed["projects"][1]["default_environment"],
            json!("production")
        );
        assert_eq!(
            listed["projects"][1]["environments"],
            json!(["production", "staging"])
        );

        let unconfirmed = error(store, &["project", "rm", "shop"]);
        assert_eq!(unconfirmed["code"], json!("confirmation_required"));
        assert_eq!(
            unconfirmed["details"]["environments"][0]["services"],
            json!(["web"])
        );
        assert_eq!(
            unconfirmed["details"]["retry"],
            json!("ployz project rm shop --confirm shop")
        );
        let mistyped = error(store, &["project", "rm", "shop", "--confirm", "blog"]);
        assert_eq!(
            mistyped["details"]["retry"],
            json!("ployz project rm shop --confirm shop")
        );

        // Nothing of it ever ran, so it goes at once, with no Server.
        let removed = ok(store, &["project", "rm", "shop", "--confirm", "shop"]);
        assert_eq!(removed["project"]["name"], json!("shop"));
        assert_eq!(removed["environments"], json!(["production", "staging"]));
        assert_eq!(removed["removal"], json!("removed"));
        assert_eq!(removed["deployments"], json!([]));
        let listed = ok(store, &["project", "ls"]);
        assert_eq!(listed["projects"].as_array().unwrap().len(), 1);
        assert_eq!(
            error(store, &["project", "rm", "shop", "--confirm", "shop"])["code"],
            json!("not_found")
        );
    }
}

#[test]
fn an_agent_reads_pr_plans_and_names_the_repository_to_change() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        // No Service deploys from a GitHub repository: no plans, nothing to name.
        let listed = ok(store, &["env", "pr"]);
        assert_eq!(listed["project"]["name"], json!("shop"));
        assert_eq!(listed["plans"], json!([]));
        failed(store, &["env", "pr", "--on"], 2);
        let missing = error(store, &["env", "pr", "acme/web", "--on"]);
        assert_eq!(missing["code"], json!("not_found"));
    }
}

#[test]
fn an_agent_branches_an_environment_without_servers() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["service", "add", "db", "--image", "postgres:17"]);
        ok(
            store,
            &["set", "web.env.DB_URL=${{ db.PLOYZ_PRIVATE_DOMAIN }}"],
        );

        // production never deployed, so nothing can lend db: the Branch copies it too.
        let refused = error(
            store,
            &["env", "branch", "fix-web", "--copy", "web", "--live", "db"],
        );
        assert_eq!(refused["code"], json!("invalid_argument"));
        failed(
            store,
            &[
                "env", "branch", "fix-web", "--copy", "web", "--setup", "seed",
            ],
            2,
        );
        let made = ok(
            store,
            &[
                "env",
                "branch",
                "fix-web",
                "--from",
                "production",
                "--copy",
                "web",
                "--setup",
                "web=pnpm db:seed",
                "--keep",
            ],
        );
        assert_eq!(made["staged"], json!(["db", "web"]));
        assert_eq!(made["next"], json!("ployz deploy --env fix-web"));
        assert_eq!(made["branch"]["parent"], json!("production"));
        assert_eq!(made["branch"]["kept"], json!(true));
        assert_eq!(
            made["branch"]["setup"],
            json!([{ "service": "web", "command": "pnpm db:seed" }])
        );
        assert_eq!(
            ok(store, &["get", "web.env.DB_URL", "--env", "fix-web"])["settings"][0]["value"],
            json!("${{ db.PLOYZ_PRIVATE_DOMAIN }}")
        );

        assert_eq!(
            error(store, &["env", "copy", "db", "--env", "fix-web"])["code"],
            json!("conflict")
        );
        let unkept = ok(store, &["env", "keep", "--env", "fix-web", "--off"]);
        assert_eq!(unkept["branch"]["kept"], json!(false));
        assert!(unkept.get("next").is_none());

        // A take names the Parent, not an id.
        failed(
            store,
            &["env", "sync", "--undo", "sync", "--only", "web.image"],
            2,
        );
        let take = error(
            store,
            &[
                "env",
                "sync",
                "--take",
                "00000000-0000-4000-8000-000000000099",
                "--env",
                "fix-web",
            ],
        );
        assert_eq!(take["code"], json!("invalid_argument"));
    }
}

#[test]
fn an_agent_marks_settings_never_sync_without_servers() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
        ok(
            store,
            &[
                "set",
                "--env",
                "fix-web",
                "web.image=web:2",
                "web.env.APP_ENV=fix",
            ],
        );
        let marked = ok(
            store,
            &[
                "env",
                "never-sync",
                "web.env.APP_ENV",
                "web.source",
                "--env",
                "fix-web",
            ],
        );
        assert_eq!(marked["environment"]["name"], json!("fix-web"));
        assert_eq!(
            labels(&marked["never_synced"]),
            ["web.env.APP_ENV", "web.source"]
        );
        let mut rows: Vec<&str> = marked["never_synced"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["row"].as_str().unwrap())
            .collect();
        rows.sort_unstable();
        assert_eq!(
            ok(store, &["get", "--env", "fix-web"])["never_synced"],
            json!(rows)
        );

        // The plan lists them apart, so there is nothing to sync.
        let plan = ok(
            store,
            &["env", "sync", "--to", "--plan", "--env", "fix-web"],
        );
        assert_eq!(plan["rows"], json!([]));
        assert_eq!(
            labels(&plan["never_synced"]),
            ["web.source", "web.env.APP_ENV"]
        );
        assert_eq!(
            plan["never_synced"][0]["marks"][0]["environment"],
            json!("fix-web")
        );
        let nothing = error(store, &["env", "sync", "--to", "--env", "fix-web"]);
        assert_eq!(nothing["code"], json!("conflict"));

        let again = ok(
            store,
            &[
                "env",
                "never-sync",
                "web.source",
                "--off",
                "--env",
                "fix-web",
            ],
        );
        assert_eq!(labels(&again["never_synced"]), ["web.env.APP_ENV"]);
        let plan = ok(
            store,
            &["env", "sync", "--to", "--plan", "--env", "fix-web"],
        );
        assert_eq!(label(&plan["rows"][0]), "web.source");

        failed(store, &["env", "never-sync", "--env", "fix-web"], 2);
        // A node's name marks it and each of its rows, changed, unchanged or at its default.
        let whole = ok(store, &["env", "never-sync", "web", "--env", "fix-web"]);
        let whole = labels(&whole["never_synced"]);
        assert_eq!(whole.len(), 19);
        for row in ["web", "web.env.APP_ENV", "web.source", "web.startCommand"] {
            assert!(whole.contains(&row.to_owned()), "{row} in {whole:?}");
        }
        let missing = error(
            store,
            &["env", "never-sync", "api.env.KEY", "--env", "fix-web"],
        );
        assert_eq!(missing["code"], json!("not_found"));
    }
}

#[test]
fn an_agent_syncs_a_branch_into_its_parent_without_servers() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
        let plan_next = json!("ployz env sync --to --plan --env fix-web");
        let nothing = error(store, &["env", "sync", "--to", "--env", "fix-web"]);
        assert_eq!(nothing["code"], json!("conflict"));
        assert_eq!(nothing["details"]["next"], plan_next);
        failed(store, &["env", "sync", "--env", "fix-web"], 2);
        failed(store, &["env", "sync", "--to", "--plan", "--close"], 2);

        ok(
            store,
            &[
                "set",
                "--env",
                "fix-web",
                "web.image=web:2",
                "web.env.DEBUG=1",
                "web.env.NEW=1",
            ],
        );
        let plan = ok(
            store,
            &["env", "sync", "--to", "--plan", "--env", "fix-web"],
        );
        assert_eq!(plan["into"]["name"], json!("production"));
        let rows = plan["rows"].as_array().unwrap();
        assert_eq!(
            labels(&plan["rows"]),
            ["web.source", "web.env.DEBUG", "web.env.NEW"]
        );
        let home = tempfile::tempdir().unwrap();
        let human = store
            .command(home.path())
            .args(["env", "sync", "--to", "--plan", "--env", "fix-web"])
            .output()
            .unwrap();
        assert!(human.status.success());
        let human = String::from_utf8(human.stdout).unwrap();
        assert!(
            human.contains("\n  web (update)\n    web.image: \"web:1\" → \"web:2\"\n"),
            "{human}"
        );
        let image = &rows[0];
        assert_eq!(
            (
                &image["node"],
                &image["from"]["image"],
                &image["into"]["image"]
            ),
            (&json!("web"), &json!("web:2"), &json!("web:1"))
        );
        assert_eq!(
            (&image["ticked"], &image["change"]),
            (&json!(true), &json!("changed"))
        );
        assert!(image["row"].as_str().unwrap().ends_with(":source"));
        let version = plan["version"].as_str().unwrap();
        assert_eq!(
            plan["next"],
            json!(format!(
                "ployz env sync --to --version {version} --env fix-web"
            ))
        );

        let stale = error(
            store,
            &[
                "env",
                "sync",
                "--to",
                "--env",
                "fix-web",
                "--version",
                "0:0",
            ],
        );
        assert_eq!(stale["code"], json!("conflict"));
        assert_eq!(stale["details"]["next"], plan_next);
        let unknown = error(
            store,
            &["env", "sync", "--to", "--env", "fix-web", "--only", "api"],
        );
        assert_eq!(unknown["code"], json!("not_found"));
        assert_eq!(unknown["details"]["next"], plan_next);
        let sideways = error(
            store,
            &["env", "sync", "--to", "fix-web", "--env", "fix-web"],
        );
        assert_eq!(sideways["code"], json!("invalid_argument"));

        // Everything under web but its DEBUG variable, which is offered again.
        let synced = ok(
            store,
            &[
                "env",
                "sync",
                "--to",
                "production",
                "--env",
                "fix-web",
                "--only",
                "web",
                "--skip",
                "web.env.DEBUG",
                "--version",
                version,
            ],
        );
        assert_eq!(
            (&synced["staged"], &synced["closing"]),
            (&json!(["web"]), &json!(false))
        );
        assert!(synced.get("proposal").is_none());
        assert_eq!(synced["next"], json!("ployz deploy --env production"));
        assert_eq!(
            ok(store, &["get", "web.image"])["settings"][0]["value"],
            json!("web:2")
        );
        let plan = ok(
            store,
            &["env", "sync", "--to", "--plan", "--env", "fix-web"],
        );
        assert_eq!(labels(&plan["rows"]), ["web.env.DEBUG"]);

        // --close closes the Branch once it synced; it never ran, so it is gone.
        let closed = ok(
            store,
            &["env", "sync", "--to", "--env", "fix-web", "--close"],
        );
        assert_eq!(closed["closing"], json!(true));
        let listed = ok(store, &["env", "ls"]);
        assert_eq!(listed["environments"].as_array().unwrap().len(), 1);
    }
}

/// `env sync --plan` reads as `ployz diff` does: each node under its name and what
/// happens to it, a Config's files by name, and never a node as raw JSON.
#[test]
fn a_sync_plan_reads_new_nodes_and_config_files_as_diff_does() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let from = file.path().to_str().unwrap();
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
        ok(store, &["config", "put", "app", "app.yml", "--from", from]);
        ok(
            store,
            &[
                "env",
                "branch",
                "fix-web",
                "--copy",
                "web",
                "--copy",
                "configs.app",
            ],
        );
        ok(
            store,
            &[
                "config", "put", "app", "new.yml", "--from", from, "--env", "fix-web",
            ],
        );
        ok(
            store,
            &[
                "service", "add", "api", "--image", "api:1", "--env", "fix-web",
            ],
        );
        ok(store, &["volume", "add", "data", "--env", "fix-web"]);
        ok(store, &["config", "add", "extra", "--env", "fix-web"]);

        let home = tempfile::tempdir().unwrap();
        let human = store
            .command(home.path())
            .args(["env", "sync", "--to", "--plan", "--env", "fix-web"])
            .output()
            .unwrap();
        assert!(human.status.success());
        let human = String::from_utf8(human.stdout).unwrap();
        let (_, rows) = human.split_once('\n').unwrap();
        for block in [
            "  app (update)\n    files: added new.yml (new)\n",
            "  data (create)\n",
            "  api (create)\n    api.image: null → \"api:1\"\n",
            "  extra (create)\n",
        ] {
            assert!(rows.contains(block), "{rows}");
        }
        assert_eq!(rows.lines().count(), 6, "{rows}");
    }
}

/// Node `name`'s header and rows in `text`, where headers sit at `indent` and rows
/// two deeper, with that indent taken off.
fn node_block(text: &str, name: &str, indent: usize) -> Vec<String> {
    let header = format!("{:indent$}{name} (", "");
    let row = format!("{:1$}", "", indent + 2);
    text.lines()
        .skip_while(|line| !line.starts_with(&header))
        .enumerate()
        .take_while(|(at, line)| *at == 0 || line.starts_with(&row))
        .map(|(_, line)| line[indent..].to_owned())
        .collect()
}

/// `fix-web`'s Sync plan into production, the Sync, and production's diff after it.
fn plan_then_diff(store: &Target) -> (String, String) {
    let home = tempfile::tempdir().unwrap();
    let human = |args: &[&str]| {
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{args:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let plan = human(&["env", "sync", "--to", "--plan", "--env", "fix-web"]);
    let version = ok(
        store,
        &["env", "sync", "--to", "--plan", "--env", "fix-web"],
    )["version"]
        .as_str()
        .unwrap()
        .to_owned();
    ok(
        store,
        &[
            "env",
            "sync",
            "--to",
            "--env",
            "fix-web",
            "--version",
            &version,
        ],
    );
    (plan, human(&["diff"]))
}

/// A Service a Sync creates plans with the rows `diff` shows for it once it lands: its
/// repository and the branch it carries apart, never its root directory.
#[test]
fn a_sync_plan_reads_a_new_repository_service_as_diff_does() {
    // Only Cloud reads a repository.
    let store = &Target::Cloud {
        url: fake_cloud(),
        token: "ployz_alice",
    };
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "web:1"]);
    ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
    ok(
        store,
        &[
            "service",
            "add",
            "api",
            "--repo",
            "acme/web@dev",
            "--env",
            "fix-web",
        ],
    );
    let (plan, diff) = plan_then_diff(store);
    let planned = node_block(&plan, "api", 2);
    assert_eq!(planned, node_block(&diff, "api", 0), "{plan}\n{diff}");
    assert!(
        planned.iter().any(|line| line.contains("api.branch")),
        "{plan}"
    );
}

/// A Config a Sync creates plans as `diff` shows it once it lands: by name alone, its
/// files part of what it arrives as.
#[test]
fn a_sync_plan_reads_a_new_config_with_files_as_diff_does() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let from = file.path().to_str().unwrap();
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
        ok(store, &["config", "add", "extra", "--env", "fix-web"]);
        ok(
            store,
            &[
                "config",
                "put",
                "extra",
                "extra.yml",
                "--from",
                from,
                "--env",
                "fix-web",
            ],
        );
        let (plan, diff) = plan_then_diff(store);
        let planned = node_block(&plan, "extra", 2);
        assert_eq!(planned, node_block(&diff, "extra", 0), "{plan}\n{diff}");
        assert_eq!(planned, ["extra (create)"], "{plan}");
    }
}

/// A Service a Sync creates hides its rows, as `diff` does, but never the warnings they
/// carry: a secret it arrives without is named under it.
#[test]
fn a_sync_plan_warns_of_a_new_services_missing_secret() {
    let store = &cloud_target();
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "web:1"]);
    ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
    ok(
        store,
        &[
            "service", "add", "api", "--image", "api:1", "--env", "fix-web",
        ],
    );
    let (code, set) = ployz_with_stdin(
        store,
        &["set", "api.env.API_KEY", "--secret", "--env", "fix-web"],
        "test-key\n",
    );
    assert_eq!(code, Some(0), "{set}");
    let home = tempfile::tempdir().unwrap();
    let human = store
        .command(home.path())
        .args(["env", "sync", "--to", "--plan", "--env", "fix-web"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    assert_eq!(
        node_block(&human, "api", 2),
        [
            "api (create)",
            "  api.image: null → \"api:1\"",
            "  api.env.API_KEY (production lacks this secret: it arrives Not set)",
        ],
        "{human}"
    );
}

#[test]
fn an_agent_syncs_between_any_two_environments_of_a_project() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "new", "staging"]);
        ok(store, &["env", "branch", "fix-a", "--copy", "web"]);
        ok(store, &["env", "branch", "fix-b", "--copy", "web"]);
        failed(
            store,
            &["env", "sync", "--to", "fix-b", "--from", "fix-a"],
            2,
        );

        // Sideways, into a sibling.
        ok(store, &["set", "--env", "fix-a", "web.image=web:2"]);
        let synced = ok(store, &["env", "sync", "--to", "fix-b", "--env", "fix-a"]);
        assert_eq!(synced["into"]["name"], json!("fix-b"));
        assert_eq!(synced["next"], json!("ployz deploy --env fix-b"));
        assert_eq!(
            ok(store, &["get", "web.image", "--env", "fix-b"])["settings"][0]["value"],
            json!("web:2")
        );

        // Between two roots, from where the command runs.
        let plan_next = json!("ployz env sync --from production --plan --env staging");
        let plan = ok(
            store,
            &[
                "env",
                "sync",
                "--from",
                "production",
                "--plan",
                "--env",
                "staging",
            ],
        );
        assert_eq!(
            (&plan["from"]["name"], &plan["into"]["name"]),
            (&json!("production"), &json!("staging"))
        );
        assert_eq!(label(&plan["rows"][0]), "web");
        let version = plan["version"].as_str().unwrap();
        assert_eq!(
            plan["next"],
            json!(format!(
                "ployz env sync --from production --version {version} --env staging"
            ))
        );
        let stale = error(
            store,
            &[
                "env",
                "sync",
                "--from",
                "production",
                "--env",
                "staging",
                "--version",
                "0:0",
            ],
        );
        assert_eq!(stale["details"]["next"], plan_next);
        let synced = ok(
            store,
            &[
                "env",
                "sync",
                "--from",
                "production",
                "--env",
                "staging",
                "--version",
                version,
            ],
        );
        assert_eq!(synced["staged"], json!(["web"]));
        assert_eq!(
            ok(store, &["get", "web.image", "--env", "staging"])["settings"][0]["value"],
            json!("web:1")
        );

        // A root names where it syncs.
        let rootless = error(store, &["env", "sync", "--to"]);
        assert_eq!(
            rootless["details"]["next"],
            json!("ployz env sync --to ENV --project shop --env production")
        );
    }
}

#[test]
fn an_agent_adds_mounts_detaches_and_removes_volumes() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "db", "--image", "postgres:17"]);
        let added = ok(
            store,
            &["volume", "add", "data", "--mount", "db:/var/lib/postgresql"],
        );
        assert_eq!(added["staged"], json!(["volumes.data", "db.mounts.data"]));
        assert_eq!(added["next"], json!("ployz deploy"));
        let bad = failed(store, &["volume", "add", "logs", "--mount", "db"], 2);
        assert!(bad["message"].as_str().unwrap().contains("SERVICE:/PATH"));

        assert_eq!(
            ok(store, &["get", "db.mounts.data"])["settings"][0]["value"],
            json!("/var/lib/postgresql")
        );
        ok(store, &["set", "db.mounts.data=/data"]);
        let listed = ok(store, &["volume", "ls"]);
        assert_eq!(
            listed["volumes"],
            json!([{
                "id": added["volume"]["id"], "name": "data",
                "mounts": [{ "service": "db", "path": "/data" }],
                "deployed": false, "change": "create",
                "storage": { "kind": "provisioned", "maximumBytes": 5000000000_i64 }, "storage_locked": false,
                "shared_writes": false,
            }])
        );
        assert_eq!(
            ok(store, &["volume", "set", "data", "--docker"])["volume"]["storage"],
            json!({"kind":"docker"})
        );
        assert_eq!(
            ok(store, &["volume", "set", "data", "--size", "7GB"])["volume"]["storage"],
            json!({"kind":"provisioned","maximumBytes":7000000000_i64})
        );
        assert_eq!(
            ok(store, &["volume", "add", "logs", "--docker"])["volume"]["storage"],
            json!({"kind":"docker"})
        );
        let taken = error(store, &["volume", "rename", "logs", "data"]);
        assert_eq!(taken["code"], "conflict", "{taken}");
        let renamed = ok(store, &["volume", "rename", "logs", "cache"]);
        assert_eq!(renamed["staged"], json!(["volumes.cache"]));
        ok(store, &["volume", "rm", "cache"]);
        let inspected = ok(store, &["volume", "inspect", "data"]);
        assert_eq!(inspected["lineage"], inspected["id"]);

        // Detaching keeps the Volume; removing an undeployed one needs no Server.
        ok(store, &["unset", "db.mounts.data"]);
        assert_eq!(
            ok(store, &["volume", "ls"])["volumes"][0]["mounts"],
            json!([])
        );
        let removed = ok(store, &["volume", "rm", "data"]);
        assert_eq!(removed["staged"], json!(["volumes.data"]));
        assert_eq!(
            error(store, &["volume", "inspect", "data"])["code"],
            json!("not_found")
        );

        // Nothing deployed loses data, so there is nothing to accept. --yes answers only
        // --prune's ask, so the deploy runs and fails for want of a Server.
        let accept = error(store, &["deploy", "--accept-volume-loss", "data"]);
        assert_eq!(accept["code"], json!("invalid_argument"));
        let (code, deployed) = ployz(Some(store), &["deploy", "--yes"]);
        assert_eq!(code, Some(1), "{deployed}");
        assert_eq!(deployed["status"], json!("failed"));
    }
}

#[test]
fn deploy_prune_asks_before_removing_what_the_store_does_not_hold() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        // Under --json nothing can ask, so --prune refuses until --yes answers for it.
        let asked = error(store, &["deploy", "--prune"]);
        assert_eq!(asked["code"], json!("confirmation_required"), "{asked}");
        assert_eq!(
            asked["details"]["retry"],
            json!("ployz --json deploy --prune --yes"),
            "{asked}"
        );
        assert!(
            asked["message"]
                .as_str()
                .unwrap()
                .contains("Nothing deployed"),
            "{asked}"
        );
        assert_eq!(ok(store, &["deployment", "ls"])["deployments"], json!([]));

        let deployed = no_server_answers(store, &["deploy", "--prune", "--yes"]);
        assert_eq!(deployed["prune"], json!(true), "{deployed}");
        let id = deployed["id"].as_str().unwrap().to_owned();
        assert_eq!(
            ok(store, &["deployment", "show", &id])["prune"],
            json!(true)
        );

        // Without --prune the Deployment keeps what this Store never held, and says so.
        let kept = no_server_answers(store, &["deploy"]);
        assert!(kept.get("prune").is_none(), "{kept}");
    }
}

#[test]
fn auto_confirm_in_the_environment_answers_for_deploy() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let deploy = |auto_confirm: &str, args: &[&str]| -> (Option<i32>, Value) {
            let home = tempfile::tempdir().unwrap();
            let output = store
                .command(home.path())
                .env("PLOYZ_AUTO_CONFIRM", auto_confirm)
                .arg("--json")
                .args(args)
                .args(UNREACHABLE_SERVER)
                .output()
                .unwrap();
            let stdout = String::from_utf8(output.stdout).unwrap();
            let json = serde_json::from_str(&stdout).unwrap_or_else(|error| {
                panic!("stdout is not one JSON object ({error}): {stdout:?}")
            });
            (output.status.code(), json)
        };
        // `1` answers yes, as it does for every other --yes.
        let (code, pruned) = deploy("1", &["deploy", "--prune"]);
        assert_eq!(code, Some(1), "{pruned}");
        assert_eq!(pruned["prune"], json!(true), "{pruned}");
        // A deploy that never asks doesn't mind the answer's spelling.
        let (code, deployed) = deploy("1", &["deploy"]);
        assert_eq!(code, Some(1), "{deployed}");
        assert!(deployed.get("error").is_none(), "{deployed}");
        // `0` answers no.
        let (code, asked) = deploy("0", &["deploy", "--prune"]);
        assert_eq!(code, Some(2), "{asked}");
        assert_eq!(
            asked["error"]["code"],
            json!("confirmation_required"),
            "{asked}"
        );
    }
}

#[test]
fn an_agent_allows_shared_writes_before_a_second_writer() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "db", "--image", "postgres:17"]);
        ok(store, &["volume", "add", "data", "--mount", "db:/data"]);
        let refused = error(store, &["set", "db.replicas=2"]);
        assert_eq!(refused["code"], "conflict", "{refused}");
        assert_eq!(
            refused["details"]["next"],
            "ployz volume set data --shared-writes --env production --project shop"
        );

        let allowed = ok(store, &["volume", "set", "data", "--shared-writes"]);
        assert_eq!(allowed["volume"]["shared_writes"], true);
        assert_eq!(allowed["staged"], json!([]));
        ok(store, &["set", "db.replicas=2"]);
        let off = error(store, &["volume", "set", "data", "--shared-writes=false"]);
        assert_eq!(off["code"], "conflict", "{off}");
        assert_eq!(
            ok(store, &["volume", "inspect", "data"])["shared_writes"],
            true
        );

        let added = ok(
            store,
            &[
                "volume",
                "add",
                "uploads",
                "--shared-writes",
                "--mount",
                "db:/up",
            ],
        );
        assert_eq!(added["volume"]["shared_writes"], true);
        // One change at a time: storage and shared writes are separate writes.
        failed(
            store,
            &["volume", "set", "data", "--shared-writes", "--docker"],
            2,
        );
    }
}

#[test]
fn edits_to_a_service_never_deployed_show_in_the_diff_and_discard() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "nginx", "--image", "nginx"]);
        ok(store, &["deploy", "--skip-deploy"]);
        ok(store, &["set", "nginx.preDeployCommand=Jenje"]);
        let diff = ok(store, &["diff"]);
        let nginx = &diff["changes"][0];
        assert_eq!(nginx["lifecycle"], json!("create"), "{diff}");
        assert_eq!(nginx["settings"][0]["path"], json!("nginx.image"));
        assert_eq!(
            nginx["settings"][1]["path"],
            json!("nginx.preDeployCommand")
        );
        ok(store, &["discard", "nginx.preDeployCommand"]);
        let diff = ok(store, &["diff"]);
        let paths = diff["changes"][0]["settings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["path"].clone())
            .collect::<Vec<_>>();
        assert_eq!(paths, [json!("nginx.image")], "{diff}");
    }
}

#[test]
fn a_stale_revision_conflicts_and_names_the_read_that_refreshes_it() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["env", "new", "staging"]);
        ok(
            store,
            &[
                "service", "add", "web", "--image", "nginx:1", "--env", "staging",
            ],
        );
        let error = error(
            store,
            &["set", "web.replicas=2", "--expect", "1", "--env", "staging"],
        );
        assert_eq!(error.get("code"), Some(&json!("conflict")));
        assert_eq!(
            error.get("details"),
            Some(&json!({ "revision": 2, "next": "ployz get --env staging" }))
        );
    }
}

#[test]
fn mistakes_fail_with_their_codes() {
    for store in &targets() {
        let no_project = error(store, &["get"]);
        assert_eq!(no_project.get("code"), Some(&json!("not_found")));
        assert_eq!(
            no_project.pointer("/details/next"),
            Some(&json!("ployz project new NAME"))
        );
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let no_env = error(store, &["get", "--env", "preview"]);
        assert_eq!(
            no_env.pointer("/details/next"),
            Some(&json!("ployz env ls --project shop"))
        );

        let malformed = failed(store, &["set", "web.replicas"], 2);
        assert_eq!(malformed.get("code"), Some(&json!("invalid_argument")));
        let bad_revision = failed(
            store,
            &["set", "web.replicas=2", "--expect", "SECRET-CANARY"],
            2,
        );
        let bad_name = failed(
            store,
            &["service", "add", "SECRET-CANARY", "--image", "nginx:1"],
            2,
        );
        for error in [bad_revision, bad_name] {
            assert_eq!(error.get("code"), Some(&json!("invalid_argument")));
            assert!(!error.to_string().contains("CANARY"), "echoed: {error}");
        }

        for (args, code) in [
            (&["set", "web.replicas=lots"][..], "invalid_argument"),
            (&["set", "web.nope=1"], "invalid_argument"),
            (&["set", "api.replicas=1"], "not_found"),
            (&["project", "new", "shop"], "conflict"),
            (&["service", "add", "web", "--image", "nginx:2"], "conflict"),
        ] {
            let error = error(store, args);
            assert_eq!(error.get("code"), Some(&json!(code)), "{args:?}: {error}");
            assert!(
                !error.to_string().contains("lots"),
                "values are never echoed"
            );
        }
    }
}

#[test]
fn discarding_a_save_never_deployed_says_how_to_bring_it_back() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        ok(store, &["deploy", "--skip-deploy"]);
        let discard = || {
            let home = tempfile::tempdir().unwrap();
            let output = store
                .command(home.path())
                .arg("discard")
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(0), "{}", said(&output));
            said(&output)
        };
        let back = "web is in saved #1 but was never deployed, so discarding removes it. Bring it back: ployz discard web";

        let first = discard();
        assert!(
            first.contains("Discarded 1 staged change in shop/production: web."),
            "{first}"
        );
        assert!(first.contains(back), "{first}");
        let diff = ok(store, &["diff"]);
        assert_eq!(diff["draft_changes"][0]["name"], json!("web"), "{diff}");
        let status = ok(store, &["status"]);
        assert_ne!(
            status.pointer("/staged/changes"),
            Some(&json!(0)),
            "status counts the row diff lists: {status}"
        );

        let second = discard();
        assert!(!second.contains("nothing to discard"), "{second}");
        assert!(second.contains(back), "{second}");
        let json = ok(store, &["discard"]);
        assert_eq!(
            json["comes_back"],
            json!([{ "name": "web", "command": "ployz discard web", "drops_unsaved": false }]),
            "{json}"
        );
        assert_eq!(
            json["reversed"][0]["way_back"],
            json!({ "path": "web", "drops_unsaved": false }),
            "{json}"
        );

        let restored = ok(store, &["discard", "web"]);
        assert_eq!(restored["changed"], json!(true), "{restored}");
        let diff = ok(store, &["diff"]);
        assert_eq!(diff["changes"][0]["lifecycle"], json!("create"), "{diff}");
        assert_eq!(diff["draft_count"], json!(0), "{diff}");
    }
}

#[test]
fn an_agent_reviews_saves_and_discards() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        let added = ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        assert_eq!(added.get("next"), Some(&json!("ployz deploy")));
        let set = ok(store, &["set", "web.replicas=3"]);
        assert_eq!(set.get("next"), Some(&json!("ployz deploy")));

        let diff = ok(store, &["diff"]);
        let version = diff
            .get("version")
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        assert_eq!(diff.pointer("/changes/0/name"), Some(&json!("web")));
        assert_eq!(diff.pointer("/changes/0/lifecycle"), Some(&json!("create")));
        assert_eq!(
            diff.pointer("/changes/0/settings/1/path"),
            Some(&json!("web.replicas"))
        );
        assert_eq!(diff.get("next"), Some(&json!("ployz deploy")));

        // A review taken before another edit is stale.
        ok(store, &["set", "web.startCommand=nginx"]);
        let stale = error(
            store,
            &["deploy", "--skip-deploy", "--expect-version", &version],
        );
        assert_eq!(stale.get("code"), Some(&json!("conflict")));
        let fresh = stale
            .pointer("/details/diff/version")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(
            stale.pointer("/details/diff"),
            Some(&json!({ "version": fresh })),
            "a stale review hands back only the fresh version, not every staged value"
        );
        assert_eq!(
            stale.pointer("/details/next"),
            Some(&json!(format!(
                "ployz deploy --skip-deploy --expect-version {fresh}"
            )))
        );

        let saved = ok(
            store,
            &["deploy", "--skip-deploy", "--expect-version", fresh],
        );
        assert_eq!(saved.get("saved"), Some(&json!(1)));
        assert_eq!(saved.get("created"), Some(&json!(true)));
        assert_eq!(saved.get("next"), Some(&json!("ployz deploy")));
        let diff = ok(store, &["diff"]);
        assert_eq!(diff.get("holds_staged"), Some(&json!(true)));
        assert_eq!(
            diff.get("next"),
            Some(&json!("ployz deploy")),
            "saved is not yet deployed"
        );

        let discarded = ok(store, &["discard", "web"]);
        assert_eq!(discarded.get("saved"), Some(&json!(1)));
        assert_eq!(discarded.get("next"), Some(&json!("ployz diff")));
        let inverse = ok(store, &["diff"]);
        assert_eq!(inverse.get("changes"), Some(&json!([])));
        assert_eq!(inverse.get("draft_count"), Some(&json!(1)));
        assert_eq!(inverse.get("holds_staged"), Some(&json!(false)));
        assert_eq!(
            inverse.pointer("/draft_changes/0/lifecycle"),
            Some(&json!("delete"))
        );
        let version = inverse["version"].as_str().unwrap();
        assert_eq!(
            inverse.get("next"),
            Some(&json!("ployz deploy --skip-deploy"))
        );
        assert_eq!(
            ok(store, &["history"])["revisions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let saved = ok(
            store,
            &["deploy", "--skip-deploy", "--expect-version", version],
        );
        assert_eq!(saved.get("saved"), Some(&json!(2)));
        assert_eq!(saved.get("created"), Some(&json!(true)));
        let after = ok(store, &["diff"]);
        assert_eq!(after.get("changes"), Some(&json!([])));
        assert_eq!(after.get("draft_count"), Some(&json!(0)));
        assert_eq!(after.get("holds_staged"), Some(&json!(true)));
        assert_eq!(
            ok(store, &["history"])["revisions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            ok(store, &["deploy", "--skip-deploy"]).get("created"),
            Some(&json!(false))
        );
        assert_eq!(
            ok(store, &["history"])["revisions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}

#[test]
fn get_patch_get_round_trips_and_the_environment_shows_only_what_is_set() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);

        let mut values = ok(store, &["get", "web"])["values"].clone();
        values["cpuLimit"] = json!(0.5);
        values["memLimit"] = json!(1.5);
        values["restartPolicy"] = json!("on-failure");
        let patch = values.to_string();
        let patched = ok(store, &["set", "web", "--patch", &patch]);
        assert_eq!(
            patched.get("staged"),
            Some(&json!([
                "web.cpuLimit",
                "web.memLimit",
                "web.restartPolicy"
            ]))
        );
        assert_eq!(ok(store, &["get", "web"])["values"], values);

        let home = tempfile::tempdir().unwrap();
        let mut stdin = store.command(home.path());
        stdin
            .args(["set", "web", "--patch", "-", "--json"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        let mut child = stdin.spawn().unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), br#"{"replicas": 2}"#).unwrap();
        assert!(child.wait_with_output().unwrap().status.success());

        let whole = ok(store, &["get"]);
        let paths = whole["settings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["path"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                "web.cpuLimit",
                "web.image",
                "web.memLimit",
                "web.replicas",
                "web.restartPolicy"
            ]
        );
        assert_eq!(whole.get("values"), None);
        assert_eq!(
            ok(store, &["get", "--all"])["settings"]
                .as_array()
                .unwrap()
                .len(),
            13
        );

        ok(store, &["set", "web.memLimit=1GiB"]);
        assert_eq!(
            ok(store, &["get", "web.memLimit"])["settings"][0]["value"],
            json!(1.073741824)
        );
        let bytes = failed(store, &["set", "web.memLimit=1073741824"], 2);
        assert_eq!(bytes.pointer("/details/example"), Some(&json!("1GiB")));

        ok(store, &["set", "web.env.API_URL=https://api.example"]);
        let renamed = ok(
            store,
            &["set", "web.env.API_URL", "--rename", "PUBLIC_API_URL"],
        );
        assert_eq!(
            renamed.get("staged"),
            Some(&json!(["web.env.PUBLIC_API_URL", "web.env.API_URL"]))
        );
        assert_eq!(
            ok(store, &["get", "web.env.PUBLIC_API_URL"])["settings"][0]["value"],
            json!("https://api.example")
        );
        ok(store, &["set", "web.env.LOG_LEVEL=info"]);
        let clash = error(
            store,
            &["set", "web.env.LOG_LEVEL", "--rename", "PUBLIC_API_URL"],
        );
        assert_eq!(clash.get("code"), Some(&json!("conflict")));

        ok(store, &["set", "web.env.UPSTREAM=${{ PUBLIC_API_URL }}/v1"]);
        let used = error(
            store,
            &["set", "web.env.PUBLIC_API_URL", "--rename", "API_URL"],
        );
        assert_eq!(used.get("code"), Some(&json!("conflict")));
        assert_eq!(
            used.pointer("/details/variables"),
            Some(&json!(["web.UPSTREAM"]))
        );
        let next = used
            .pointer("/details/next")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            next.starts_with(
                "ployz set web.env.PUBLIC_API_URL --rename API_URL --update-references"
            ),
            "{next}"
        );
        let alone = failed(
            store,
            &["set", "web.env.LOG_LEVEL", "--update-references"],
            2,
        );
        assert_eq!(alone.get("code"), Some(&json!("invalid_argument")));
        let switched = ok(
            store,
            &[
                "set",
                "web.env.PUBLIC_API_URL",
                "--rename",
                "API_URL",
                "--update-references",
            ],
        );
        assert_eq!(
            switched.get("staged"),
            Some(&json!([
                "web.env.API_URL",
                "web.env.PUBLIC_API_URL",
                "web.env.UPSTREAM"
            ]))
        );
        assert_eq!(
            ok(store, &["get", "web.env.UPSTREAM"])["settings"][0]["value"],
            json!("${{ API_URL }}/v1")
        );

        for patch in [r#"{"memLimit": null}"#, "not json"] {
            let refused = failed(store, &["set", "web", "--patch", patch], 2);
            assert_eq!(
                refused.get("code"),
                Some(&json!("invalid_argument")),
                "{patch}"
            );
        }
        let before = ok(store, &["diff"]);
        for patch in [
            "{\"env\":{\"PRIVATE\":\"fixture-private-value\"",
            &format!("{{\"cpuLimit\":{}0{}}}", "[".repeat(140), "]".repeat(140)),
        ] {
            let (exit, output) = ployz_with_stdin(store, &["set", "web", "--patch", "-"], patch);
            assert_eq!(exit, Some(2));
            assert_eq!(
                serde_json::from_str::<Value>(&output).unwrap()["error"]["code"],
                "invalid_argument"
            );
            assert!(!output.contains("fixture-private-value"));
            assert_eq!(ok(store, &["diff"]), before);
        }
    }
}

#[test]
fn the_catalog_describes_settings_without_a_store() {
    let (code, schema) = ployz(None, &["schema"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        schema.get("$schema"),
        Some(&json!("https://json-schema.org/draft/2020-12/schema"))
    );
    assert_eq!(schema.get("x-ployz-version"), Some(&json!(1)));
    assert_eq!(
        schema.pointer("/$defs/service/properties/memLimit/title"),
        Some(&json!("Memory limit"))
    );
    let (_, again) = ployz(None, &["schema"]);
    assert_eq!(schema.to_string(), again.to_string(), "deterministic");
    let commands = schema
        .get("x-ployz-commands")
        .and_then(Value::as_array)
        .unwrap();
    let set = commands
        .iter()
        .find(|entry| entry.get("command") == Some(&json!("set")))
        .unwrap();
    assert_eq!(set.get("json"), Some(&json!(true)));
    assert!(
        set.get("args")
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .any(|arg| arg.get("name") == Some(&json!("--patch")))
    );
    let project_rm = commands
        .iter()
        .find(|entry| entry.get("command") == Some(&json!("project rm")))
        .unwrap();
    assert!(
        project_rm["args"].as_array().unwrap().iter().any(
            |arg| arg["name"] == json!("NAME") && arg["help"] == json!("The Project to remove")
        ),
        "{project_rm}"
    );
    let completion = commands
        .iter()
        .find(|entry| entry.get("command") == Some(&json!("completion")))
        .unwrap();
    assert_eq!(completion.get("json"), Some(&json!(false)));
    assert!(
        commands
            .iter()
            .all(|entry| entry.get("command") != Some(&json!("service"))),
        "groups are not commands"
    );
    let classified = |command: &str| {
        let entry = commands
            .iter()
            .find(|entry| entry.get("command") == Some(&json!(command)))
            .unwrap_or_else(|| panic!("{command} is in the catalog"));
        (
            entry.get("approval").and_then(Value::as_str).unwrap(),
            entry.get("surface").and_then(Value::as_str).unwrap(),
        )
    };
    assert_eq!(classified("ctx"), ("never", "local"));
    for action in [
        "add", "put", "rm-file", "inspect", "ls", "rename", "rm", "mount", "unmount",
    ] {
        assert_eq!(classified(&format!("config {action}")), ("never", "cloud"));
    }
    for action in ["move", "release"] {
        assert_eq!(classified(&format!("volume {action}")), ("never", "cloud"));
    }
    assert_eq!(classified("volume mirror"), ("never", "cloud"));
    assert_eq!(classified("volume mirror rm"), ("always", "cloud"));
    assert_eq!(classified("volume restore"), ("always", "cloud"));
    assert_eq!(classified("deploy"), ("depends", "cloud"));
    assert_eq!(classified("mcp"), ("never", "local"));
    assert!(
        commands
            .iter()
            .all(|entry| entry.get("approval").is_some() && entry.get("surface").is_some())
    );

    let (code, one) = ployz(None, &["schema", "web.replicas"]);
    assert_eq!(code, Some(0));
    assert_eq!(one.get("maximum"), Some(&json!(50)));

    let (code, explained) = ployz(None, &["explain", "web.restartPolicy"]);
    assert_eq!(code, Some(0));
    assert_eq!(explained.get("path"), Some(&json!("web.restartPolicy")));
    assert_eq!(
        explained.get("example"),
        Some(&json!("ployz set 'web.restartPolicy=on-failure'"))
    );
    assert_eq!(
        explained.pointer("/schema/x-ployz-apply"),
        Some(&json!("staged"))
    );

    let (code, listed) = ployz(None, &["explain", "WEB"]);
    assert_eq!(code, Some(0));
    assert_eq!(listed.get("service"), Some(&json!("web")));
    assert_eq!(listed.pointer("/domains/path"), Some(&json!("web.domains")));

    let (code, json) = ployz(None, &["explain", "web.restart_policy"]);
    assert_eq!(code, Some(2));
    assert_eq!(
        json.pointer("/error/details/did_you_mean"),
        Some(&json!("restartPolicy"))
    );
    let valid = json
        .pointer("/error/details/valid_children")
        .and_then(Value::as_array)
        .unwrap();
    let position = valid.iter().position(|name| name == "restartPolicy");
    assert!(
        position.is_some_and(|at| at >= ployz::ui::VALID_SHOWN),
        "{valid:?}"
    );
}

#[test]
fn setting_paths_complete_from_the_store_and_the_catalog() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let complete = |word: &str| {
            let home = tempfile::tempdir().unwrap();
            let output = store
                .command(home.path())
                .args(["--", "ployz", "set", word])
                .env("PLOYZ_COMPLETE", "bash")
                .env("_CLAP_COMPLETE_INDEX", "2")
                .env("_CLAP_IFS", "\n")
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap()
        };
        assert_eq!(complete("w").trim(), "web.");
        assert_eq!(complete("web.me").trim(), "web.memLimit");
        ok(store, &["set", "web.env.LOG_LEVEL=info"]);
        ok(store, &["volume", "add", "data", "--mount", "web:/data"]);
        assert!(complete("web.env").contains("web.env.LOG_LEVEL"));
        assert_eq!(complete("web.mou").trim(), "web.mounts.data");
    }
}

#[test]
fn an_ambiguous_project_names_the_link_that_settles_it() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["project", "new", "blog"]);
        let refused = error(store, &["env", "new", "staging"]);
        assert_eq!(refused.get("code"), Some(&json!("ambiguous")));
        assert_eq!(
            refused.get("details"),
            Some(&json!({
                "projects": ["blog", "shop"],
                "next": "ployz link --project PROJECT",
            }))
        );
        // Linked, the command runs again as typed: no value is echoed, no flag lost.
        let next = |args: &[&str]| error(store, args)["details"]["next"].clone();
        assert_eq!(
            next(&["set", "--env", "production", "web.replicas=SECRET-CANARY"]),
            json!("ployz link --project PROJECT --env production")
        );
        assert_eq!(
            next(&["env", "keep", "--off"]),
            json!("ployz link --project PROJECT")
        );
    }
}

#[test]
fn cloud_answers_only_a_credential_in_its_own_organization() {
    let url = fake_cloud();
    let alice = Target::Cloud {
        url: url.clone(),
        token: "ployz_alice",
    };
    ok(&alice, &["project", "new", "shop"]);
    let bob = Target::Cloud {
        url: url.clone(),
        token: "ployz_bob",
    };
    assert_eq!(
        error(&bob, &["get"]).get("code"),
        Some(&json!("not_found")),
        "another Organization sees nothing"
    );
    let refused = error(
        &Target::Cloud {
            url,
            token: "revoked",
        },
        &["get"],
    );
    assert_eq!(refused.get("code"), Some(&json!("unauthenticated")));
    assert_eq!(
        refused.pointer("/details/next"),
        Some(&json!("unset PLOYZ_TOKEN && ployz login"))
    );

    let (code, json) = ployz(None, &["get"]);
    assert_eq!(code, Some(1), "signed out: {json}");
    assert_eq!(json.pointer("/error/code"), Some(&json!("unauthenticated")));
    assert_eq!(
        json.pointer("/error/details/next"),
        Some(&json!("ployz login"))
    );
}

#[test]
fn a_member_reaching_a_server_is_told_to_ask_an_admin() {
    let linus = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_linus",
    };
    let refused = error(&linus, &["server", "ls"]);
    assert_eq!(refused["code"], json!("forbidden"), "{refused}");
    assert_eq!(
        refused["message"],
        json!("Server access needs an admin. Ask an owner or admin of acme."),
        "{refused}"
    );
    for args in [&["server", "ls"][..], &["exec", "web", "true"]] {
        let home = tempfile::tempdir().unwrap();
        let output = linus.command(home.path()).args(args).output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains("Server access needs an admin. Ask an owner or admin of acme."),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn a_member_names_a_preferred_builder_for_cloud_to_resolve() {
    let linus = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_linus",
    };
    ok(&linus, &["project", "new", "shop"]);
    ok(&linus, &["service", "add", "web", "--repo", "acme/web@dev"]);
    // Cloud resolves a name from the Servers it keeps; this fake Cloud is the bare Store,
    // which takes only an ID, so its refusal shows the name reached it as typed.
    let refused = error(&linus, &["set", "web.preferredBuilder=machine-1"]);
    assert_eq!(refused["code"], json!("invalid_argument"), "{refused}");
    assert_eq!(
        refused["message"],
        json!("preferredBuilder: expected \"github\" or a Server's Machine ID"),
        "{refused}"
    );
}

/// #3275: a Deploy saves first, so one that ran nothing leaves its save behind. Discard
/// and diff say that save's deploy ran nothing, not that it was never deployed.
#[test]
fn a_save_whose_deploy_ran_nothing_says_so() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        let Written::Deployment(admitted) = written else {
            return;
        };
        if admitted.number == Some(1) {
            succeed(store, &admitted.id);
            return;
        }
        let runner = RunnerId::parse("refusing-worker").unwrap();
        store.claim(&admitted.id, &runner).unwrap();
        let evidence = serde_json::from_value(json!({"evidence": "not_executed", "value": {
            "reason": "Storage capacity is unknown", "cause": []
        }}))
        .unwrap();
        store.record(&admitted.id, &runner, evidence).unwrap();
    }
    let cloud = Target::Cloud {
        url: fake_cloud_with_dispatch(worker),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "nginx:1"]);
    ok(&cloud, &["deploy"]);
    ok(&cloud, &["set", "web.replicas=3"]);
    let (code, refused) = ployz(Some(&cloud), &["deploy"]);
    assert_eq!(code, Some(1), "{refused}");

    let text = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = cloud.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{}", said(&output));
        said(&output)
    };
    let refused = text(&["diff"]);
    assert!(
        refused.contains(
            "Saved as version #2. Its deploy ran nothing, so none of these changes reached a server."
        ),
        "{refused}"
    );
    let discarded = text(&["discard"]);
    assert!(
        discarded.contains(
            "web has changes in saved #2, whose deploy ran nothing, so discarding undoes them. Bring them back: ployz discard web"
        ),
        "{discarded}"
    );
    let diff = text(&["diff"]);
    assert!(
        diff.contains(
            "Saved #2 holds changes above that its deploy never ran; a new save or deploy leaves them behind."
        ),
        "{diff}"
    );
    assert_eq!(ok(&cloud, &["diff"])["saved_ran_nothing"], json!(true));
}

#[test]
fn deployment_text_shows_the_deepest_cause_and_json_keeps_the_chain() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        let Written::Deployment(admitted) = written else {
            return;
        };
        let runner = RunnerId::parse("failing-worker").unwrap();
        let claimed = store.claim(&admitted.id, &runner).unwrap();
        let cause = match admitted.number {
            Some(3) => json!([]),
            _ => json!(["Writing Docker metadata failed", "No space left on device"]),
        };
        let evidence = if admitted.number == Some(2) {
            let operation = json!({
                "type": "remove_container", "machine_id": "a".repeat(32),
                "container_id": "b".repeat(64)
            });
            let preview = serde_json::from_value(json!({
                "namespace": claimed.intent.namespace,
                "operations": [{"index": 0, "machine_id": "a".repeat(32),
                    "service_name": "web", "operation": operation,
                    "status": {"type": "pending"}}],
                "warnings": [], "would_remove": [], "preserved_volumes": []
            }))
            .unwrap();
            store
                .record(
                    &admitted.id,
                    &runner,
                    ployz_store::RunEvidence::Prepared(preview),
                )
                .unwrap();
            let outcome = serde_json::from_value(json!({
                "type": "failed", "completed": [], "unexecuted": [],
                "failed": {"type": "operation", "operation": operation, "error": {
                    "type": "machine", "action": "RemoveContainer", "error": {
                        "code": "internal", "message": "Docker storage operation failed",
                        "details": {}, "cause": cause
                    }
                }}
            }))
            .unwrap();
            ployz_store::RunEvidence::Executed {
                outcome: Box::new(outcome),
                progress: Vec::new(),
                removed: Vec::new(),
            }
        } else {
            serde_json::from_value(json!({"evidence": "not_executed", "value": {
                "reason": "Preparing storage failed", "cause": cause
            }}))
            .unwrap()
        };
        store.record(&admitted.id, &runner, evidence).unwrap();
    }
    let cloud = Target::Cloud {
        url: fake_cloud_with_dispatch(worker),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "nginx:1"]);
    for number in 1..=3 {
        let (code, deployed) = ployz(Some(&cloud), &["deploy"]);
        assert_eq!(code, Some(1), "{deployed}");
        let id = deployed["id"].as_str().unwrap();
        let shown = ok(&cloud, &["deployment", "show", id]);
        assert_eq!(shown["outcome"], deployed["outcome"]);
        let cause = shown["outcome"]["cause"].as_array().unwrap();
        let reason = shown["outcome"]["reason"].as_str().unwrap();
        let home = tempfile::tempdir().unwrap();
        let output = cloud
            .command(home.path())
            .args(["deployment", "show", id])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(reason), "{text}");
        if number == 3 {
            assert!(cause.is_empty());
            assert!(!text.contains("cause:"), "{text}");
        } else {
            assert!(cause.len() >= 2, "{shown}");
            assert_eq!(cause.last(), Some(&json!("No space left on device")));
            assert!(cause.contains(&json!("Writing Docker metadata failed")));
            assert_eq!(text.matches("cause:").count(), 1, "{text}");
            assert_eq!(text.matches("No space left on device").count(), 1, "{text}");
            for wrapper in cause.iter().take(cause.len() - 1) {
                assert!(!text.contains(wrapper.as_str().unwrap()), "{text}");
            }
        }
    }
}

#[test]
fn an_agent_plans_deploys_and_reads_the_deployment() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        ok(store, &["service", "add", "api", "--image", "nginx:1"]);

        let plan = ok(store, &["deploy", "web", "--plan"]);
        let version = plan["version"].as_str().unwrap().to_owned();
        assert_eq!(plan["namespace"], json!("shop-production"));
        assert_eq!(plan["changes"].as_array().unwrap().len(), 1);
        assert_eq!(plan["unresolved"], json!(["operations"]));
        assert_eq!(
            plan["next"],
            json!(format!("ployz deploy web --expect-version {version}"))
        );
        assert_eq!(
            error(store, &["deploy", "nope", "--plan"])["code"],
            json!("not_found")
        );

        // A review taken before another edit is stale.
        ok(store, &["set", "web.replicas=2"]);
        let stale = error(store, &["deploy", "--expect-version", &version]);
        assert_eq!(stale["code"], json!("conflict"));
        assert_eq!(
            stale["details"]["next"],
            json!(format!(
                "ployz deploy --expect-version {}",
                stale["details"]["diff"]["version"].as_str().unwrap()
            ))
        );

        let deployed = no_server_answers(store, &["deploy"]);
        assert_eq!(deployed["saved"], json!(1));
        assert_eq!(deployed["status"], json!("failed"));
        assert_eq!(deployed["outcome"]["type"], json!("not_executed"));
        let outcome = deployed["outcome"].to_string();
        assert!(
            outcome.contains("Ployz couldn't reach your Server's daemon")
                && !outcome.contains("connections from")
                && !outcome.contains("Machine"),
            "{outcome}"
        );
        assert_eq!(deployed["nodes"][0]["outcome"], json!("not_attempted"));
        let id = deployed["id"].as_str().unwrap().to_owned();
        assert_eq!(
            deployed["next"],
            json!(format!("ployz deployment show {}", deployed["number"]))
        );

        let shown = ok(store, &["deployment", "show", &id]);
        assert_eq!(shown["id"], json!(id));
        assert_eq!(shown.get("next"), None);

        let status = ok(store, &["status"]);
        let show = json!(format!("ployz deployment show {}", deployed["number"]));
        assert_eq!(status["next"], show);
        assert_eq!(status["deploying"], json!([]));
        assert_eq!(status["attention"][0]["reason"], json!("deployment_failed"));
        assert_eq!(status["attention"][0]["deployment"], json!(id));
        let listed = ok(store, &["deployment", "ls", "--limit", "1"]);
        assert_eq!(listed["deployments"][0]["id"], json!(id));
        assert_eq!(listed["next_cursor"], Value::Null);
        failed(store, &["deployment", "show", "not-an-id"], 2);
        let number = shown["number"].to_string();
        assert_eq!(ok(store, &["deployment", "show", &number])["id"], json!(id));
        let hashed = format!("#{number}");
        assert_eq!(ok(store, &["deployment", "show", &hashed])["id"], json!(id));
        let missing = error(store, &["deployment", "show", "999"]);
        assert_eq!(missing["code"], json!("not_found"), "{missing}");

        // Detached, `deploy` returns the Deployment Cloud's runner runs at once.
        match store {
            Target::Local(_) => {
                failed(store, &["deploy", "--detach"], 2);
            }
            Target::Cloud { .. } => {
                let detached = ok(
                    store,
                    &["deploy", "--detach", "--message", "Ship the header"],
                );
                assert_eq!(detached["number"], json!(2));
                assert_eq!(detached["message"], json!("Ship the header"));
                assert_eq!(detached["next"], json!("ployz deployment show 2"));
            }
        }
    }
}

#[test]
fn an_upload_is_recorded_with_its_base_commit_and_kept_for_later_deploys() {
    let source = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .arg("-C")
            .arg(source.path())
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    };
    std::fs::write(source.path().join("Dockerfile"), "FROM scratch\n").unwrap();
    git(&["init", "-q"]);
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let head = Command::new("git")
        .arg("-C")
        .arg(source.path())
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let head = String::from_utf8(head.stdout).unwrap().trim().to_owned();
    std::fs::write(source.path().join("app.txt"), "not committed").unwrap();
    let dir = source.path().to_str().unwrap();
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "app"]);
        let args = ["deploy", "--upload", dir];
        match store {
            Target::Local(_) => {
                let deployed = no_server_answers(store, &args);
                let upload = &deployed["upload"];
                assert_eq!(upload["base"], json!({"commit": head, "changed": true}));
                let digest = upload["digest"].as_str().unwrap();
                assert!(ployz_core::is_lower_hex(digest, 64), "{digest}");
                // A later deploy without a new upload reuses the latest one's images.
                let redeployed = no_server_answers(store, &["deploy"]);
                assert_eq!(&redeployed["upload"], upload);
            }
            Target::Cloud { .. } => {
                // Cloud takes the upload first and names who sent it.
                let deployed = no_server_answers(store, &args);
                assert_eq!(
                    deployed["upload"]["base"],
                    json!({"commit": head, "changed": true})
                );
                assert_eq!(deployed["upload"]["uploader"], json!("alice"));
            }
        }
    }
}

#[test]
fn redeploying_an_older_deployment_asks_before_it_rolls_back() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "nginx:1"]);
    applied(&store, "production");
    ok(&store, &["set", "web.replicas=2"]);
    applied(&store, "production");

    let asked = error(&store, &["deployment", "redeploy", "1"]);
    assert_eq!(asked["code"], "confirmation_required", "{asked}");
    assert_eq!(asked["details"]["rolls_back"], json!([2]));
    assert_eq!(
        asked["details"]["retry"],
        json!("ployz --json deployment redeploy 1 --yes")
    );

    let rolled_back = no_server_answers(&store, &["deployment", "redeploy", "1", "--yes"]);
    assert_eq!(rolled_back["number"], json!(3), "{rolled_back}");
    // Redeploying the newest one rolls nothing back, so it asks nothing.
    no_server_answers(&store, &["deployment", "redeploy", "3"]);
}

#[test]
fn an_agent_retries_a_failed_deployment_and_reads_its_logs() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let deployed = no_server_answers(store, &["deploy"]);
        let failed_id = deployed["id"].as_str().unwrap().to_owned();
        // Saved since: a redeploy still ships what the failed one froze.
        ok(store, &["set", "web.replicas=2"]);
        ok(store, &["deploy", "--skip-deploy"]);

        let retried = no_server_answers(store, &["deployment", "redeploy", &failed_id]);
        assert_eq!(
            (&retried["number"], &retried["saved"], &retried["status"]),
            (&json!(2), &json!(1), &json!("failed"))
        );
        let id = retried["id"].as_str().unwrap();
        assert_ne!(id, failed_id);
        assert_eq!(retried["next"], json!("ployz deployment show 2"));

        let show = json!("ployz deployment show 1");
        for verb in ["start", "cancel"] {
            let refused = error(store, &["deployment", verb, &failed_id]);
            assert_eq!(refused["code"], json!("conflict"));
            assert_eq!(refused["details"]["next"], show);
        }
        let missing = error(
            store,
            &[
                "deployment",
                "redeploy",
                "00000000-0000-4000-8000-000000000999",
            ],
        );
        assert_eq!(missing["code"], json!("not_found"));
        failed(store, &["deployment", "cancel", "not-an-id"], 2);

        failed(store, &["logs", "--deployment", "not-an-id"], 2);
        let unknown = error(
            store,
            &[
                "logs",
                "--deployment",
                "00000000-0000-4000-8000-000000000999",
            ],
        );
        assert_eq!(unknown["code"], json!("not_found"));

        // Its build logs: an image-only Deployment built nothing; `--build` needs a Deployment.
        let builds = ok(store, &["logs", "--deployment", &failed_id, "--build"]);
        assert_eq!(builds, json!({"builds": []}));
        let unbuilt = error(
            store,
            &["logs", "--deployment", &failed_id, "--build", "web"],
        );
        assert_eq!(unbuilt["code"], json!("not_found"));
        failed(store, &["logs", "--build"], 2);
    }
}

#[test]
fn following_logs_refuses_a_service_the_environment_does_not_have() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        for args in [
            &["logs", "-f", "nosvc"][..],
            &["logs", "-f", "web", "nosvc"],
        ] {
            let refused = error(store, args);
            assert_eq!(
                (
                    &refused["code"],
                    &refused["message"],
                    &refused["details"]["valid_children"]
                ),
                (
                    &json!("not_found"),
                    &json!("No Service named nosvc in Environment production"),
                    &json!(["web"])
                ),
                "{args:?}"
            );
        }
    }
}

#[test]
fn reading_through_cloud_refuses_a_name_nothing_answered_for() {
    // Cloud refuses this member Server access, so `logs` and `metrics` read through Cloud.
    let linus = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_linus",
    };
    ok(&linus, &["project", "new", "shop"]);
    ok(&linus, &["service", "add", "web", "--image", "nginx:1"]);
    ok(&linus, &["service", "add", "worker", "--image", "nginx:1"]);
    for args in [&["logs", "nosvc"][..], &["metrics", "nosvc"]] {
        let refused = error(&linus, args);
        assert_eq!(
            (
                &refused["code"],
                &refused["message"],
                &refused["details"]["valid_children"]
            ),
            (
                &json!("not_found"),
                &json!("No Service named nosvc in Environment production"),
                &json!(["web", "worker"])
            ),
            "{args:?}"
        );
    }
    ok(&linus, &["metrics", "web"]);
    // web-2 did not answer for `worker` and may hold its lines, so that outranks the refusal.
    let home = tempfile::tempdir().unwrap();
    let output = linus
        .command(home.path())
        .args(["logs", "worker", "nosvc"])
        .output()
        .unwrap();
    assert_eq!(
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap()
        ),
        (
            Some(3),
            String::new(),
            "web-2 did not answer: timed out\n! web-2 did not answer; its rows are missing.\n"
                .to_owned()
        )
    );
    // Lines for `web` already printed, so the refusal for `nosvc` comes after them.
    let home = tempfile::tempdir().unwrap();
    let output = linus
        .command(home.path())
        .args(["logs", "web", "nosvc"])
        .output()
        .unwrap();
    assert_eq!(
        (
            output.status.code(),
            String::from_utf8(output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap()
        ),
        (
            Some(3),
            "web@web-1  ready\n".to_owned(),
            "error: No Service named nosvc in Environment production\nvalid: web, worker\n"
                .to_owned()
        )
    );
}

#[test]
fn a_staging_token_is_refused_logs_of_another_environment_with_nothing_deployed() {
    let stagehand = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_stagehand",
    };
    ok(&stagehand, &["project", "new", "shop"]);
    for args in [&["logs"][..], &["ps"]] {
        let refused = error(&stagehand, args);
        assert_eq!(
            (&refused["code"], &refused["message"], &refused["details"]),
            (
                &json!("forbidden"),
                &json!("This token can deploy only to staging."),
                &json!({ "environments": ["staging"] }),
            ),
            "{args:?}"
        );
    }
}

#[test]
fn logs_of_an_environment_with_nothing_deployed_stay_empty_unless_its_scope_refuses() {
    let stagehand = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_stagehand",
    };
    ok(&stagehand, &["project", "new", "shop"]);
    ok(&stagehand, &["env", "new", "staging"]);
    // Cloud refuses the token's own Namespace too until a Deployment fixes it.
    assert_eq!(
        ok(&stagehand, &["logs", "--env", "staging"]),
        json!({ "logs": [] })
    );
    for token in ["ployz_serverless", "ployz_unanswered", "ployz_unpermitted"] {
        let target = Target::Cloud {
            url: fake_cloud(),
            token,
        };
        ok(&target, &["project", "new", "shop"]);
        assert_eq!(ok(&target, &["logs"]), json!({ "logs": [] }), "{token}");
    }
}

#[test]
fn reading_through_cloud_exits_3_when_a_server_did_not_answer() {
    let linus = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_linus",
    };
    ok(&linus, &["project", "new", "shop"]);
    ok(&linus, &["service", "add", "worker", "--image", "nginx:1"]);
    for (args, note) in [
        (&["logs", "worker"][..], "web-2 did not answer: timed out\n"),
        (&["metrics", "worker"], "web-2 did not answer: timed out\n"),
        (&["ps"], ""),
    ] {
        let (code, json) = ployz(Some(&linus), args);
        assert_eq!(code, Some(3), "{args:?}: {json}");
        let home = tempfile::tempdir().unwrap();
        let output = linus.command(home.path()).args(args).output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(3), "{args:?}: {stderr}");
        assert!(
            stderr.ends_with(&format!(
                "{note}! web-2 did not answer; its rows are missing.\n"
            )),
            "{args:?}: {stderr}"
        );
    }
}

/// Run `ployz --json ARGS` in `dir` with a lasting `home`, where directory links live.
fn in_dir(
    store: &Target,
    home: &std::path::Path,
    dir: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> (Option<i32>, Value) {
    let output = store
        .command(home)
        .current_dir(dir)
        .envs(env.iter().copied())
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

/// `ployz up` in a directory without Git creates a Project named after it, links it,
/// adds a Service (on Cloud with a generated domain), uploads it and deploys. Again,
/// it reuses all of that; an unlinked directory of the same name is refused.
#[test]
fn up_ships_a_directory_without_git() {
    // Adding a Server needs a signed-in device, and is checked before anything else.
    let (code, unsigned) = ployz(None, &["up", "--server", "root@192.0.2.1"]);
    assert_eq!(code, Some(1), "{unsigned}");
    assert_eq!(unsigned["error"]["details"]["next"], json!("ployz login"));

    let args = [&["up"][..], &UNREACHABLE_SERVER].concat();
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = first.path().canonicalize().unwrap().join("My Shop");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\nEXPOSE 80\n").unwrap();

        let (code, up) = in_dir(store, home.path(), &dir, &args, &[]);
        assert_eq!(code, Some(1), "{up}");
        assert_eq!(up["directory"], json!(dir.to_str().unwrap()));
        let deployment = &up["deployment"];
        assert_eq!(deployment["environment"]["project"], json!("my-shop"));
        assert_eq!(deployment["environment"]["name"], json!("production"));
        assert_eq!(deployment["upload"]["base"], json!(null), "{up}");
        let id = deployment["id"].as_str().unwrap();
        assert_eq!(
            up["next"],
            json!(format!("ployz deployment show {}", deployment["number"]))
        );
        match store {
            Target::Local(_) => {
                assert_eq!(up["urls"], json!([]));
                assert_eq!(up.get("dashboard"), None);
            }
            Target::Cloud { url, .. } => {
                assert_eq!(up["urls"], json!(["https://my-shop.acme.ployz.app"]));
                assert_eq!(
                    up["dashboard"],
                    json!(format!("{url}/cloud/alice/my-shop/production"))
                );
                assert!(UPLOADS.lock().unwrap().contains_key(id));
                // The domain reaches the Dockerfile's EXPOSEd port.
                let (_, domains) = in_dir(store, home.path(), &dir, &["domain", "ls"], &[]);
                assert_eq!(domains["domains"][0]["port"], json!(80), "{domains}");
            }
        }

        let (code, again) = in_dir(store, home.path(), &dir, &args, &[]);
        assert_eq!(code, Some(1), "{again}");
        assert_eq!(
            again["deployment"]["environment"]["project"],
            json!("my-shop")
        );
        let (_, services) = in_dir(store, home.path(), &dir, &["service", "ls"], &[]);
        assert_eq!(
            services["services"].as_array().map(Vec::len),
            Some(1),
            "{services}"
        );
        let (_, method) = in_dir(
            store,
            home.path(),
            &dir,
            &["get", "my-shop.buildMethod"],
            &[],
        );
        assert_eq!(
            method["settings"][0]["value"],
            json!("dockerfile"),
            "{method}"
        );
        // A one-off `--env` leaves the directory's link alone.
        in_dir(store, home.path(), &dir, &["env", "new", "staging"], &[]);
        let staging = [&args[..], &["--env", "staging"]].concat();
        let (_, other_env) = in_dir(store, home.path(), &dir, &staging, &[]);
        assert_eq!(
            other_env["deployment"]["environment"]["name"],
            json!("staging"),
            "{other_env}"
        );
        let (_, status) = in_dir(store, home.path(), &dir, &["status"], &[]);
        assert_eq!(
            status.pointer("/environment/name"),
            Some(&json!("production")),
            "{status}"
        );

        let other = second.path().join("My Shop");
        std::fs::create_dir(&other).unwrap();
        let events = home.path().join("events.jsonl");
        let empty_args = [&args[..], &["--events", events.to_str().unwrap()]].concat();
        let (code, empty) = in_dir(store, home.path(), &other, &empty_args, &[]);
        assert_eq!(code, Some(2), "{empty}");
        assert!(
            empty["error"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Nothing to deploy: ")),
            "{empty}"
        );
        assert!(!events.exists(), "a refused up leaves --events alone");
        std::fs::write(other.join("Dockerfile"), "FROM scratch\n").unwrap();
        let (code, taken) = in_dir(store, home.path(), &other, &args, &[]);
        assert_eq!(code, Some(1), "{taken}");
        assert_eq!(
            taken["error"]["details"]["next"],
            json!("ployz up --project my-shop")
        );
    }
}

/// `ployz up --project` into a Project whose Services all pull images adds the
/// directory's uploaded Service and builds it, as in a new Project (#2863). A name
/// already taken by one of those Services is refused, adding nothing.
#[test]
fn up_into_an_existing_project_adds_the_directory_s_service() {
    let args = [&["up", "--project", "shop"][..], &UNREACHABLE_SERVER].concat();
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().canonicalize().unwrap().join("web");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\nEXPOSE 80\n").unwrap();
        in_dir(store, home.path(), &dir, &["project", "new", "shop"], &[]);
        in_dir(
            store,
            home.path(),
            &dir,
            &[
                "service",
                "add",
                "db",
                "--project",
                "shop",
                "--image",
                "postgres:16-alpine",
            ],
            &[],
        );

        let (code, up) = in_dir(store, home.path(), &dir, &args, &[]);
        assert_eq!(code, Some(1), "{up}");
        let (_, services) = in_dir(
            store,
            home.path(),
            &dir,
            &["service", "ls", "--project", "shop"],
            &[],
        );
        let names: Vec<_> = services["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|service| service["name"].clone())
            .collect();
        assert_eq!(names, [json!("db"), json!("web")], "{services}");
        let (_, method) = in_dir(
            store,
            home.path(),
            &dir,
            &["get", "web.buildMethod", "--project", "shop"],
            &[],
        );
        assert_eq!(
            method["settings"][0]["value"],
            json!("dockerfile"),
            "{method}"
        );

        let taken = root.path().canonicalize().unwrap().join("db");
        std::fs::create_dir(&taken).unwrap();
        std::fs::write(taken.join("Dockerfile"), "FROM scratch\n").unwrap();
        let fresh = tempfile::tempdir().unwrap();
        in_dir(
            store,
            fresh.path(),
            &taken,
            &["project", "new", "other"],
            &[],
        );
        in_dir(
            store,
            fresh.path(),
            &taken,
            &[
                "service",
                "add",
                "db",
                "--project",
                "other",
                "--image",
                "postgres:16-alpine",
            ],
            &[],
        );
        let (code, refused) = in_dir(
            store,
            fresh.path(),
            &taken,
            &[&["up", "--project", "other"][..], &UNREACHABLE_SERVER].concat(),
            &[],
        );
        assert_eq!(code, Some(2), "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|message| message.starts_with("Nothing would build this directory")),
            "{refused}"
        );
        let (_, status) = in_dir(store, fresh.path(), &taken, &["status"], &[]);
        assert_ne!(
            status.pointer("/environment/project"),
            Some(&json!("other")),
            "a refused up leaves the directory unlinked: {status}"
        );
    }
}

/// A Branch's copy of a Service `ployz up` uploaded has no source until an upload,
/// so the Branch names `ployz up --env`, which ships it; `deploy` there is refused.
#[test]
fn a_branch_of_an_uploaded_service_names_up_to_ship_it() {
    let up = [&["up"][..], &UNREACHABLE_SERVER].concat();
    for store in &targets() {
        let (home, root) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = root.path().canonicalize().unwrap().join("up-local");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\nEXPOSE 80\n").unwrap();
        let run = |args: &[&str]| in_dir(store, home.path(), &dir, args, &[]);
        let (code, shipped) = run(&up);
        assert_eq!(code, Some(1), "{shipped}");

        let (code, made) = run(&["env", "branch", "staging", "--copy", "up-local"]);
        assert_eq!(code, Some(0), "{made}");
        assert_eq!(made["next"], json!("ployz up --env staging"));
        let deploy = [&["deploy", "--env", "staging"][..], &UNREACHABLE_SERVER].concat();
        let (code, refused) = run(&deploy);
        assert_eq!(code, Some(2), "{refused}");
        assert!(
            refused["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("has nothing to run yet")),
            "{refused}"
        );
        let (code, staging) = run(&[&up[..], &["--env", "staging"]].concat());
        assert_eq!(code, Some(1), "{staging}");
        assert_eq!(
            staging["deployment"]["environment"]["name"],
            json!("staging"),
            "{staging}"
        );
    }
}

/// `config mount` written the way `config add --mount` takes it names the order it takes,
/// before it looks up the Service or the Config.
#[test]
fn a_config_mount_written_like_add_names_the_order_mount_takes() {
    for store in &targets() {
        for args in [
            ["config", "mount", "app", "web:/etc/app"],
            ["config", "mount", "web:/etc/app", "app"],
        ] {
            let refused = error(store, &args);
            assert_eq!(
                refused["message"],
                json!("config mount takes SERVICE CONFIG DIR: ployz config mount web app /etc/app"),
                "{args:?}"
            );
        }
    }
}

#[test]
fn two_linked_directories_act_on_their_own_environments() {
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let a_dir = a.path().canonicalize().unwrap();
        let nested = a_dir.join("src");
        std::fs::create_dir(&nested).unwrap();
        let run = |dir: &std::path::Path, args: &[&str], env: &[(&str, &str)]| {
            let (code, json) = in_dir(store, home.path(), dir, args, env);
            assert_eq!(code, Some(0), "{args:?}: {json}");
            json
        };
        ok(store, &["project", "new", "shop"]);
        ok(store, &["env", "new", "staging"]);

        let linked = run(&a_dir, &["link", "--env", "staging"], &[]);
        assert_eq!(
            linked.get("directory"),
            Some(&json!(a_dir.to_str().unwrap()))
        );
        assert_eq!(linked.get("project"), Some(&json!("shop")));
        assert_eq!(linked.get("environment"), Some(&json!("staging")));
        assert_eq!(linked.get("next"), Some(&json!("ployz status")));
        let linked = run(b.path(), &["link"], &[]);
        assert_eq!(linked.get("environment"), Some(&json!("production")));

        // A subdirectory acts where its linked ancestor does.
        let added = run(&nested, &["service", "add", "api", "--image", "api:1"], &[]);
        assert_eq!(added.pointer("/environment/name"), Some(&json!("staging")));
        let got = run(b.path(), &["get"], &[]);
        assert_eq!(got.pointer("/environment/name"), Some(&json!("production")));
        assert_eq!(got.get("settings"), Some(&json!([])));

        let renamed = run(b.path(), &["project", "rename", "shop", "store"], &[]);
        assert_eq!(renamed.get("links"), Some(&json!(2)));
        let got = run(b.path(), &["get"], &[]);
        assert_eq!(got.pointer("/environment/project"), Some(&json!("store")));
        run(b.path(), &["project", "rename", "store", "shop"], &[]);

        let status = run(&nested, &["status"], &[]);
        assert_eq!(status.pointer("/environment/name"), Some(&json!("staging")));
        assert_eq!(
            status.pointer("/scope/environment"),
            Some(&json!({ "name": "staging", "source": "link" }))
        );
        assert_eq!(
            status.pointer("/scope/link"),
            Some(&json!(a_dir.to_str().unwrap()))
        );
        assert_eq!(status.pointer("/staged/saved"), Some(&json!(false)));
        assert_eq!(status.get("deploying"), Some(&json!([])));
        assert_eq!(status.get("attention"), Some(&json!([])));
        assert_eq!(status.get("next"), Some(&json!("ployz diff")));
        let organization = match store {
            Target::Local(_) => ("local", "local"),
            Target::Cloud { .. } => ("token", "alice"),
        };
        assert_eq!(
            status.pointer("/identity/credential"),
            Some(&json!(organization.0))
        );
        assert_eq!(
            status.pointer("/identity/organization/slug"),
            Some(&json!(organization.1))
        );

        // Flags beat environment variables, which beat the link.
        let by_env = run(&a_dir, &["status"], &[("PLOYZ_ENV", "production")]);
        assert_eq!(
            by_env.pointer("/scope/environment"),
            Some(&json!({ "name": "production", "source": "env" }))
        );
        assert_eq!(
            by_env.pointer("/scope/project/source"),
            Some(&json!("link"))
        );
        let by_flag = run(
            &a_dir,
            &["status", "--env", "staging"],
            &[("PLOYZ_ENV", "production")],
        );
        assert_eq!(
            by_flag.pointer("/scope/environment"),
            Some(&json!({ "name": "staging", "source": "flag" }))
        );

        let context = run(&nested, &["ctx"], &[]);
        assert_eq!(
            context.pointer("/environment/name"),
            Some(&json!("staging"))
        );
        assert_eq!(context.pointer("/project/source"), Some(&json!("link")));
        let via = match store {
            Target::Local(_) => "local",
            Target::Cloud { .. } => "cloud",
        };
        assert_eq!(context.pointer("/servers/via"), Some(&json!(via)));
    }
}

#[test]
fn missing_ambiguous_and_foreign_scope_name_the_fix() {
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| in_dir(store, home.path(), dir.path(), args, &[]);

        let (code, status) = run(&["status"]);
        assert_eq!(code, Some(0), "{status}");
        assert_eq!(status.get("environment"), Some(&Value::Null));
        assert_eq!(
            status.pointer("/attention/0/reason"),
            Some(&json!("not_found"))
        );
        assert_eq!(status.get("next"), Some(&json!("ployz project new NAME")));
        let human = store
            .command(home.path())
            .current_dir(dir.path())
            .arg("status")
            .output()
            .unwrap();
        assert!(human.status.success());
        let human = String::from_utf8(human.stdout).unwrap();
        match store {
            Target::Cloud { url, .. } => {
                assert!(human.contains(&format!("cloud = {url}")), "{human}")
            }
            Target::Local(_) => assert!(!human.contains("cloud ="), "{human}"),
        }
        let (code, refused) = run(&["link"]);
        assert_eq!(code, Some(1));
        assert_eq!(refused.pointer("/error/code"), Some(&json!("not_found")));

        ok(store, &["project", "new", "shop"]);
        ok(store, &["project", "new", "blog"]);
        let (_, status) = run(&["status"]);
        assert_eq!(
            status.pointer("/attention/0/reason"),
            Some(&json!("ambiguous"))
        );
        assert_eq!(
            status.get("next"),
            Some(&json!("ployz link --project PROJECT"))
        );
        let (code, refused) = run(&["link"]);
        assert_eq!(code, Some(2));
        assert_eq!(
            refused.pointer("/error/details/next"),
            Some(&json!("ployz link --project PROJECT"))
        );
        let (_, refused) = run(&["link", "--project", "blog", "--env", "preview"]);
        assert_eq!(
            refused.pointer("/error/details/next"),
            Some(&json!("ployz env ls --project blog"))
        );

        ok(store, &["env", "new", "staging", "--project", "blog"]);
        let (code, _) = run(&["link", "--project", "blog", "--env", "staging"]);
        assert_eq!(code, Some(0));
        // Another Project on the command line drops the link's Environment.
        let (_, status) = run(&["status", "--project", "shop"]);
        assert_eq!(status.pointer("/environment/project"), Some(&json!("shop")));
        assert_eq!(
            status.pointer("/environment/name"),
            Some(&json!("production"))
        );
        assert_eq!(status.pointer("/scope/environment"), Some(&Value::Null));
        assert_eq!(status.pointer("/scope/link"), Some(&Value::Null));
    }

    // A link made in another Organization is refused, never followed.
    let store = &targets()[0];
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    ok(store, &["project", "new", "shop"]);
    let (code, _) = in_dir(store, home.path(), dir.path(), &["link"], &[]);
    assert_eq!(code, Some(0));
    let links = home.path().join("links.json");
    let moved = std::fs::read_to_string(&links)
        .unwrap()
        .replace("\"local\"", "\"acme\"");
    std::fs::write(&links, moved).unwrap();
    let (code, refused) = in_dir(store, home.path(), dir.path(), &["get"], &[]);
    assert_eq!(code, Some(1));
    assert_eq!(refused.pointer("/error/code"), Some(&json!("conflict")));
    assert_eq!(
        refused.pointer("/error/details/next"),
        Some(&json!("ployz link"))
    );
}

/// `ployz ctx` in an unlinked directory, as humans read it, and its JSON.
fn ctx(store: &Target, home: &std::path::Path, dir: &std::path::Path) -> (String, String, Value) {
    let human = store
        .command(home)
        .current_dir(dir)
        .arg("ctx")
        .output()
        .unwrap();
    assert!(human.status.success(), "{human:?}");
    let (_, json) = in_dir(store, home, dir, &["ctx"], &[]);
    (
        String::from_utf8(human.stdout).unwrap(),
        String::from_utf8(human.stderr).unwrap(),
        json,
    )
}

#[test]
fn ctx_asks_the_store_what_an_unlinked_directory_acts_on() {
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();

        ok(store, &["project", "new", "shop"]);
        let (stdout, stderr, json) = ctx(store, home.path(), dir.path());
        assert!(
            stdout.contains("project = shop (the only Project)"),
            "{stdout}"
        );
        assert!(
            stdout.contains("environment = production (the Default Environment)"),
            "{stdout}"
        );
        assert!(!stderr.contains("next:"), "{stderr}");
        assert_eq!(json.get("project"), Some(&Value::Null));
        assert_eq!(
            json.get("default"),
            Some(&json!({ "projects": 1, "project": "shop", "environment": "production" }))
        );
        assert_eq!(json.get("next"), None);

        ok(store, &["project", "new", "blog"]);
        let (stdout, stderr, json) = ctx(store, home.path(), dir.path());
        assert!(
            stdout.contains("project = (none: this Organization has 2 Projects)"),
            "{stdout}"
        );
        assert!(stdout.contains("environment = (none)"), "{stdout}");
        assert!(
            stderr.contains("next: ployz link --project PROJECT"),
            "{stderr}"
        );
        assert_eq!(json.get("default"), Some(&json!({ "projects": 2 })));
        assert_eq!(
            json.get("next"),
            Some(&json!("ployz link --project PROJECT"))
        );

        // A linked Project needs no listing.
        let (code, _) = in_dir(
            store,
            home.path(),
            dir.path(),
            &["link", "--project", "blog"],
            &[],
        );
        assert_eq!(code, Some(0));
        let (code, json) = in_dir(store, home.path(), dir.path(), &["ctx"], &[]);
        assert_eq!(code, Some(0));
        assert_eq!(json.get("default"), None);
        assert_eq!(json.get("next"), None);
    }
}

/// Offline, or when Cloud is slow to answer, `ployz ctx` says nothing is linked.
#[test]
fn ctx_offline_says_nothing_is_linked() {
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let held = format!("http://{}", silent.local_addr().unwrap());
    std::thread::spawn(move || {
        let held: Vec<TcpStream> = silent.incoming().map_while(Result::ok).collect();
        drop(held);
    });
    for url in ["http://127.0.0.1:9".to_owned(), held] {
        let store = Target::Cloud {
            url,
            token: "ployz_alice",
        };
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let (stdout, stderr, json) = ctx(&store, home.path(), dir.path());
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            started.elapsed()
        );
        assert!(stdout.contains("project = (none)\n"), "{stdout}");
        assert!(stdout.contains("environment = (none)\n"), "{stdout}");
        assert!(
            stderr.contains("next: ployz link --project PROJECT"),
            "{stderr}"
        );
        assert_eq!(json.get("default"), None);
        assert_eq!(
            json.get("next"),
            Some(&json!("ployz link --project PROJECT"))
        );
    }
}

/// With a config file that can't pick a context, commands reach no Server, and `ployz ctx`
/// says so and names the fix the failing command would.
#[test]
fn ctx_names_no_servers_when_the_config_picks_no_context() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    for (config, shown, next) in [
        (
            "contexts:\n  lab:\n    connections:\n      - unix: /tmp/MY-SERVER.sock\n",
            "servers = (none: no current context)\n",
            "ployz ctx use CONTEXT",
        ),
        (
            "contexts: {}\n",
            "servers = (none: no contexts)\n",
            "ployz login",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.yaml"), config).unwrap();

        let (code, listed) = in_dir(&store, home.path(), dir.path(), &["server", "ls"], &[]);
        assert_ne!(code, Some(0), "{listed}");

        let (stdout, stderr, json) = ctx(&store, home.path(), dir.path());
        assert!(stdout.contains(shown), "{stdout}");
        assert!(stderr.contains(&format!("next: {next}")), "{stderr}");
        assert_eq!(json.get("servers"), Some(&json!({ "via": "none" })));
        assert_eq!(json.get("next"), Some(&json!(next)));
    }
}

/// A config file ctx can't read is an error, as it is for every command, not `via local`.
#[test]
fn ctx_refuses_a_malformed_config() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.yaml"), "contexts: [\n").unwrap();

    let (code, output) = in_dir(&store, home.path(), dir.path(), &["ctx"], &[]);
    assert_ne!(code, Some(0), "{output}");
    assert_eq!(output.get("servers"), None, "{output}");
}

#[test]
fn a_repository_service_is_checked_by_cloud() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        if let Target::Local(_) = store {
            // The hidden local Store can't ask GitHub, so it connects no repository.
            let refused = error(store, &["service", "add", "web", "--repo", "acme/web"]);
            assert_eq!(refused["code"], "not_found", "{refused}");
            assert_eq!(refused["details"]["next"], "ployz github connect");
            continue;
        }
        let created = ok(store, &["service", "add", "web", "--repo", "acme/web@dev"]);
        assert!(
            created["staged"]
                .as_array()
                .unwrap()
                .contains(&json!("web.branch")),
            "{created}"
        );
        let got = ok(store, &["get", "web"]);
        assert_eq!(got["values"]["repository"], "acme/web");
        assert_eq!(got["values"]["branch"], "dev");

        let gone = error(store, &["service", "add", "api", "--repo", "acme/web@gone"]);
        assert_eq!(
            gone["details"]["next"], "ployz github ls acme/web",
            "{gone}"
        );
        assert!(!gone.to_string().contains("gone"), "{gone}");
        let other = error(store, &["service", "add", "api", "--repo", "acme/other"]);
        assert_eq!(other["details"]["next"], "ployz github connect", "{other}");
        let usage = failed(
            store,
            &[
                "service", "add", "api", "--repo", "acme/web", "--image", "nginx",
            ],
            2,
        );
        assert_eq!(usage["code"], "invalid_argument");

        ok(store, &["set", "web.branch=main", "web.rootDir=/apps/web"]);
        let diff = ok(store, &["diff"]);
        assert!(diff.to_string().contains("web.source"), "{diff}");

        ok(store, &["service", "add", "blank"]);
        ok(store, &["set", "blank.repository=acme/web"]);
        let got = ok(store, &["get", "blank"]);
        assert_eq!(got["values"]["branch"], "main", "{got}");
        ok(store, &["unset", "blank.repository"]);
        let got = ok(store, &["get", "blank"]);
        assert_eq!(got["values"].get("repository"), None, "{got}");
    }
}

#[test]
fn github_lists_branches_reads_files_and_disconnects_in_cloud() {
    let [_, cloud] = targets();
    let listed = ok(&cloud, &["github", "ls"]);
    assert_eq!(listed["repositories"][0]["repository"], "acme/web");
    assert!(listed.get("next").is_none(), "{listed}");
    let branches = ok(&cloud, &["github", "ls", "acme/web"]);
    assert_eq!(branches["branches"], json!(["dev", "main"]));
    let missing = error(&cloud, &["github", "ls", "acme/nope"]);
    assert_eq!(missing["code"], "not_found");
    assert_eq!(missing["details"]["valid_children"], json!(["acme/web"]));
    assert_eq!(
        failed(&cloud, &["github", "ls", "not a repo"], 2)["code"],
        "invalid_argument"
    );
    let tree = ok(
        &cloud,
        &[
            "github", "tree", "acme/web", "src", "--ref", "dev", "--match", "**/*.rs",
        ],
    );
    assert_eq!(tree["paths"], json!(["src/main.rs"]));
    let file = ok(&cloud, &["github", "cat", "acme/web", "Dockerfile"]);
    assert_eq!(file["content"], "FROM alpine\n");
    let directory = error(&cloud, &["github", "cat", "acme/web", "src"]);
    assert_eq!(directory["code"], "invalid_argument", "{directory}");
    assert_eq!(
        directory["message"],
        "src is a directory: list it with github tree."
    );
    assert_eq!(
        error(&cloud, &["github", "cat", "acme/nope", "Dockerfile"])["code"],
        "not_found"
    );
    assert_eq!(listed["installations"][0]["account"], "acme");
    let removed = ok(&cloud, &["github", "disconnect", "acme"]);
    assert_eq!(removed["disconnected"]["account"], "acme");
    assert_eq!(
        error(&cloud, &["github", "disconnect", "bob-gh"])["code"],
        "not_found"
    );
    assert_eq!(
        failed(&cloud, &["github", "disconnect", "acme/web"], 2)["code"],
        "invalid_argument"
    );
    let pending = ok(&cloud, &["github", "connect"]);
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["next"], "ployz github connect --wait");
}

/// `NODE`, or `NODE.name`: a row as reads name it.
fn label(row: &Value) -> String {
    match row["name"].as_str() {
        Some(name) => format!("{}.{name}", row["node"].as_str().unwrap()),
        None => row["node"].as_str().unwrap().to_owned(),
    }
}

fn labels(rows: &Value) -> Vec<String> {
    rows.as_array().unwrap().iter().map(label).collect()
}

fn ployz_with_stdin(store: &Target, args: &[&str], input: &str) -> (Option<i32>, String) {
    let home = tempfile::tempdir().unwrap();
    let mut command = store.command(home.path());
    command
        .arg("--json")
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
fn secrets_arrive_on_stdin_or_an_env_file_and_never_print() {
    let store = &cloud_target();
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "nginx:1"]);
    ok(store, &["service", "add", "api", "--image", "nginx:1"]);

    let set = ok(
        store,
        &[
            "set",
            "web.env.db_host=db",
            "api.env.URL=http://${{ web.DB_HOST }}",
        ],
    );
    assert_eq!(set["staged"], json!(["web.env.DB_HOST", "api.env.URL"]));
    assert_eq!(set["next"], json!("ployz deploy"));
    let (code, sealed) = ployz_with_stdin(store, &["set", "web.env.TOKEN", "--secret"], "s3cr3t\n");
    assert_eq!(code, Some(0), "{sealed}");
    assert!(!sealed.contains("s3cr3t"));
    assert_eq!(
        ok(store, &["get", "web.env.TOKEN"])["settings"][0]["value"],
        json!({ "secret": true })
    );
    let refused = error(store, &["set", "web.env.TOKEN=plain-text"]);
    assert_eq!(refused["code"], json!("conflict"));
    assert!(!refused.to_string().contains("plain-text"));
    failed(store, &["set", "web.env.TOKEN=x", "--secret"], 2);
    failed(
        store,
        &[
            "set",
            "web",
            "--patch",
            r#"{"env":{"TOKEN":{"secret":"x"}}}"#,
        ],
        2,
    );

    // A variable that is secret stays secret; the others are plain.
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        "\u{feff}# app\nexport QUOTED=\"a b\\n\" # comment\nRAW='lit ${{ LEVEL }} # inside' # comment\nTOKEN=rotated-s3cr3t\nLEVEL=info # comment\nexport\tTAB_EXPORT=works\nTAB_COMMENT=first\t# ignored # later\n",
    )
    .unwrap();
    let path = file.path().to_str().unwrap();
    let imported = ok(store, &["set", "web", "--from-env-file", path]);
    assert_eq!(
        imported["staged"],
        json!([
            "web.env.QUOTED",
            "web.env.RAW",
            "web.env.TOKEN",
            "web.env.LEVEL",
            "web.env.TAB_EXPORT",
            "web.env.TAB_COMMENT"
        ])
    );
    let env = ok(store, &["get", "web"])["values"]["env"].clone();
    assert_eq!(
        env,
        json!({
            "DB_HOST": "db",
            "LEVEL": "info",
            "QUOTED": "a b\n",
            "RAW": "lit ${{ LEVEL }} # inside",
            "TOKEN": { "secret": true },
            "TAB_EXPORT": "works",
            "TAB_COMMENT": "first",
        })
    );
    failed(
        store,
        &["set", "web", "--from-env-file", "/nonexistent/.env"],
        2,
    );

    let before_invalid = ok(store, &["diff"]);
    for (invalid, why) in [
        (
            "\"unterminated-test-secret",
            "unterminated double-quoted value",
        ),
        (
            "'unterminated-test-secret",
            "unterminated single-quoted value",
        ),
        (
            "\"test-secret\" trailing",
            "unexpected text after the closing quote",
        ),
    ] {
        std::fs::write(file.path(), format!("LEVEL=changed\nTOKEN={invalid}\n")).unwrap();
        let refused = failed(store, &["set", "web", "--from-env-file", path], 2);
        assert_eq!(refused["code"], "invalid_argument");
        assert_eq!(refused["message"], format!("Env file line 2: {why}"));
        assert!(!refused.to_string().contains("test-secret"));
        assert_eq!(ok(store, &["diff"]), before_invalid);
    }
    std::fs::write(file.path(), "LEVEL=changed\nTOKEN=\"test\u{0}secret\"\n").unwrap();
    let refused = failed(store, &["set", "web", "--from-env-file", path], 2);
    assert_eq!(
        refused["message"],
        "Env file line 2: null characters are not allowed"
    );
    assert_eq!(ok(store, &["diff"]), before_invalid);

    for key in [
        "DB_HOST.exported",
        "",
        "1KEY",
        "KEY-",
        "KEY\u{0}",
        "KEY\u{feff}",
    ] {
        std::fs::write(file.path(), format!("LEVEL=changed\n{key}=true\n")).unwrap();
        let refused = failed(store, &["set", "web", "--from-env-file", path], 2);
        assert_eq!(
            refused["message"],
            "Env file line 2: a variable name is letters, digits and _, not starting with a digit"
        );
        assert_eq!(ok(store, &["diff"]), before_invalid);
    }

    let diff = ok(store, &["diff"]).to_string();
    assert!(!diff.contains("s3cr3t"), "{diff}");
    let deployed = no_server_answers(store, &["deploy"]);
    assert!(!deployed.to_string().contains("s3cr3t"), "{deployed}");
}

#[test]
fn a_synced_secret_arrives_not_set_and_never_blocks_a_deploy() {
    let store = &cloud_target();
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "web:1"]);
    ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
    let secret = ["set", "web.env.API_KEY", "--secret"];
    let (code, set) = ployz_with_stdin(
        store,
        &[&secret[..], &["--env", "fix-web"]].concat(),
        "test-key\n",
    );
    assert_eq!(code, Some(0), "{set}");
    let plan = ok(
        store,
        &["env", "sync", "--to", "--plan", "--env", "fix-web"],
    );
    let row = &plan["rows"][0];
    assert_eq!(
        (label(row), &row["from"], &row["secret"]),
        (
            "web.env.API_KEY".to_owned(),
            &json!({ "secret": true }),
            &json!({})
        )
    );
    ok(store, &["env", "sync", "--to", "--env", "fix-web"]);

    // Not set, it never blocks: production's Deploy is admitted and skips it.
    let (_, deployed) = ployz(Some(store), &["deploy"]);
    assert_eq!(deployed["number"], json!(1), "{deployed}");
    assert!(!deployed.to_string().contains("ployz set"), "{deployed}");
    let (code, set) = ployz_with_stdin(store, &secret, "prod-key\n");
    assert_eq!(code, Some(0), "{set}");
    let (_, deployed) = ployz(Some(store), &["deploy"]);
    assert_eq!(deployed["number"], json!(2), "{deployed}");

    // A sync can give the receiver its value, a line of stdin per --value.
    ok(store, &["env", "branch", "fix-key", "--copy", "web"]);
    let other = ["set", "web.env.OTHER_KEY", "--secret", "--env", "fix-key"];
    let (code, set) = ployz_with_stdin(store, &other, "branch-key\n");
    assert_eq!(code, Some(0), "{set}");
    let to = ["env", "sync", "--to", "--env", "fix-key"];
    let (code, short) = ployz_with_stdin(
        store,
        &[
            &to[..],
            &["--value", "web.env.OTHER_KEY", "--value", "web.image"],
        ]
        .concat(),
        "given-key\n",
    );
    assert_eq!(code, Some(2), "{short}");
    let (code, synced) = ployz_with_stdin(
        store,
        &[&to[..], &["--value", "web.env.OTHER_KEY"]].concat(),
        "given-key\n",
    );
    assert_eq!(code, Some(0), "{synced}");
    assert!(!synced.contains("given-key"), "{synced}");
    let (_, deployed) = ployz(Some(store), &["deploy"]);
    assert_eq!(deployed["number"], json!(3), "{deployed}");
}

#[test]
fn a_private_image_credential_arrives_on_stdin_and_rotates_at_once() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(
            store,
            &["service", "add", "web", "--image", "ghcr.io/acme/web:1"],
        );
        let path = "web.registryCredential";
        let (code, first) = ployz_with_stdin(store, &["set", path, "--secret"], "first-token\n");
        assert_eq!(code, Some(0), "{first}");
        let first: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["staged"], json!([path]));
        assert_eq!(first["immediate"], json!([path]));
        assert_eq!(
            ok(store, &["get", path])["settings"][0]["value"],
            json!({ "secret": true })
        );
        let body = r#"{"registryCredential":{"username":"octocat","secret":"second-token"}}"#;
        let (code, rotated) = ployz_with_stdin(store, &["set", "web", "--patch", "-"], body);
        assert_eq!(code, Some(0), "{rotated}");
        let rotated: Value = serde_json::from_str(&rotated).unwrap();
        assert_eq!(rotated["staged"], json!([]));
        assert_eq!(rotated["immediate"], json!([path]));
        failed(store, &["set", "web", "--patch", body], 2);
        let refused = error(store, &["set", "web.registryCredential=plain-token"]);
        assert_eq!(refused["code"], json!("invalid_argument"));
        let shown = [
            refused,
            ok(store, &["get", "web"]),
            ok(store, &["diff"]),
            ok(store, &["explain", path]),
        ];
        for read in shown {
            assert!(!read.to_string().contains("-token"), "{read}");
        }
    }
}

/// Over Cloud, `deploy --upload` hands Cloud the directory before admitting the
/// Deployment it's for; Cloud names the uploader. With no upload to build from, the
/// Deploy is refused up front.
#[test]
fn an_agent_uploads_a_directory_to_cloud_and_is_told_when_to_upload_again() {
    let cloud = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "app"]);
    // Nothing to build from: the Store refuses before anything queues.
    let never = error(&cloud, &["deploy"]);
    assert_eq!(never["details"]["service"], json!("app"), "{never}");

    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("Dockerfile"), "FROM scratch\n").unwrap();
    let dir = source.path().to_str().unwrap();
    let uploaded = no_server_answers(&cloud, &["deploy", "--upload", dir]);
    assert_eq!(uploaded["upload"]["uploader"], json!("alice"));
    let id = uploaded["id"].as_str().unwrap();
    let archive = UPLOADS.lock().unwrap().get(id).cloned().unwrap();
    assert_eq!(archive.get(..2), Some(&[0x1f, 0x8b][..]), "a gzip");
}

#[test]
fn an_agent_reads_and_sets_the_build_order_at_once() {
    for store in &targets() {
        let auto = ok(store, &["org", "build-order"]);
        assert_eq!(auto["build_order"], Value::Null);
        assert_eq!(auto["builders"], json!(["github", "servers"]));
        assert!(auto.get("immediate").is_none());

        let set = ok(store, &["org", "build-order", "servers-only"]);
        assert_eq!(set["build_order"], "servers-only");
        assert_eq!(set["builders"], json!(["servers"]));
        assert_eq!(set["immediate"], true);
        assert_eq!(
            ok(store, &["org", "build-order"])["build_order"],
            "servers-only"
        );

        assert_eq!(
            ok(store, &["org", "build-order", "auto"])["build_order"],
            Value::Null
        );
        let (code, _) = ployz(Some(store), &["org", "build-order", "gitlab-first"]);
        assert_eq!(code, Some(2));
    }
}

/// Run Deployment `id` to success, as a runner that reached the Servers would.
fn succeed(store: &ConfigStore, id: &ployz_store::DeploymentId) {
    let runner = RunnerId::parse("successful-worker").unwrap();
    let claimed = store.claim(id, &runner).unwrap();
    let preview = serde_json::from_value(json!({
        "namespace": claimed.intent.namespace, "operations": [],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(id, &runner, ployz_store::RunEvidence::Prepared(preview))
        .unwrap();
    let outcome = serde_json::from_value(json!({"type": "success", "completed": []})).unwrap();
    store
        .record(
            id,
            &runner,
            ployz_store::RunEvidence::Executed {
                progress: Vec::new(),
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
}

/// Deploy `environment` of Project shop and have it applied: Cloud's worker runs it;
/// locally the test runs it as the runner would.
fn applied(store: &Target, environment: &str) {
    match store {
        Target::Cloud { .. } => {
            ok(store, &["deploy", "--env", environment]);
        }
        Target::Local(dir) => {
            let path = dir.path().join("store.db");
            let key = SealingKey::from_file(&dir.path().join("store.db.key")).unwrap();
            let local = ConfigStore::open(&format!("sqlite:{}", path.display()), key).unwrap();
            let who = Actor::system(OrganizationId::parse("local").unwrap());
            let id = ployz_store::DeploymentId::parse(uuid::Uuid::new_v4().to_string()).unwrap();
            let deploy = ployz_store::Admit::Deploy(ployz_store::Deploy {
                id: id.clone(),
                environment: ployz_store::EnvironmentRef {
                    project: Some(ployz_store::ProjectName::parse("shop").unwrap()),
                    environment: Some(ployz_store::EnvironmentName::parse(environment).unwrap()),
                },
                services: Vec::new(),
                version: None,
                upload: None,
                accept_volume_loss: Vec::new(),
                message: None,
                prune: false,
                no_prune: false,
            });
            local
                .write_trusted(&who, &deploy, &Trusted::default())
                .unwrap();
            succeed(&local, &id);
        }
    }
}

#[test]
fn a_branch_follows_its_parent_and_diff_shows_where_changes_came_from_and_the_hints() {
    fn succeeding(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        if let Written::Deployment(admitted) = written {
            succeed(store, &admitted.id);
        }
    }
    let targets = [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud_with_dispatch(succeeding),
            token: "ployz_alice",
        },
    ];
    for store in &targets {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        applied(store, "production");
        ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
        // fix-web's own, undeployed change; production changes it too, and adds one.
        ok(store, &["set", "web.image=web:mine", "--env", "fix-web"]);
        ok(store, &["set", "web.image=web:2"]);
        ok(store, &["set", "web.env.NEW=1"]);
        applied(store, "production");

        let diff = ok(store, &["diff", "--env", "fix-web"]);
        assert_eq!(labels(&diff["incoming"]), ["web.env.NEW"], "{diff}");
        assert_eq!(diff["incoming"][0]["from"], json!("production"));
        assert_eq!(labels(&diff["follow_hints"]), ["web.source"]);
        assert_eq!(
            (
                &diff["follow_hints"][0]["from"],
                &diff["follow_hints"][0]["value"]["image"]
            ),
            (&json!("production"), &json!("web:2"))
        );
        let took = ok(
            store,
            &[
                "env",
                "sync",
                "--take",
                "production",
                "--only",
                "web.source",
                "--env",
                "fix-web",
            ],
        );
        assert_eq!(took["into"]["name"], json!("fix-web"));
        assert_eq!(took["next"], json!("ployz deploy --env fix-web"));
        assert_eq!(
            ok(store, &["diff", "--env", "fix-web"])["follow_hints"],
            json!([])
        );
        let gone = error(
            store,
            &["env", "sync", "--take", "production", "--env", "fix-web"],
        );
        assert_eq!(gone["code"], json!("conflict"));
        assert_eq!(gone["details"]["next"], json!("ployz diff --env fix-web"));
    }
}

#[test]
fn diff_escapes_what_would_steer_the_terminal_in_follow_hints_and_file_names() {
    fn succeeding(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        if let Written::Deployment(admitted) = written {
            succeed(store, &admitted.id);
        }
    }
    let targets = [
        Target::Local(tempfile::tempdir().unwrap()),
        Target::Cloud {
            url: fake_cloud_with_dispatch(succeeding),
            token: "ployz_alice",
        },
    ];
    let file = tempfile::NamedTempFile::new().unwrap();
    let from = file.path().to_str().unwrap();
    for store in &targets {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["set", "web.env.MSG=one"]);
        applied(store, "production");
        ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
        ok(store, &["set", "web.env.MSG=mine", "--env", "fix-web"]);
        ok(store, &["set", "web.env.MSG=hi\u{202E}evil"]);
        applied(store, "production");
        ok(store, &["config", "add", "cfg", "--env", "fix-web"]);
        for name in ["safe\u{202E}evil.txt", "line\u{2028}break.txt"] {
            ok(
                store,
                &[
                    "config", "put", "cfg", name, "--from", from, "--env", "fix-web",
                ],
            );
        }

        let home = tempfile::tempdir().unwrap();
        let output = store
            .command(home.path())
            .args(["diff", "--env", "fix-web"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let human = String::from_utf8(output.stdout).unwrap();
        assert!(!human.contains(['\u{202E}', '\u{2028}']), "{human}");
        assert!(
            human
                .contains(r#"production deployed web.env.MSG = "hi\u{202e}evil", not staged here"#),
            "{human}"
        );
        assert!(
            human.contains(r"files: added safe\u{202e}evil.txt"),
            "{human}"
        );
        assert!(
            human.contains(r"files: added line\u{2028}break.txt"),
            "{human}"
        );
    }
}

/// A local Store whose Project shop has PR Environments on for acme/web, and ada's
/// pull request #5 open at head 1111111, with `members` the Organization's GitHub
/// accounts. Cloud reports the pull request, which this does through the Store itself.
fn opened_pull_request(members: Vec<ployz_store::GithubUserId>) -> Target {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    let local = local_store(&store);
    let who = Actor::system(OrganizationId::parse("local").unwrap());
    let shop = Some(ployz_store::ProjectName::parse("shop").unwrap());
    local
        .write_trusted(
            &who,
            &ployz_store::CreateGitService {
                id: ployz_store::ServiceLineageId::parse("00000000-0000-4000-8000-000000000003")
                    .unwrap(),
                environment: ployz_store::EnvironmentRef {
                    project: shop.clone(),
                    environment: None,
                },
                name: ployz_core::ServiceName::parse("web").unwrap(),
                repository: ployz_store::RepositoryName::parse("acme/web").unwrap(),
                branch: None,
            },
            &github(),
        )
        .unwrap();
    ok(&store, &["deploy", "--skip-deploy"]);
    local
        .write(
            &who,
            &ployz_store::SetPrPlan {
                project: shop,
                repository: ployz_store::RepositoryName::parse("acme/web").unwrap(),
                enabled: Some(true),
                start_from: Some(ployz_store::EnvironmentName::parse("production").unwrap()),
                copy: None,
                setup: None,
                remove_on_close: None,
                include_bots: None,
            },
        )
        .unwrap();
    let opened = ployz_store::PullRequest {
        repository_id: ployz_store::RepositoryId::parse(11).unwrap(),
        number: ployz_store::PullRequestNumber::parse(5).unwrap(),
        title: "Add search".into(),
        author: "ada".into(),
        author_id: Some(ployz_store::GithubUserId::parse(9001).unwrap()),
        bot: false,
        head_branch: ployz_store::BranchName::parse("search").unwrap(),
        head_repository_id: Some(ployz_store::RepositoryId::parse(11).unwrap()),
        head: ployz_store::CommitSha::parse("1".repeat(40)).unwrap(),
        target_branch: ployz_store::BranchName::parse("main").unwrap(),
        commits: 1,
        open: true,
        merge_commit: None,
        target_head: None,
        updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:00:01Z").unwrap(),
    };
    local
        .system(
            &who.organization,
            &ployz_store::SystemEvent::PullRequest(opened),
            &Trusted {
                members: Some(members),
                ..Trusted::default()
            },
        )
        .unwrap();
    drop(local);
    store
}

/// `web.env.{name}` in `local`'s `environment`, sealed for it as a device would, which
/// the in-process Store takes where it refuses plain text.
fn sealed_in_local(local: &Target, environment: &str, name: &str) {
    use base64::Engine as _;
    let store = local_store(local);
    let who = Actor::system(OrganizationId::parse("local").unwrap());
    let environment = ployz_store::EnvironmentRef {
        project: Some(ployz_store::ProjectName::parse("shop").unwrap()),
        environment: Some(ployz_store::EnvironmentName::parse(environment).unwrap()),
    };
    let web = store
        .read_trusted(
            &who,
            &ployz_store::ServiceQuery {
                environment: environment.clone(),
                service: ployz_core::ServiceName::parse("web").unwrap(),
            },
            &github(),
        )
        .unwrap();
    let sealed = base64::engine::general_purpose::STANDARD.encode([7; 48]);
    store
        .write_trusted(
            &who,
            &ployz_store::Edit {
                environment,
                expect: None,
                changes: vec![ployz_store::Change::Set {
                    path: ployz_store::SettingPath::parse(&format!("web.env.{name}")).unwrap(),
                    value: json!({ "secret": {
                        "sealedFor": web.environment.id,
                        "value": {
                            "sealed": sealed,
                            "scope": { "service": web.service.service.id },
                            "key": name,
                        },
                    } }),
                }],
            },
            &github(),
        )
        .unwrap();
}

/// Only the in-process Store, which [`opened_pull_request`] reaches directly.
#[test]
fn a_sync_from_a_pr_environment_stages_in_production_now() {
    let store = opened_pull_request(vec![ployz_store::GithubUserId::parse(9001).unwrap()]);
    ok(&store, &["set", "--env", "pr-5", "web.env.MODE=fast"]);
    sealed_in_local(&store, "pr-5", "TOKEN");
    // From a PR Environment, --to with no value is its Parent, and the Sync stages now.
    let plan = ok(&store, &["env", "sync", "--to", "--plan", "--env", "pr-5"]);
    assert_eq!(plan["into"]["name"], json!("production"));
    assert_eq!(labels(&plan["rows"]), ["web.env.MODE", "web.env.TOKEN"]);
    let synced = ok(&store, &["env", "sync", "--to", "--env", "pr-5"]);
    assert_eq!(
        (&synced["staged"], &synced["closing"]),
        (&json!(["web"]), &json!(false))
    );
    assert_eq!(
        ok(&store, &["get", "web.env.MODE", "--env", "production"])["settings"][0]["value"],
        json!("fast")
    );

    let sync = synced["sync"].as_str().unwrap();
    let undone = ok(&store, &["env", "sync", "--undo", sync]);
    assert_eq!(undone["into"]["name"], json!("production"));
    let plan = ok(&store, &["env", "sync", "--to", "--plan", "--env", "pr-5"]);
    assert_eq!(labels(&plan["rows"]), ["web.env.MODE", "web.env.TOKEN"]);
}

/// Only the in-process Store, which [`opened_pull_request`] reaches directly.
#[test]
fn an_outsiders_pull_request_deploys_once_a_member_approves_its_head() {
    let store = opened_pull_request(Vec::new());
    let waiting = ok(&store, &["env", "pr"]);
    assert_eq!(
        waiting["plans"][0]["open"][0],
        json!({
            "number": 5, "title": "Add search", "author": "ada", "head": "1".repeat(40),
            "standing": "needs_approval", "environment": null,
        })
    );
    let names = |listed: &Value| -> Vec<Value> {
        listed["environments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|env| env["name"].clone())
            .collect()
    };
    assert_eq!(names(&ok(&store, &["env", "ls"])), [json!("production")]);
    assert_eq!(
        error(&store, &["env", "pr", "approve", "6"])["code"],
        json!("not_found")
    );

    let approved = ok(&store, &["env", "pr", "approve", "5"]);
    assert_eq!(
        (&approved["standing"], &approved["environment"]),
        (&json!({ "approved": "1".repeat(40) }), &json!("pr-5"))
    );
    assert_eq!(
        names(&ok(&store, &["env", "ls"])),
        [json!("pr-5"), json!("production")]
    );
}

#[test]
fn a_config_lists_as_new_until_deployed_and_its_change_names_the_services_it_restarts() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let file = tempfile::NamedTempFile::new().unwrap();
    let put = |content: &str| {
        std::fs::write(file.path(), content).unwrap();
        let from = file.path().to_str().unwrap();
        ok(
            &store,
            &["config", "put", "sentry", "config.yml", "--from", from],
        );
    };
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "web:1"]);
    ok(&store, &["service", "add", "worker", "--image", "worker:1"]);
    ok(
        &store,
        &[
            "config",
            "add",
            "sentry",
            "--mount",
            "web:/etc/sentry",
            "--mount",
            "worker:/etc/sentry",
        ],
    );
    put("url: one\n");
    let row = |listing: &str| {
        listing
            .lines()
            .find(|line| line.starts_with("sentry"))
            .map(|line| line.split_whitespace().last().unwrap().to_owned())
    };
    assert_eq!(row(&human(&["config", "ls"])).as_deref(), Some("new"));
    assert!(
        human(&["config", "inspect", "sentry"])
            .lines()
            .any(|line| line.starts_with("next deploy") && line.ends_with(" new"))
    );

    applied(&store, "production");
    assert_eq!(row(&human(&["config", "ls"])).as_deref(), Some("-"));

    put("url: two\n");
    assert_eq!(
        ok(&store, &["diff"])["changes"][0]["restarts"],
        json!(["web", "worker"])
    );
    assert!(human(&["diff"]).contains("\n  restarts web, worker\n"));
    assert!(human(&["deploy", "--plan"]).contains("\n  restarts web, worker\n"));
    assert!(human(&["deploy", "web", "--plan"]).contains("\n  restarts web\n"));
}

#[test]
fn a_size_the_store_refuses_never_says_it_was_staged() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "web:1"]);
    ok(
        store,
        &["volume", "add", "data", "--docker", "--mount", "web:/data"],
    );
    applied(store, "production");
    let home = tempfile::tempdir().unwrap();
    let (code, stderr) = person(
        store,
        home.path(),
        &["volume", "set", "data", "--size", "10", "--wait"],
    );
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("keeps its storage"), "{stderr}");
    assert!(!stderr.contains("Nothing to wait for"), "{stderr}");
}

#[test]
fn a_verb_that_changes_nothing_says_so_and_keeps_the_revision() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let revision = || ok(store, &["config", "ls"])["environment"]["revision"].clone();
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "web:1"]);
    ok(store, &["volume", "add", "data", "--docker"]);
    ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "port: 80\n").unwrap();
    let from = file.path().to_str().unwrap();
    ok(store, &["config", "put", "app", "app.yml", "--from", from]);
    applied(store, "production");

    let unchanged = [
        (
            vec!["volume", "rename", "data", "data"],
            "Nothing changed in shop/production; Volume data already has that name.\n",
        ),
        (
            vec!["volume", "set", "data", "--docker"],
            "Nothing changed in shop/production; Volume data already has that storage.\n",
        ),
        (
            vec!["config", "put", "app", "app.yml", "--from", from],
            "Nothing changed in shop/production; Config app already holds app.yml as given.\n",
        ),
        (
            vec!["config", "rename", "app", "app"],
            "Nothing changed in shop/production; Config app already has that name.\n",
        ),
        (
            vec!["config", "mount", "web", "app", "/etc/app"],
            "Nothing changed in shop/production; web already mounts Config app there.\n",
        ),
    ];
    for (args, said) in &unchanged {
        let before = revision();
        let result = ok(store, args);
        assert_eq!(result["staged"], json!([]), "{args:?}");
        assert_eq!(result.get("next"), None, "{args:?}");
        assert_eq!(result["environment"]["revision"], before, "{args:?}");
        assert_eq!(&human(args), said, "{args:?}");
        assert_eq!(revision(), before, "{args:?}");
    }

    let unmounted = ok(store, &["config", "unmount", "web", "app"]);
    assert_eq!(unmounted["staged"], json!(["web.configs.app"]));
    let before = revision();
    assert_eq!(
        human(&["config", "unmount", "web", "app"]),
        "Nothing changed in shop/production; web does not mount Config app.\n"
    );
    assert_eq!(revision(), before);

    let listing = human(&["config", "ls"]);
    let row = listing
        .lines()
        .find(|line| line.starts_with("app"))
        .unwrap();
    assert!(row.ends_with("unmount from web"), "{listing}");
    assert_eq!(
        ok(store, &["config", "ls"])["configs"][0]["mount_changes"],
        json!([{ "service": "web", "before": "/etc/app", "after": null }])
    );

    assert_eq!(
        human(&["project", "rename", "shop", "shop"]),
        "Nothing changed; Project shop already has that name.\n"
    );
    let renamed = ok(store, &["project", "rename", "shop", "shop"]);
    assert_eq!(renamed["project"]["name"], json!("shop"));
    assert_eq!(renamed["links"], json!(0));
}

#[test]
fn history_scope_flags_work_before_or_after_the_action() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["env", "new", "staging"]);
        ok(
            store,
            &[
                "service", "add", "web", "--image", "web:1", "--env", "staging",
            ],
        );
        ok(store, &["deploy", "--skip-deploy", "--env", "staging"]);
        for args in [
            ["history", "--env", "staging", "restore", "1", "--preview"],
            ["history", "restore", "1", "--preview", "--env", "staging"],
        ] {
            let preview = ok(store, &args);
            assert_eq!(preview["environment"]["name"], json!("staging"), "{args:?}");
        }
    }
}

#[test]
fn history_says_nothing_staged_when_the_next_deploy_already_matches() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "web:1"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    ok(&store, &["set", "web.image=web:2"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    assert_eq!(
        human(&["history", "restore", "1"]),
        "Staged a rollback to #1. Deploy to apply.\n"
    );
    assert_eq!(
        human(&["history", "restore", "1"]),
        "Already matches #1. Nothing staged.\n"
    );
    assert_eq!(
        ok(&store, &["history", "restore", "1"])["changed"],
        json!(false)
    );
    assert_eq!(
        human(&["history", "undo", "2"]),
        "Nothing to undo in #2. Nothing staged.\n"
    );

    ok(&store, &["deploy", "--skip-deploy"]);
    assert_eq!(
        human(&["history", "undo", "3"]),
        "Staged an undo of #3. Deploy to apply.\n"
    );
    assert_eq!(
        human(&["history", "undo", "3"]),
        "Nothing to undo in #3. Nothing staged.\n"
    );
}

#[test]
fn history_lists_when_who_and_what_and_names_the_draft_an_action_overwrites() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "web:1"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    ok(&store, &["set", "web.env.SOAK=saved"]);
    ok(&store, &["deploy", "--skip-deploy", "--message", "soak it"]);
    let listed = human(&["history"]);
    let newest = listed.lines().nth(1).unwrap_or_default();
    assert!(newest.starts_with("#2"), "{listed}");
    assert!(newest.contains("web.env.SOAK"), "{listed}");
    assert!(newest.contains("soak it"), "{listed}");
    assert!(newest.contains("s ago"), "{listed}");
    assert!(
        listed
            .lines()
            .nth(2)
            .unwrap_or_default()
            .contains("first version"),
        "{listed}"
    );

    ok(&store, &["set", "web.env.SOAK=draft"]);
    let refused = error(&store, &["history", "undo", "2"]);
    assert_eq!(refused["code"], json!("confirmation_required"), "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .ends_with("draft changes to web.env.SOAK"),
        "{refused}"
    );
    assert_eq!(
        refused["details"]["next"],
        json!("ployz history undo 2 --accept-overwrite"),
        "{refused}"
    );
    ok(&store, &["history", "undo", "2", "--accept-overwrite"]);
}

#[test]
fn history_names_a_config_and_its_mount_by_name() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let human = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = store.command(home.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let file = tempfile::NamedTempFile::new().unwrap();
    let put = |content: &str| {
        std::fs::write(file.path(), content).unwrap();
        let from = file.path().to_str().unwrap();
        ok(&store, &["config", "put", "app", "app.yml", "--from", from]);
    };
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "web:1"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    ok(&store, &["config", "add", "app"]);
    put("a: 1");
    ok(&store, &["deploy", "--skip-deploy"]);
    ok(&store, &["config", "mount", "web", "app", "/etc/app"]);
    put("a: 2");
    ok(&store, &["deploy", "--skip-deploy"]);

    let listed = human(&["history"]);
    let newest = listed.lines().nth(1).unwrap_or_default();
    assert!(newest.contains("web.configs.app"), "{listed}");
    assert!(newest.contains("configs.app.files.app.yml"), "{listed}");
    assert!(!listed.contains("configs.@"), "{listed}");

    let preview = human(&["history", "undo", "3", "--preview"]);
    assert!(preview.contains("  web.configs.app: "), "{preview}");
    assert!(
        preview.contains("  configs.app.files.app.yml: "),
        "{preview}"
    );
    assert!(!preview.contains("configs.@"), "{preview}");
}

/// A Local Store with Service `web` and Config `app` saved as #1 and #2 and applied,
/// `app` mounted at `mount`.
fn web_and_app(mount: Option<&str>) -> Target {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "web", "--image", "web:1"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    applied(&store, "production");
    match mount {
        Some(mount) => ok(&store, &["config", "add", "app", "--mount", mount]),
        None => ok(&store, &["config", "add", "app"]),
    };
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "a: 1").unwrap();
    let from = file.path().to_str().unwrap();
    ok(&store, &["config", "put", "app", "app.yml", "--from", from]);
    ok(&store, &["deploy", "--skip-deploy"]);
    applied(&store, "production");
    store
}

/// What `ployz args` prints to people.
fn printed(store: &Target, args: &[&str]) -> String {
    let home = tempfile::tempdir().unwrap();
    let output = store.command(home.path()).args(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn a_bare_discard_names_a_dropped_config_mount_by_its_config() {
    let store = web_and_app(None);
    ok(&store, &["config", "mount", "web", "app", "/etc/app"]);
    let discarded = ok(&store, &["discard"]);
    assert!(
        discarded["dropped"][0]
            .as_str()
            .unwrap()
            .starts_with("web.configs.@"),
        "{discarded}"
    );

    ok(&store, &["config", "mount", "web", "app", "/etc/app"]);
    let said = printed(&store, &["discard"]);
    assert!(said.contains("web.configs.app"), "{said}");
    assert!(!said.contains("configs.@"), "{said}");
}

#[test]
fn an_undo_names_the_draft_config_mount_it_overwrites() {
    let store = web_and_app(Some("web:/etc/app"));
    ok(&store, &["config", "mount", "web", "app", "/etc/other"]);

    let preview = printed(&store, &["history", "undo", "2", "--preview"]);
    assert!(
        preview.contains("  overwrites draft web.configs.app\n"),
        "{preview}"
    );
    assert!(!preview.contains("configs.@"), "{preview}");

    let refused = error(&store, &["history", "undo", "2"]);
    assert_eq!(refused["code"], json!("confirmation_required"), "{refused}");
    let message = refused["message"].as_str().unwrap();
    assert!(
        message.ends_with("draft changes to web.configs.app"),
        "{refused}"
    );
}

#[test]
fn an_undo_names_an_overwritten_mount_under_its_renamed_service() {
    let store = web_and_app(Some("web:/etc/app"));
    ok(&store, &["service", "rename", "web", "api"]);
    ok(&store, &["config", "mount", "api", "app", "/etc/v2"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    ok(&store, &["config", "mount", "api", "app", "/etc/v3"]);

    let preview = printed(&store, &["history", "undo", "3", "--preview"]);
    assert!(
        preview.contains("  overwrites draft api.configs.app\n"),
        "{preview}"
    );
    assert!(!preview.contains("configs.@"), "{preview}");
}

#[test]
fn discarding_a_mount_by_id_names_its_config() {
    let store = web_and_app(None);
    ok(&store, &["config", "mount", "web", "app", "/etc/app"]);
    let diff = ok(&store, &["diff"]);
    let path = diff["changes"][0]["settings"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(path.starts_with("web.configs.@"), "{diff}");

    let said = printed(&store, &["discard", &path]);
    assert!(said.contains("web.configs.app"), "{said}");
    assert!(!said.contains("configs.@"), "{said}");
}

#[test]
fn a_discard_to_saved_reads_a_lone_mount_without_an_id() {
    let store = web_and_app(Some("web:/etc/app"));
    ok(&store, &["config", "rm", "app"]);
    ok(&store, &["deploy", "--skip-deploy"]);
    ok(
        &store,
        &["config", "add", "app", "--mount", "web:/etc/app2"],
    );

    let said = printed(&store, &["discard", "--target", "saved"]);
    assert!(said.contains("web.configs.app"), "{said}");
    assert!(!said.contains("(@"), "{said}");
}

#[test]
fn a_second_discard_of_a_mount_by_id_still_names_its_config() {
    let store = web_and_app(Some("web:/etc/app"));
    ok(&store, &["config", "mount", "web", "app", "/etc/v2"]);
    let diff = ok(&store, &["diff"]);
    let path = diff["changes"][0]["settings"][0]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(path.starts_with("web.configs.@"), "{diff}");
    ok(&store, &["discard", &path]);

    let said = printed(&store, &["discard", &path]);
    assert!(
        said.contains("Nothing staged at web.configs.app "),
        "{said}"
    );
    assert!(!said.contains("configs.@"), "{said}");
}

#[test]
fn an_undo_tells_apart_the_files_of_two_configs_of_one_name() {
    let store = web_and_app(None);
    ok(&store, &["config", "rm", "app"]);
    ok(&store, &["config", "add", "app"]);
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "b: 2").unwrap();
    let from = file.path().to_str().unwrap();
    ok(&store, &["config", "put", "app", "app.yml", "--from", from]);
    ok(&store, &["deploy", "--skip-deploy"]);

    let preview = printed(&store, &["history", "undo", "3", "--preview"]);
    let files: Vec<&str> = preview
        .lines()
        .filter(|line| line.contains(".files.app.yml"))
        .collect();
    assert_eq!(files.len(), 2, "{preview}");
    assert!(
        files
            .iter()
            .all(|line| line.contains("configs.app.files.app.yml (@")),
        "{preview}"
    );
    assert_ne!(
        files[0].split(':').next(),
        files[1].split(':').next(),
        "{preview}"
    );
}

#[test]
fn a_replaced_config_of_the_same_name_reads_apart_in_diff() {
    let store = web_and_app(Some("web:/etc/app"));
    ok(&store, &["config", "rm", "app"]);
    ok(
        &store,
        &["config", "add", "app", "--mount", "web:/etc/app2"],
    );

    let diff = printed(&store, &["diff"]);
    let mounts: Vec<&str> = diff
        .lines()
        .filter(|line| line.contains("web.configs.app"))
        .collect();
    assert_eq!(mounts.len(), 2, "{diff}");
    assert!(
        mounts
            .iter()
            .all(|line| line.contains("web.configs.app (@")),
        "{diff}"
    );
    assert_ne!(
        mounts[0].split(':').next(),
        mounts[1].split(':').next(),
        "{diff}"
    );
}

#[test]
fn a_bare_discard_names_every_change_it_drops() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    ok(store, &["project", "new", "shop"]);
    ok(store, &["service", "add", "web", "--image", "web:1"]);
    applied(store, "production");
    ok(store, &["set", "web.env.MINE=1"]);
    ok(store, &["service", "add", "api", "--image", "api:1"]);
    let discarded = ok(store, &["discard"]);
    let mut dropped: Vec<&str> = discarded["dropped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect();
    dropped.sort_unstable();
    assert_eq!(dropped, ["api", "web.env.MINE"], "{discarded}");
}

/// `ployz ARGS` fed `input` on stdin: its exit code, stdout and stderr.
fn ployz_fed(target: &Target, args: &[&str], input: &str) -> (Option<i32>, String, String) {
    let home = tempfile::tempdir().unwrap();
    let mut child = target
        .command(home.path())
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.as_bytes()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn a_sync_a_pr_plan_and_a_build_order_retry_with_their_arguments_and_the_approval() {
    let (target, approvals) = approving(&[("apr_1", "pending")]);
    ok(&target, &["env", "branch", "fix-web", "--copy", "web"]);
    for (args, retry) in [
        (
            &[
                "env",
                "sync",
                "--to",
                "--env",
                "fix-web",
                "--only",
                "web",
                "--skip",
                "web.env.DEBUG",
                "--close",
            ][..],
            "ployz env sync --to --only web --skip web.env.DEBUG --close --approval apr_1 --env fix-web",
        ),
        (
            &["env", "sync", "--from", "fix-web", "--version", REVIEWED],
            "ployz env sync --from fix-web --version 3:1:0.1 --approval apr_1",
        ),
        (
            &[
                "env",
                "sync",
                "--take",
                "production",
                "--only",
                "web",
                "--env",
                "fix-web",
            ],
            "ployz env sync --take production --only web --approval apr_1 --env fix-web",
        ),
        (
            &[
                "env",
                "pr",
                "acme/web",
                "--on",
                "--from",
                "production",
                "--copy",
                "web",
                "--setup",
                "web=make seed",
                "--remove-on-close",
                "false",
                "--bots",
                "true",
                "--project",
                "shop",
            ],
            "ployz env pr acme/web --on --from production --copy web --setup 'web=make seed' \
             --remove-on-close false --bots true --approval apr_1 --project shop",
        ),
        (
            &["env", "pr", "acme/web", "--off"],
            "ployz env pr acme/web --off --approval apr_1",
        ),
        (
            &["org", "build-order", "servers-only"],
            "ployz org build-order servers-only --approval apr_1",
        ),
    ] {
        let refused = error(&target, args);
        assert_eq!(refused["code"], json!("approval_required"), "{refused}");
        assert_eq!(refused["details"]["retry"], json!(retry), "{args:?}");
        // The retry parses and asks again while the approval is pending.
        let words = shell_words::split(retry).unwrap();
        let words: Vec<&str> = words[1..].iter().map(String::as_str).collect();
        let again = error(&target, &words);
        assert_eq!(again["code"], json!("approval_required"), "{again}");
        assert_eq!(again["details"]["retry"], json!(retry), "{words:?}");
        // The retry sends the same request, but for a Sync's receipt, new each run.
        let gated = approvals.gated.lock().unwrap();
        let [.., first, retried] = gated.as_slice() else {
            panic!("{args:?} didn't reach Cloud twice")
        };
        let receipt = |command: &Value| {
            let mut command = command.clone();
            command.as_object_mut().unwrap().remove("id");
            command
        };
        assert_eq!(receipt(retried), receipt(first), "{args:?}");
    }
    assert!(
        approvals.calls().is_empty(),
        "--json never waits or decides"
    );
}

#[test]
fn a_sync_that_gives_a_secret_names_its_row_in_the_retry_and_never_the_value() {
    const SECRET: &str = "s3cret-sync-value";
    let (target, _) = approving(&[("apr_1", "approved")]);
    ok(&target, &["env", "branch", "fix-web", "--copy", "web"]);
    let secret = ["set", "web.env.API_KEY", "--secret", "--env", "fix-web"];
    let (code, set) = ployz_with_stdin(&target, &secret, "branch-key\n");
    assert_eq!(code, Some(0), "{set}");
    let sync = [
        "env",
        "sync",
        "--to",
        "--env",
        "fix-web",
        "--value",
        "web.env.API_KEY",
    ];
    let fed = format!("{SECRET}\n");

    let (code, stdout, stderr) = ployz_fed(&target, &[&["--json"][..], &sync].concat(), &fed);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        !stdout.contains(SECRET) && !stderr.contains(SECRET),
        "{stdout}{stderr}"
    );
    let refused: Value = serde_json::from_str(&stdout).unwrap();
    let retry = refused["error"]["details"]["retry"].as_str().unwrap();
    assert_eq!(
        retry,
        "ployz env sync --to --value web.env.API_KEY --env fix-web"
    );
    let message = refused["error"]["message"].as_str().unwrap();
    assert!(
        message.ends_with(
            ". --approval can't go with --value, so the retry reads each --value from stdin \
             again and asks for a new approval. To finish in one run, run it without --json: \
             it waits for the approval."
        ),
        "{message}"
    );

    // Even approved, an approval never rides along with secrets.
    approve_in_cloud(&target, "apr_1");
    let approved = [&sync[..], &["--approval", "apr_1"]].concat();
    let (code, stdout, stderr) = ployz_fed(&target, &[&["--json"][..], &approved].concat(), &fed);
    assert_eq!(code, Some(2), "{stdout}{stderr}");
    assert!(
        !stdout.contains(SECRET) && !stderr.contains(SECRET),
        "{stdout}{stderr}"
    );
    let refused: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        refused["error"]["code"],
        json!("invalid_argument"),
        "{refused}"
    );
    assert_eq!(
        refused["error"]["message"],
        json!(
            "Expected --approval without --value: an approval can't carry secrets given again \
             on stdin. Run the sync without --approval and without --json: it waits for a new \
             approval."
        )
    );

    // The retry, its value on stdin again, asks again and names itself again.
    let words = shell_words::split(retry).unwrap();
    let words: Vec<&str> = words[1..].iter().map(String::as_str).collect();
    let (code, stdout, stderr) = ployz_fed(&target, &[&["--json"][..], &words].concat(), &fed);
    assert_eq!(code, Some(1), "{stdout}{stderr}");
    assert!(
        !stdout.contains(SECRET) && !stderr.contains(SECRET),
        "{stdout}{stderr}"
    );
    let again: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        again["error"]["code"],
        json!("approval_required"),
        "{again}"
    );
    assert_eq!(again["error"]["details"]["retry"], json!(retry));

    // A person without a terminal waits for the approval, and sees no secret either.
    let (target, approvals) = approving(&[("apr_1", "approved")]);
    ok(&target, &["env", "branch", "fix-web", "--copy", "web"]);
    let (code, set) = ployz_with_stdin(&target, &secret, "branch-key\n");
    assert_eq!(code, Some(0), "{set}");
    let (code, stdout, stderr) = ployz_fed(&target, &sync, &fed);
    assert_eq!(code, Some(0), "{stdout}{stderr}");
    assert!(
        stderr.contains("Waiting for approval in Ployz Cloud"),
        "{stderr}"
    );
    assert!(
        !stdout.contains(SECRET) && !stderr.contains(SECRET),
        "{stdout}{stderr}"
    );
    assert!(approvals.calls().iter().all(|call| call.0 == "GET"));
}

#[test]
fn an_agent_gets_the_approval_a_save_needs_and_retries_with_it() {
    let (target, approvals) = approving(&[("apr_1", "approved")]);
    let refused = error(&target, &["deploy", "--skip-deploy"]);
    assert_eq!(refused["code"], json!("approval_required"), "{refused}");
    assert_eq!(refused["details"]["approval_id"], json!("apr_1"));
    assert_eq!(
        refused["details"]["retry"],
        json!("ployz deploy --skip-deploy --approval apr_1"),
        "{refused}"
    );
    let pending = error(&target, &["deploy", "--skip-deploy", "--approval", "apr_1"]);
    assert_eq!(pending["code"], json!("approval_required"), "{pending}");
    assert!(
        approvals.calls().is_empty(),
        "--json never waits or decides"
    );
}

#[test]
fn without_a_terminal_a_save_waits_until_a_human_approves_it() {
    let (target, approvals) = approving(&[("apr_1", "approved")]);
    let home = tempfile::tempdir().unwrap();
    let (code, stderr) = person(&target, home.path(), &["deploy", "--skip-deploy"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "The next deploy after this save destroys 1 thing:\n  \u{2715} removes Service worker\n  + 2 other changes"
        ),
        "{stderr}"
    );
    // A token's approval names no Organization, so the CLI can't link the agent's panel.
    assert!(
        stderr.contains("Approve apr_1 in Ployz Cloud at http://"),
        "{stderr}"
    );
    assert!(!stderr.contains("Waiting on you"), "{stderr}");
    assert!(
        stderr.contains("Waiting for approval in Ployz Cloud… approved.\n"),
        "{stderr}"
    );
    let polls: Vec<_> = approvals.calls().into_iter().map(|call| call.0).collect();
    assert_eq!(polls, ["GET", "GET"], "pending, then approved");
}

#[test]
fn without_a_terminal_a_denied_save_fails_with_the_reason() {
    let (target, approvals) = approving(&[("apr_1", "denied")]);
    let home = tempfile::tempdir().unwrap();
    let (code, stderr) = person(&target, home.path(), &["deploy", "--skip-deploy"]);
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("A human denied the request to save production: the worker still drains"),
        "{stderr}"
    );
    // The wait names the approval to answer; a denied one has no retry to name it in.
    let denial = &stderr[stderr.find("error:").unwrap_or_default()..];
    assert!(!denial.contains("apr_1"), "{stderr}");
    assert!(approvals.calls().iter().all(|call| call.0 == "GET"));
    let diff = ok(&target, &["diff"]);
    assert_eq!(diff["holds_staged"], json!(false), "nothing was saved");
}

#[cfg(unix)]
#[test]
fn without_a_terminal_ctrl_c_stops_the_wait_and_saves_nothing() {
    let (target, approvals) = approving(&[("apr_1", "pending")]);
    let home = tempfile::tempdir().unwrap();
    let mut child = target
        .command(home.path())
        .args(["deploy", "--skip-deploy"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut seen = String::new();
    let mut chunk = [0; 256];
    while !seen.contains("Waiting for approval in Ployz Cloud") {
        let read = stderr.read(&mut chunk).unwrap();
        assert_ne!(read, 0, "{seen}");
        seen.push_str(&String::from_utf8_lossy(&chunk[..read]));
    }
    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .unwrap()
            .success()
    );
    stderr.read_to_string(&mut seen).unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(130), "{seen}");
    assert!(
        seen.contains("Stopped waiting; the approval stays pending in Ployz Cloud."),
        "{seen}"
    );
    assert!(approvals.calls().iter().all(|call| call.0 == "GET"));
    let diff = ok(&target, &["diff"]);
    assert_eq!(diff["holds_staged"], json!(false), "nothing was saved");
}

#[test]
fn without_a_terminal_a_superseded_approval_waits_on_the_new_one() {
    let (target, approvals) = approving(&[("apr_1", "superseded"), ("apr_2", "approved")]);
    let home = tempfile::tempdir().unwrap();
    let (code, stderr) = person(&target, home.path(), &["deploy", "--skip-deploy"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(
        stderr
            .matches("The next deploy after this save destroys 1 thing:")
            .count(),
        2,
        "{stderr}"
    );
    let polled: Vec<_> = approvals.calls().into_iter().map(|call| call.1).collect();
    assert_eq!(polled, ["apr_1", "apr_1", "apr_2", "apr_2"]);
}

#[test]
fn an_agent_following_each_retry_deploys_what_deletes_a_volume() {
    let (target, approvals) = serving(Approvals {
        deletes_volume: Some("data"),
        ..Approvals::of(&[("apr_1", "pending")])
    });
    let mut args = vec!["deploy".to_owned(), "--detach".to_owned()];
    let mut refused = Vec::new();
    let deployed = loop {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        let (code, json) = ployz(Some(&target), &words);
        if code == Some(0) {
            break json;
        }
        let error = &json["error"];
        refused.push(error["code"].as_str().unwrap().to_owned());
        assert!(refused.len() <= 2, "{refused:?}, then {json}");
        if let Some(id) = error["details"]["approval_id"].as_str() {
            approve_in_cloud(&target, id);
        }
        let retry = shell_words::split(error["details"]["retry"].as_str().unwrap()).unwrap();
        args = retry[1..].to_vec();
    };
    assert_eq!(refused, ["confirmation_required", "approval_required"]);
    assert_eq!(
        args,
        [
            "deploy",
            "--detach",
            "--accept-volume-loss",
            "data",
            "--expect-version",
            REVIEWED,
            "--approval",
            "apr_1"
        ]
    );
    assert_eq!(deployed["number"], json!(1), "admitted: {deployed}");
    let decided: Vec<_> = approvals.calls().into_iter().map(|call| call.0).collect();
    assert_eq!(decided, ["POST"], "only the human decided");
}

/// Undo what makes ployz skip its prompts: CI, or a terminal that can't show them.
#[cfg(target_os = "linux")]
fn as_a_person(command: &mut Command) {
    command.env("TERM", "xterm").env_remove("CI");
}

#[cfg(target_os = "linux")]
fn at_a_terminal(target: &Target, args: &str, typed: &str) -> (Option<i32>, String) {
    let home = tempfile::tempdir().unwrap();
    let ployz = format!("{} {args}", env!("CARGO_BIN_EXE_ployz"));
    let mut script = Command::new("script");
    for (key, value) in target.command(home.path()).get_envs() {
        match value {
            Some(value) => script.env(key, value),
            None => script.env_remove(key),
        };
    }
    as_a_person(&mut script);
    let mut child = script
        .args(["--quiet", "--return", "--command", &ployz, "/dev/null"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(typed.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let screen = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.code(), screen)
}

#[cfg(target_os = "linux")]
#[test]
fn at_a_terminal_a_wrong_name_leaves_the_approval_pending() {
    let (target, approvals) = approving(&[("apr_1", "approved")]);
    let (code, screen) = at_a_terminal(&target, "deploy --skip-deploy", "staging\n");
    assert_eq!(code, Some(130), "{screen}");
    assert!(screen.contains("Type production to continue"), "{screen}");
    assert!(
        screen.contains("Nothing saved; the approval stays pending in Ployz Cloud."),
        "{screen}"
    );
    assert!(approvals.calls().is_empty(), "nothing was decided in Cloud");
}

#[cfg(target_os = "linux")]
#[test]
fn at_a_terminal_the_environment_name_approves_and_saves() {
    let (target, approvals) = approving(&[("apr_1", "pending")]);
    let (code, screen) = at_a_terminal(&target, "deploy --skip-deploy", "production\n");
    assert_eq!(code, Some(0), "{screen}");
    assert!(screen.contains("Type production to continue"), "{screen}");
    let approve = json!({ "approve": { "digest": "3:abc" } });
    assert_eq!(
        approvals.calls(),
        [("POST".to_owned(), "apr_1".to_owned(), approve)]
    );
    assert_eq!(ok(&target, &["diff"])["holds_staged"], json!(true));
}

#[test]
fn signed_in_a_drain_runs_in_cloud() {
    let (target, approvals) = running(&[]);
    let drained = ok(&target, &["server", "drain", CLOUD_SERVER]);
    assert_eq!(drained["services_role"], json!("turned_off"), "{drained}");
    assert_eq!(drained["server"]["name"], json!("web-1"), "{drained}");
    assert_eq!(
        approvals.runs(),
        [
            format!("POST servers/{CLOUD_SERVER}/drain"),
            "GET server-drains/drn_1".to_owned(),
        ]
    );
    assert_eq!(approvals.bodies()[0], json!({ "server_name": "web-1" }));
}

#[test]
fn signed_in_an_upgrade_runs_in_cloud_along_the_release_channel() {
    let (target, approvals) = running(&[]);
    let upgraded = ok(&target, &["server", "upgrade", "stable", CLOUD_SERVER]);
    assert_eq!(
        upgraded["attempts"][0]["outcome"],
        json!("succeeded"),
        "{upgraded}"
    );
    assert_eq!(upgraded["attempts"][0]["target_version"], json!("0.3.0"));
    assert_eq!(upgraded["unattempted"], json!([]));
    assert_eq!(
        approvals.runs(),
        [
            format!("POST servers/{CLOUD_SERVER}/upgrade"),
            "GET server-upgrades/upg_1".to_owned(),
        ]
    );
    assert_eq!(
        approvals.bodies()[0],
        json!({ "channel": "stable", "server_name": "web-1" })
    );

    let mismatch = error(&target, &["server", "upgrade", "beta", CLOUD_SERVER]);
    assert_eq!(mismatch["code"], json!("channel_mismatch"), "{mismatch}");
    assert_eq!(mismatch["details"]["channel"], json!("stable"));
    assert_eq!(
        mismatch["details"]["retry"],
        json!(format!("ployz server upgrade stable {CLOUD_SERVER}")),
        "{mismatch}"
    );

    let exact = error(&target, &["server", "upgrade", "0.3.1", CLOUD_SERVER]);
    assert_eq!(exact["code"], json!("invalid_argument"), "{exact}");
    assert_eq!(
        approvals.runs().len(),
        3,
        "an exact version never reaches Cloud"
    );
}

#[test]
fn a_cloud_upgrade_skips_an_offline_or_busy_server_by_name_and_upgrades_the_rest() {
    let (target, approvals) = running(&[]);
    let (code, upgraded) = ployz(
        Some(&target),
        &[
            "server",
            "upgrade",
            "stable",
            OFFLINE_SERVER,
            CLOUD_SERVER,
            BUSY_SERVER,
        ],
    );
    assert_eq!(
        code,
        Some(3),
        "a partial Upgrade exits as a partial result: {upgraded}"
    );
    assert_eq!(
        upgraded["attempts"][0]["server"],
        json!(CLOUD_SERVER),
        "{upgraded}"
    );
    assert_eq!(upgraded["attempts"][0]["outcome"], json!("succeeded"));
    assert_eq!(
        upgraded["skipped"],
        json!([
            { "server": OFFLINE_SERVER, "reason": "This Server isn't online and idle, so it can't take an Upgrade now." },
            { "server": BUSY_SERVER, "reason": "The Server was busy with a build or another Upgrade, or went offline, so it took no Upgrade." },
        ])
    );
    let skipped = &upgraded["follow_up_error"];
    assert_eq!(
        skipped["message"],
        json!("Skipped Servers web-2, web-3; they took no Upgrade."),
        "{skipped}"
    );
    assert_eq!(
        skipped["details"]["retry"],
        json!(format!(
            "ployz server upgrade stable {OFFLINE_SERVER} {BUSY_SERVER}"
        ))
    );
    assert_eq!(
        approvals.runs(),
        [
            format!("POST servers/{OFFLINE_SERVER}/upgrade"),
            format!("POST servers/{CLOUD_SERVER}/upgrade"),
            "GET server-upgrades/upg_1".to_owned(),
            format!("POST servers/{BUSY_SERVER}/upgrade"),
            "GET server-upgrades/upg_busy".to_owned(),
        ],
        "a busy Server's run is read once, not polled"
    );

    let refused = error(&target, &["server", "upgrade", "stable", OLD_CLOUD_SERVER]);
    assert_eq!(
        refused["message"],
        json!("Your servers aren't answering. Try again once they are."),
        "Cloud's own failure reads as its sentence, not its JSON: {refused}"
    );
}

/// stdout then stderr of a person's `ployz ARGS`, without `--json`.
fn said(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn a_cloud_upgrade_chosen_by_id_names_the_server_not_its_id() {
    let (target, _) = running(&[]);
    let home = tempfile::tempdir().unwrap();
    let output = target
        .command(home.path())
        .args(["server", "upgrade", "stable", CLOUD_SERVER])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let said = said(&output);
    assert_eq!(output.status.code(), Some(0), "{said}");
    assert!(said.contains("Upgrading Server web-1."), "{said}");
    assert!(said.contains("Upgraded Server web-1 to 0.3.0."), "{said}");
    assert!(!said.contains(CLOUD_SERVER), "ids are machinery: {said}");
}

#[test]
fn stopping_a_cloud_drain_chosen_by_id_names_the_server_not_its_id() {
    let (target, approvals) = running(&[]);
    let home = tempfile::tempdir().unwrap();
    let child = target
        .command(home.path())
        .args(["server", "drain", BUSY_SERVER])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    approvals.wait_for_run("GET server-drains/drn_running");
    let pid = rustix::process::Pid::from_raw(child.id().try_into().unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
    let output = child.wait_with_output().unwrap();
    let said = said(&output);
    assert_eq!(output.status.code(), Some(130), "{said}");
    assert!(
        said.contains("Draining Server web-3 keeps running in Ployz Cloud"),
        "{said}"
    );
    assert!(!said.contains(BUSY_SERVER), "ids are machinery: {said}");
}

#[test]
fn an_agent_gets_the_approval_a_cleanup_needs() {
    let (target, approvals) = running(&[("apr_1", "approved")]);
    let refused = error(
        &target,
        &[
            "server",
            "clean",
            "--namespace",
            "stray",
            "--confirm",
            "stray",
        ],
    );
    assert_eq!(refused["code"], json!("approval_required"), "{refused}");
    assert_eq!(
        refused["details"]["operation"],
        json!({ "verb": "clean", "name": "stray" })
    );
    assert_eq!(
        refused["details"]["retry"],
        json!("ployz server clean --namespace stray --confirm stray --approval apr_1"),
        "{refused}"
    );
    assert_eq!(approvals.runs(), ["POST namespaces/stray/clean"]);
    assert!(approvals.calls().is_empty(), "--json never waits");
}

#[test]
fn without_a_terminal_a_cleanup_waits_for_approval_then_retries_with_it() {
    let (target, approvals) = running(&[("apr_1", "approved")]);
    let home = tempfile::tempdir().unwrap();
    let output = target
        .command(home.path())
        .args([
            "server",
            "clean",
            "--namespace",
            "stray",
            "--confirm",
            "stray",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Waiting for approval in Ployz Cloud"),
        "{stderr}"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        stdout, "Removed Namespace stray and the data of its Volume.\n",
        "names no Docker Volume"
    );
    let polls: Vec<_> = approvals.calls().into_iter().map(|call| call.0).collect();
    assert_eq!(polls, ["GET", "GET"], "pending, then approved");
    assert_eq!(
        approvals.runs(),
        [
            "POST namespaces/stray/clean",
            "POST namespaces/stray/clean approved by apr_1",
            "GET namespace-cleanups/cln_1",
        ]
    );
}

#[test]
fn signed_in_a_cleanup_takes_the_typed_namespace_even_when_nobody_must_approve() {
    let (target, approvals) = running(&[]);
    let refused = error(&target, &["server", "clean", "--namespace", "stray"]);
    assert_eq!(refused["code"], json!("confirmation_required"), "{refused}");
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .contains("Volume used by web at /data on web-1 and its data."),
        "the Volume reads as Cloud's approval names it: {refused}"
    );
    assert_eq!(
        refused["details"]["volumes"],
        json!([{ "machine_id": CLOUD_SERVER, "name": "stray_data", "label": "used by web at /data on web-1" }])
    );
    assert_eq!(approvals.runs(), ["GET namespaces/stray/clean"]);

    let cleaned = ok(
        &target,
        &[
            "server",
            "clean",
            "--namespace",
            "stray",
            "--confirm",
            "stray",
        ],
    );
    assert_eq!(cleaned["volumes"], json!(["data"]), "{cleaned}");
    assert_eq!(
        approvals.runs(),
        [
            "GET namespaces/stray/clean",
            "POST namespaces/stray/clean",
            "GET namespace-cleanups/cln_1"
        ]
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_following_a_cleanup_cloud_keeps_running() {
    let (target, approvals) = running(&[]);
    let home = tempfile::tempdir().unwrap();
    let mut child = target
        .command(home.path())
        .args([
            "server",
            "clean",
            "--namespace",
            "slow",
            "--confirm",
            "slow",
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    approvals.wait_for_run("GET namespace-cleanups/cln_slow");
    let pid = child.id().to_string();
    assert!(
        Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .unwrap()
            .success()
    );
    let mut child_stderr = child.stderr.take().unwrap();
    let (exited, exit) = std::sync::mpsc::channel();
    std::thread::spawn(move || exited.send(child.wait().unwrap()));
    let Ok(status) = exit.recv_timeout(std::time::Duration::from_secs(5)) else {
        Command::new("kill").args(["-KILL", &pid]).status().unwrap();
        panic!("still following 5s after Ctrl-C: {:?}", approvals.runs());
    };
    let mut stderr = String::new();
    child_stderr.read_to_string(&mut stderr).unwrap();
    assert_eq!(status.code(), Some(130), "{stderr}");
    assert!(
        stderr.contains(
            "Stopped following. Removing Namespace slow keeps running in Ployz Cloud and finishes on its own."
        ),
        "{stderr}"
    );
    assert!(
        approvals
            .runs()
            .iter()
            .all(|run| !run.starts_with("DELETE")),
        "nothing cancels the run: {:?}",
        approvals.runs()
    );
}

#[test]
fn an_agent_gets_the_approval_a_drain_needs_and_a_person_waits_for_it() {
    let (target, approvals) = running(&[("apr_1", "approved")]);
    let refused = error(&target, &["server", "drain", CLOUD_SERVER]);
    assert_eq!(refused["code"], json!("approval_required"), "{refused}");
    assert_eq!(
        refused["details"]["operation"],
        json!({ "verb": "drain", "name": CLOUD_SERVER })
    );
    assert_eq!(
        refused["details"]["retry"],
        json!(format!(
            "ployz server drain {CLOUD_SERVER} --approval apr_1"
        )),
        "{refused}"
    );
    assert!(approvals.calls().is_empty(), "--json never waits");

    let home = tempfile::tempdir().unwrap();
    let (code, stderr) = person(&target, home.path(), &["server", "drain", CLOUD_SERVER]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stderr.contains("Waiting for approval in Ployz Cloud"),
        "{stderr}"
    );
    assert_eq!(
        approvals.runs(),
        [
            format!("POST servers/{CLOUD_SERVER}/drain"),
            format!("POST servers/{CLOUD_SERVER}/drain"),
            format!("POST servers/{CLOUD_SERVER}/drain approved by apr_1"),
            "GET server-drains/drn_1".to_owned(),
        ]
    );
}

#[test]
fn a_denied_drain_or_cleanup_stops_with_the_reason_and_starts_nothing() {
    for (args, subject) in [
        (
            &["server", "drain", CLOUD_SERVER][..],
            format!("drain {CLOUD_SERVER}"),
        ),
        (
            &[
                "server",
                "clean",
                "--namespace",
                "stray",
                "--confirm",
                "stray",
            ],
            "clean stray".to_owned(),
        ),
    ] {
        let (target, approvals) = running(&[("apr_1", "denied")]);
        let home = tempfile::tempdir().unwrap();
        let (code, stderr) = person(&target, home.path(), args);
        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "A human denied the request to {subject}: declined"
            )),
            "{stderr}"
        );
        // The wait names the approval to answer; a denied one has no retry to name it in.
        let denial = &stderr[stderr.find("error:").unwrap_or_default()..];
        assert!(!denial.contains("apr_1"), "{stderr}");
        let followed = approvals.runs().into_iter().any(|run| {
            run.starts_with("GET server-drains") || run.starts_with("GET namespace-cleanups")
        });
        assert!(!followed, "{:?}", approvals.runs());

        let refused = error(&target, &[args, &["--approval", "apr_1"]].concat());
        assert_eq!(refused["code"], json!("approval_denied"), "{refused}");
    }
}

#[test]
fn with_a_context_a_signed_in_command_dials_directly() {
    let (target, approvals) = running(&[("apr_1", "approved")]);
    for args in [
        &["server", "drain", CLOUD_SERVER, "--context", "lab"][..],
        &[
            "server",
            "upgrade",
            "stable",
            CLOUD_SERVER,
            "--context",
            "lab",
        ],
        &[
            "server",
            "clean",
            "--namespace",
            "stray",
            "--context",
            "lab",
        ],
    ] {
        let (code, json) = ployz(Some(&target), args);
        assert_ne!(code, Some(0), "{args:?}: no context lab to dial: {json}");
    }
    assert!(approvals.runs().is_empty(), "{:?}", approvals.runs());
}

#[test]
fn a_removal_no_server_answers_is_still_removing_and_retries_with_the_same_rm() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        dispatch(store, written);
        if let Written::Removal(Removal::Removing { left, .. }) = written {
            for deployment in left.iter().filter_map(|left| left.deployment.clone()) {
                dispatch(store, &Written::Deployment(deployment));
            }
        }
    }
    let cloud = Target::Cloud {
        url: fake_cloud_with_dispatch(worker),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["env", "new", "staging"]);
    let web = ["service", "add", "web", "--image", "nginx:alpine"];
    ok(&cloud, &[&web[..], &["--env", "staging"]].concat());
    assert_eq!(
        ployz(Some(&cloud), &["deploy", "--env", "staging"]).0,
        Some(1)
    );

    let removing = |args: &[&str]| {
        let home = tempfile::tempdir().unwrap();
        let output = cloud.command(home.path()).args(args).output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(3), "{args:?}: {stderr}");
        let retry = stderr
            .lines()
            .find_map(|line| line.strip_prefix("retry: "))
            .unwrap_or_else(|| panic!("{args:?} names no retry: {stderr}"))
            .to_owned();
        (output.stdout, retry)
    };
    let (stdout, retry) = removing(&[
        "--json",
        "env",
        "rm",
        "staging",
        "--confirm",
        "shop/staging",
    ]);
    let removed: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(removed["removal"], "removing", "{removed}");
    assert_eq!(removed["environment"]["name"], "staging", "{removed}");
    assert_eq!(removed["deployments"][0]["status"], "failed", "{removed}");
    assert_eq!(
        removed["left"][0]["deployment"]["status"], "failed",
        "{removed}"
    );
    assert_eq!(retry, "ployz env rm staging --confirm shop/staging");

    // The retry it names runs the removal again rather than refusing.
    let rerun: Vec<&str> = retry.split(' ').skip(1).collect();
    let (stdout, retry) = removing(&rerun);
    let said = String::from_utf8(stdout).unwrap();
    assert!(
        said.contains("Deployment #3 of shop/staging: failed"),
        "{said}"
    );
    assert!(!said.contains("Cloud goes on"), "{said}");
    assert_eq!(retry, "ployz env rm staging --confirm shop/staging");

    let (stdout, retry) = removing(&["--json", "project", "rm", "shop", "--confirm", "shop"]);
    let removed: Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(removed["removal"], "removing", "{removed}");
    assert_eq!(removed["project"]["name"], "shop", "{removed}");
    assert_eq!(retry, "ployz project rm shop --confirm shop");
}

#[test]
fn a_removal_without_volumes_names_the_server_that_did_not_answer_cloud() {
    /// The deploy fails on `web-1`, so the Store records its name; Cloud's runner
    /// takes each removal Deployment to the Server, which never answers.
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        let Written::Deployment(admitted) = written else {
            if let Written::Removal(Removal::Removing { left, .. }) = written {
                for deployment in left.iter().filter_map(|left| left.deployment.clone()) {
                    dispatch(store, &Written::Deployment(deployment));
                }
            }
            return;
        };
        if admitted.remove {
            return dispatch(store, written);
        }
        let runner = RunnerId::parse("cloud-worker").unwrap();
        let claimed = store.claim(&admitted.id, &runner).unwrap();
        let operation = json!({
            "type": "remove_container", "machine_id": CLOUD_SERVER, "container_id": "b".repeat(64)
        });
        let row = |status: Value| {
            json!({
                "index": 0, "machine_id": CLOUD_SERVER, "machine_name": "web-1",
                "service_name": "web", "operation": operation, "status": status
            })
        };
        let preview = serde_json::from_value(json!({
            "namespace": claimed.intent.namespace,
            "operations": [row(json!({"type": "pending"}))],
            "warnings": [], "would_remove": [], "preserved_volumes": []
        }))
        .unwrap();
        store
            .record(
                &admitted.id,
                &runner,
                ployz_store::RunEvidence::Prepared(preview),
            )
            .unwrap();
        let error = json!({"type": "machine", "action": "RemoveContainer", "error": {
            "code": "internal", "message": "busy", "details": {}, "cause": []
        }});
        let rows: Vec<ployz_core::OperationRow> =
            serde_json::from_value(json!([row(json!({"type": "failed", "error": error}))]))
                .unwrap();
        let outcome = serde_json::from_value(json!({
            "type": "failed", "completed": [], "unexecuted": [],
            "failed": {"type": "operation", "operation": operation, "error": error}
        }))
        .unwrap();
        let executed = ployz_store::RunEvidence::Executed {
            outcome: Box::new(outcome),
            progress: ployz_store::RowTracker::default().changes(&rows),
            removed: Vec::new(),
        };
        store.record(&admitted.id, &runner, executed).unwrap();
    }
    /// Cloud asks its Servers before it sends a removal: its one Server is silent.
    fn silent(command: &StoreCommand) -> Trusted {
        let unanswered = vec![ployz_core::MachineId::parse(CLOUD_SERVER).unwrap()];
        Trusted {
            unanswered: matches!(command, StoreCommand::Remove(_)).then_some(unanswered),
            ..evidence()
        }
    }
    let cloud = Target::Cloud {
        url: cloud(worker, std::sync::Arc::default(), silent, ready()),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["env", "new", "staging"]);
    let web = ["service", "add", "web", "--image", "nginx:alpine"];
    ok(&cloud, &[&web[..], &["--env", "staging"]].concat());
    assert_eq!(
        ployz(Some(&cloud), &["deploy", "--env", "staging"]).0,
        Some(1)
    );

    let why = "It didn't answer, so shop/staging can't come off it yet";
    let (code, removed) = ployz(
        Some(&cloud),
        &["env", "rm", "staging", "--confirm", "shop/staging"],
    );
    assert_eq!(code, Some(3), "{removed}");
    assert_eq!(removed["removal"], "removing", "{removed}");
    assert_eq!(
        removed["waiting_on"],
        json!([{
            "server": "web-1", "server_id": CLOUD_SERVER, "why": why,
            "fix": { "server_remove": { "server": CLOUD_SERVER } },
        }]),
        "{removed}"
    );

    let home = tempfile::tempdir().unwrap();
    let output = cloud
        .command(home.path())
        .args(["env", "rm", "staging", "--confirm", "shop/staging"])
        .output()
        .unwrap();
    let said = String::from_utf8(output.stdout).unwrap();
    assert_eq!(output.status.code(), Some(3), "{said}");
    assert!(said.contains(&format!("Waiting on web-1: {why}")), "{said}");
    assert!(!said.contains(CLOUD_SERVER), "ids are machinery: {said}");
}

/// Run `ployz --json ARGS` on `stdin`. An input read without bound would fill memory,
/// so the run is killed after 15 s and the test fails instead of hanging.
fn ployz_on(
    store: &Target,
    args: &[&str],
    stdin: impl Into<std::process::Stdio>,
) -> (Option<i32>, Value) {
    let home = tempfile::tempdir().unwrap();
    let mut child = store
        .command(home.path())
        .arg("--json")
        .args(args)
        .stdin(stdin)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("{args:?} still reading after 15 s");
        }
        // real-wait: polls the CLI child, which signals nothing before it exits.
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json = serde_json::from_str(&stdout)
        .unwrap_or_else(|error| panic!("stdout is not one JSON object ({error}): {stdout:?}"));
    (output.status.code(), json)
}

/// The `--json` error of a run on `stdin` that fails with `code`, and its exit status:
/// 2 for `invalid_argument`, 1 otherwise.
fn failed_on(
    store: &Target,
    args: &[&str],
    stdin: impl Into<std::process::Stdio>,
    code: &str,
) -> Value {
    let (exit, json) = ployz_on(store, args, stdin);
    let expected = if code == "invalid_argument" { 2 } else { 1 };
    assert_eq!(exit, Some(expected), "{args:?}: {json}");
    assert_eq!(json["error"]["code"], code, "{json}");
    json["error"].clone()
}

/// A refusal of a too-long input, read no further than its limit.
fn refused_on(store: &Target, args: &[&str], stdin: impl Into<std::process::Stdio>) -> Value {
    failed_on(store, args, stdin, "invalid_argument")
}

fn dev_zero() -> std::fs::File {
    std::fs::File::open("/dev/zero").unwrap()
}

/// A temporary file holding `bytes`, opened for reading.
fn file_of(bytes: &[u8]) -> (tempfile::NamedTempFile, std::fs::File) {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), bytes).unwrap();
    let read = std::fs::File::open(file.path()).unwrap();
    (file, read)
}

const MAX_CLI_INPUT: usize = 32 * 1024 * 1024;

#[test]
fn an_input_past_the_cli_read_bound_is_refused_after_reading_one_byte_past_it() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    shop_with_web(store);
    ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
    let null = std::process::Stdio::null;
    let refusals = [
        (
            vec!["config", "put", "app", "x.txt", "--from", "/dev/zero"],
            null(),
            "/dev/zero: the CLI reads at most 32 MiB of a Config file; this one holds more",
            "/dev/zero",
        ),
        (
            vec!["config", "put", "app", "x.txt"],
            dev_zero().into(),
            "stdin: the CLI reads at most 32 MiB of a Config file; this one holds more",
            "stdin",
        ),
        (
            vec!["set", "web", "--from-env-file", "/dev/zero"],
            null(),
            "/dev/zero: the CLI reads at most 32 MiB of an env file; this one holds more",
            "/dev/zero",
        ),
        (
            vec!["set", "web", "--from-env-file", "-"],
            dev_zero().into(),
            "stdin: the CLI reads at most 32 MiB of an env file; this one holds more",
            "stdin",
        ),
        (
            vec!["set", "web.env.S", "--secret"],
            dev_zero().into(),
            "stdin: the CLI reads at most 32 MiB of a secret; this one holds more",
            "stdin",
        ),
    ];
    for (args, stdin, message, input) in refusals {
        let refused = refused_on(store, &args, stdin);
        assert_eq!(refused["message"], message);
        assert_eq!(
            refused["details"],
            json!({ "input": input, "max_bytes": MAX_CLI_INPUT })
        );
    }
    let (code, unset) = ployz(Some(store), &["get", "web.env.S"]);
    assert_ne!(code, Some(0), "nothing was set: {unset}");
}

#[test]
fn an_env_file_cloud_takes_as_one_edit_is_read_whole() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    shop_with_web(store);
    // A long comment past Cloud's 4 MiB request cap becomes a short edit.
    let mut text = b"#".to_vec();
    text.extend(std::iter::repeat_n(b'x', 4 * 1024 * 1024));
    text.extend(b"\nA=ok\n");
    assert_eq!(text.len(), 4_194_311);
    let (file, _) = file_of(&text);
    ok(
        store,
        &[
            "set",
            "web",
            "--from-env-file",
            file.path().to_str().unwrap(),
        ],
    );
    let (_, stdin) = file_of(&text);
    let (code, set) = ployz_on(store, &["set", "web", "--from-env-file", "-"], stdin);
    assert_eq!(code, Some(0), "{set}");
    assert_eq!(
        ok(store, &["get", "web.env.A"])["settings"][0]["value"],
        json!("ok")
    );
}

#[test]
fn below_the_cli_read_bound_the_store_judges_an_input_as_on_main() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    shop_with_web(store);
    ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
    let null = std::process::Stdio::null;
    let put = ["config", "put", "app", "x.txt", "--from"];
    let max_config_file = 256 * 1024;

    let (exact, _) = file_of(&vec![b'a'; max_config_file]);
    ok(
        store,
        &[&put[..], &[exact.path().to_str().unwrap()]].concat(),
    );
    for len in [max_config_file + 1, 396 * 1024] {
        let (big, stdin) = file_of(&vec![b'a'; len]);
        let details = json!({ "file": "x.txt", "bytes": len, "max_bytes": max_config_file });
        let big = big.path().to_str().unwrap();
        let refused = refused_on(store, &[&put[..], &[big]].concat(), null());
        assert_eq!(refused["details"], details, "{len}");
        let refused = refused_on(store, &put[..4], stdin);
        assert_eq!(refused["details"], details, "{len}");
    }

    // `S=VALUE` and its closing NUL: 128 KiB. A secret variable needs a Cloud that seals.
    let cloud = &cloud_target();
    shop_with_web(cloud);
    let longest = 128 * 1024 - "S=".len() - 1;
    let mut value = vec![b'v'; longest];
    value.push(b'\n');
    let (_, stdin) = file_of(&value);
    let (code, set) = ployz_on(cloud, &["set", "web.env.S", "--secret"], stdin);
    assert_eq!(code, Some(0), "{set}");
    for len in [longest + 1, 1024 * 1024] {
        let (_, stdin) = file_of(&vec![b'v'; len]);
        let refused = refused_on(cloud, &["set", "web.env.S", "--secret"], stdin);
        assert_eq!(refused["details"], json!({ "variable": "S" }), "{len}");
    }

    // A registry credential never becomes a variable, so the 128 KiB limit is not its.
    let (_, stdin) = file_of(&vec![b't'; 128 * 1024 + 1]);
    let (code, set) = ployz_on(store, &["set", "web.registryCredential", "--secret"], stdin);
    assert_eq!(code, Some(0), "{set}");

    // The path is judged before the value's length, as on main.
    let (_, stdin) = file_of(&vec![b'v'; 200 * 1024]);
    let refused = failed_on(
        store,
        &["set", "missing.env.S", "--secret"],
        stdin,
        "not_found",
    );
    assert!(refused["details"]["valid_children"].is_array(), "{refused}");
}

#[test]
fn an_unreadable_input_keeps_its_error_code() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    shop_with_web(store);
    ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
    let null = std::process::Stdio::null;

    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.yml");
    let missing = missing.to_str().unwrap();
    let put = ["config", "put", "app", "app.yml", "--from"];
    let refused = failed_on(store, &[&put[..], &[missing]].concat(), null(), "not_found");
    assert_eq!(refused["message"], format!("Can't read --from {missing}"));
    let refused = failed_on(
        store,
        &["set", "web", "--from-env-file", missing],
        null(),
        "invalid_argument",
    );
    assert_eq!(
        refused["message"],
        format!("Can't read the env file {missing}")
    );
    assert_eq!(
        refused["cause"],
        json!(["No such file or directory (os error 2)"])
    );
    let folder = dir.path().to_str().unwrap();
    let refused = failed_on(
        store,
        &["set", "web", "--from-env-file", folder],
        null(),
        "invalid_argument",
    );
    assert_eq!(
        refused["message"],
        format!("Can't read the env file {folder}")
    );
    assert_eq!(refused["cause"], json!(["Is a directory (os error 21)"]));

    let locked = dir.path().join("locked.yml");
    std::fs::write(&locked, "a: 1\n").unwrap();
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    // root reads it anyway.
    if std::fs::File::open(&locked).is_err() {
        let locked = locked.to_str().unwrap();
        let refused = failed_on(store, &[&put[..], &[locked]].concat(), null(), "internal");
        assert_eq!(refused["message"], format!("Can't read --from {locked}"));
        assert_eq!(refused["cause"], json!(["Permission denied (os error 13)"]));
    }

    let binary = || file_of(&[0xff, 0xfe, b'\n']).1;
    let refused = failed_on(
        store,
        &["set", "web.env.S", "--secret"],
        binary(),
        "internal",
    );
    assert_eq!(
        refused["message"],
        "web.env.S: the secret on stdin isn't UTF-8 text"
    );
    let refused = failed_on(
        store,
        &["set", "web", "--from-env-file", "-"],
        binary(),
        "internal",
    );
    assert_eq!(refused["message"], "stdin: an env file is UTF-8 text");
}

#[test]
fn config_put_names_the_file_it_cant_read_and_takes_dash_for_stdin() {
    let store = &Target::Local(tempfile::tempdir().unwrap());
    shop_with_web(store);
    ok(store, &["config", "add", "app", "--mount", "web:/etc/app"]);
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.yml");
    let missing = missing.to_str().unwrap();
    let refused = error(
        store,
        &["config", "put", "app", "app.yml", "--from", missing],
    );
    assert_eq!(refused["message"], format!("Can't read --from {missing}"));
    assert_eq!(
        refused["cause"],
        json!(["No such file or directory (os error 2)"])
    );

    let (code, put) = ployz_with_stdin(
        store,
        &["config", "put", "app", "app.yml", "--from", "-"],
        "a: 1\n",
    );
    assert_eq!(code, Some(0), "{put}");
    let listed = ok(store, &["config", "ls"]);
    assert!(listed.to_string().contains("app.yml"), "{listed}");
}

#[test]
fn a_local_volume_restore_reaches_for_the_servers_instead_of_needing_cloud() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    ok(&store, &["project", "new", "shop"]);
    ok(&store, &["service", "add", "db", "--image", "postgres:17"]);
    ok(
        &store,
        &["volume", "add", "data", "--mount", "db:/var/lib/postgresql"],
    );

    let (code, json) = ployz(
        Some(&store),
        &["volume", "restore", "data", "--from", "web-1"],
    );

    assert!(matches!(code, Some(1..=3)), "{code:?}: {json}");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(!message.contains("need Ployz Cloud"), "{message}");
}

#[test]
fn set_seals_each_new_secret_in_the_cli_and_cloud_never_sees_its_plain_text() {
    let keypair = Keypair::generate();
    let state = SealingState::Ready(keypair.public().to_string());
    let cloud = sealing_cloud(Some(state));
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "nginx:1"]);
    // A write with no new secret asks Cloud nothing about sealing.
    ok(&cloud, &["set", "web.env.LOG_LEVEL=info"]);
    assert!(requests_to(&cloud, "/api/config/sealing").is_empty());

    let (code, set) = ployz_with_stdin(
        &cloud,
        &["set", "web.env.TOKEN", "--secret"],
        "direct-s3cr3t\n",
    );
    assert_eq!(code, Some(0), "{set}");
    let asked = requests_to(&cloud, "/api/config/sealing");
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0]["target"], json!("edit"));
    assert_eq!(
        asked[0]["environment"],
        json!({ "project": null, "environment": null })
    );

    // An env file seals the variable that is already secret and keeps the rest plain;
    // with --secret it seals every value.
    let file = tempfile::NamedTempFile::new().unwrap();
    let path = file.path().to_str().unwrap();
    std::fs::write(file.path(), "TOKEN=rotated-s3cr3t\nLEVEL=info\n").unwrap();
    let imported = ok(&cloud, &["set", "web", "--from-env-file", path]);
    assert_eq!(
        imported["staged"],
        json!(["web.env.TOKEN", "web.env.LEVEL"])
    );
    std::fs::write(file.path(), "API_KEY=file-s3cr3t\nTOKEN=again-s3cr3t\n").unwrap();
    let imported = ok(&cloud, &["set", "web", "--from-env-file", path, "--secret"]);
    assert_eq!(
        imported["staged"],
        json!(["web.env.API_KEY", "web.env.TOKEN"])
    );
    let patch = r#"{"env":{"PATCHED":{"secret":"patch-s3cr3t"},"LEVEL":"debug"}}"#;
    let (code, patched) = ployz_with_stdin(&cloud, &["set", "web", "--patch", "-"], patch);
    assert_eq!(code, Some(0), "{patched}");
    assert_eq!(requests_to(&cloud, "/api/config/sealing").len(), 4);

    for plaintext in [
        "direct-s3cr3t",
        "rotated-s3cr3t",
        "file-s3cr3t",
        "again-s3cr3t",
        "patch-s3cr3t",
    ] {
        cloud_never_saw(&cloud, plaintext);
    }
    // Each ciphertext opens with the Cluster's key, as its variable, for the Store's
    // own Service and Environment: the Store took every write with the flag on.
    let sealed = sealed_in_writes(&cloud);
    assert_eq!(
        sealed.keys().collect::<Vec<_>>(),
        ["API_KEY", "PATCHED", "TOKEN"]
    );
    let last_token = &sealed["TOKEN"];
    assert_eq!(
        keypair.open(&authored(last_token), last_token).as_deref(),
        Some("again-s3cr3t")
    );
    let patched = &sealed["PATCHED"];
    assert_eq!(
        keypair.open(&authored(patched), patched).as_deref(),
        Some("patch-s3cr3t")
    );
    let api_key = &sealed["API_KEY"];
    assert_eq!(
        keypair.open(&authored(api_key), api_key).as_deref(),
        Some("file-s3cr3t")
    );
    assert_eq!(authored(api_key), authored(patched));
    assert_eq!(
        ok(&cloud, &["get", "web"])["values"]["env"],
        json!({
            "API_KEY": { "secret": true },
            "LEVEL": "debug",
            "LOG_LEVEL": "info",
            "PATCHED": { "secret": true },
            "TOKEN": { "secret": true },
        })
    );
}

#[test]
fn set_seals_a_secret_asked_with_its_exported_flag_and_keeps_the_flag() {
    let keypair = Keypair::generate();
    let state = SealingState::Ready(keypair.public().to_string());
    let cloud = sealing_cloud(Some(state));
    one_service(&cloud);
    let patch = r#"{"env":{"HIDDEN":{"value":{"secret":"hidden-s3cr3t"},"exported":true}}}"#;
    let (code, patched) = ployz_with_stdin(&cloud, &["set", "web", "--patch", "-"], patch);
    assert_eq!(code, Some(0), "{patched}");

    cloud_never_saw(&cloud, "hidden-s3cr3t");
    let hidden = &sealed_in_writes(&cloud)["HIDDEN"];
    assert_eq!(
        keypair.open(&authored(hidden), hidden).as_deref(),
        Some("hidden-s3cr3t")
    );
    assert_eq!(
        ok(&cloud, &["get", "web"])["values"]["env"]["HIDDEN"],
        json!({ "value": { "secret": true }, "exported": true })
    );
}

#[test]
fn a_plain_text_secret_sent_to_a_cloud_without_the_sealing_route_is_refused_with_the_update() {
    // An old CLI sends plain text, as this one does to a Cloud it can't ask.
    let cloud = sealing_cloud(None);
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "nginx:1"]);
    let (code, refused) = ployz_with_stdin(
        &cloud,
        &["set", "web.env.TOKEN", "--secret"],
        "old-s3cr3t\n",
    );
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(requests_to(&cloud, "/api/config/sealing").len(), 1);
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(
        refused["error"],
        json!({
            "code": "unsupported",
            "message": "TOKEN: update ployz to set a Sealed value.",
            "details": { "variable": "TOKEN", "next": "curl -fsSL https://ployz.sh | sh" },
            "cause": [],
        }),
        "{refused}"
    );

    let home = tempfile::tempdir().unwrap();
    let mut command = cloud.command(home.path());
    command
        .args(["set", "web.env.TOKEN", "--secret"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"old-s3cr3t\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("TOKEN: update ployz to set a Sealed value.")
            && stderr.contains("next: curl -fsSL https://ployz.sh | sh"),
        "{stderr}"
    );
    let missing = error(&cloud, &["get", "web.env.TOKEN"]);
    assert_eq!(missing["code"], json!("not_found"), "{missing}");
}

#[test]
fn set_refuses_a_new_secret_until_a_server_holds_the_sealing_key_and_writes_nothing() {
    let cloud = sealing_cloud(Some(SealingState::NoServer));
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "nginx:1"]);
    let writes = requests_to(&cloud, "/api/config/write").len();
    let (code, refused) = ployz_with_stdin(
        &cloud,
        &["set", "web.env.TOKEN", "--secret"],
        "never-s3cr3t\n",
    );
    assert_eq!(code, Some(1), "{refused}");
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(refused["error"]["code"], json!("conflict"), "{refused}");
    assert_eq!(
        refused["error"]["message"],
        json!("Sealed values need a server.")
    );
    assert_eq!(
        refused["error"]["details"]["next"],
        json!("ployz server add")
    );
    assert_eq!(requests_to(&cloud, "/api/config/write").len(), writes);
    cloud_never_saw(&cloud, "never-s3cr3t");
    let missing = error(&cloud, &["get", "web.env.TOKEN"]);
    assert_eq!(missing["code"], json!("not_found"), "{missing}");
    // A plain variable still goes through: nothing to seal.
    ok(&cloud, &["set", "web.env.LOG_LEVEL=info"]);
}

#[test]
fn set_fails_on_the_stores_not_found_from_sealing_and_writes_nothing() {
    let keypair = Keypair::generate();
    let state = SealingState::Ready(keypair.public().to_string());
    let cloud = sealing_cloud(Some(state));
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "nginx:1"]);
    let writes = requests_to(&cloud, "/api/config/write").len();
    let (code, refused) = ployz_with_stdin(
        &cloud,
        &["set", "web.env.TOKEN", "--secret", "--env", "nope"],
        "lost-s3cr3t\n",
    );
    assert_eq!(code, Some(1), "{refused}");
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(refused["error"]["code"], json!("not_found"), "{refused}");
    assert_eq!(requests_to(&cloud, "/api/config/sealing").len(), 1);
    assert_eq!(requests_to(&cloud, "/api/config/write").len(), writes);
    cloud_never_saw(&cloud, "lost-s3cr3t");
}

#[test]
fn env_sync_sealed_values_are_sealed_in_the_cli_for_the_receiver() {
    let keypair = Keypair::generate();
    let state = SealingState::Ready(keypair.public().to_string());
    let cloud = sealing_cloud(Some(state));
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "web", "--image", "web:1"]);
    ok(&cloud, &["env", "branch", "fix-key", "--copy", "web"]);
    let other = ["set", "web.env.OTHER_KEY", "--secret", "--env", "fix-key"];
    let (code, set) = ployz_with_stdin(&cloud, &other, "branch-key\n");
    assert_eq!(code, Some(0), "{set}");
    let branch_sealed = sealed_in_writes(&cloud)["OTHER_KEY"].clone();

    let to = ["env", "sync", "--to", "--env", "fix-key"];
    let (code, synced) = ployz_with_stdin(
        &cloud,
        &[&to[..], &["--value", "web.env.OTHER_KEY"]].concat(),
        "given-key\n",
    );
    assert_eq!(code, Some(0), "{synced}");
    let asked = requests_to(&cloud, "/api/config/sealing");
    assert_eq!(asked.len(), 2, "{asked:?}");
    assert_eq!(asked[1]["target"], json!("sync"));
    assert_eq!(asked[1]["rows"], json!(["web.env.OTHER_KEY"]));
    cloud_never_saw(&cloud, "given-key");
    cloud_never_saw(&cloud, "branch-key");
    // Sealed for production's own copy of web, not the Branch's.
    let landed = sealed_in_writes(&cloud)["OTHER_KEY"].clone();
    assert_ne!(authored(&landed), authored(&branch_sealed));
    assert_eq!(
        keypair.open(&authored(&landed), &landed).as_deref(),
        Some("given-key")
    );
    assert_eq!(
        keypair
            .open(&authored(&branch_sealed), &branch_sealed)
            .as_deref(),
        Some("branch-key")
    );
    assert_eq!(
        ok(&cloud, &["get", "web.env.OTHER_KEY"])["settings"][0]["value"],
        json!({ "secret": true })
    );
    // A sync without values asks nothing.
    ok(&cloud, &["set", "web.env.LEVEL=info", "--env", "fix-key"]);
    ok(&cloud, &to);
    assert_eq!(requests_to(&cloud, "/api/config/sealing").len(), 2);
}

/// What `args` with `stdin` printed and exited with against a fresh Cloud sealing to
/// the Cluster key, after `setup`; how many writes Cloud took for it; and that Cloud.
fn refused_by(
    setup: fn(&Target),
    args: &[&str],
    stdin: &str,
) -> (Option<i32>, Value, usize, Target) {
    let cloud = sealing_cloud(ready());
    setup(&cloud);
    let writes = requests_to(&cloud, "/api/config/write").len();
    let (code, refused) = ployz_with_stdin(&cloud, args, stdin);
    let refused = serde_json::from_str(&refused).unwrap();
    let written = requests_to(&cloud, "/api/config/write").len() - writes;
    (code, refused, written, cloud)
}

fn one_service(cloud: &Target) {
    ok(cloud, &["project", "new", "shop"]);
    ok(cloud, &["service", "add", "web", "--image", "nginx:1"]);
}

/// A Branch `fix-key` holding a secret production lacks.
fn branch_with_a_new_secret(cloud: &Target) {
    one_service(cloud);
    ok(cloud, &["env", "branch", "fix-key", "--copy", "web"]);
    let other = ["set", "web.env.OTHER_KEY", "--secret", "--env", "fix-key"];
    let (code, set) = ployz_with_stdin(cloud, &other, "branch-key\n");
    assert_eq!(code, Some(0), "{set}");
}

#[test]
fn set_refuses_a_secret_holding_a_nul_before_sealing_it() {
    let args = ["set", "web.env.TOKEN", "--secret"];
    let stdin = "nul\0s3cr3t\n";
    let (code, refused, written, _) = refused_by(one_service, &args, stdin);
    assert_eq!(code, Some(2), "{refused}");
    assert_eq!(
        refused["error"]["message"],
        json!("TOKEN: null characters are not allowed")
    );
    assert_eq!(written, 0);
}

#[test]
fn env_sync_refuses_an_empty_value_before_sealing_it() {
    let args = [
        "env",
        "sync",
        "--to",
        "--env",
        "fix-key",
        "--value",
        "web.env.OTHER_KEY",
    ];
    let (code, refused, written, _) = refused_by(branch_with_a_new_secret, &args, "\n");
    assert_eq!(code, Some(2), "{refused}");
    assert_eq!(
        refused["error"]["message"],
        json!("web.env.OTHER_KEY: a secret needs a value")
    );
    assert!(
        refused["error"]["details"]["row"]
            .as_str()
            .is_some_and(|row| row.ends_with(":variables.OTHER_KEY")),
        "{refused}"
    );
    assert_eq!(written, 0);
}

#[test]
fn env_sync_with_a_value_at_a_stale_version_names_the_plan() {
    let args = [
        "env",
        "sync",
        "--to",
        "--env",
        "fix-key",
        "--version",
        "0:0",
        "--value",
        "web.env.OTHER_KEY",
    ];
    let stdin = "given-key\n";
    let (code, refused, written, _) = refused_by(branch_with_a_new_secret, &args, stdin);
    assert_eq!(code, Some(1), "{refused}");
    assert_eq!(refused["error"]["code"], json!("conflict"), "{refused}");
    assert_eq!(
        refused["error"]["details"]["next"],
        json!("ployz env sync --to --plan --env fix-key")
    );
    assert!(refused["error"]["details"]["version"].is_string());
    assert_eq!(written, 0);
}

#[test]
fn a_patch_s_secret_under_a_bad_variable_name_is_refused_as_the_store_does_and_never_sent() {
    let patch = r#"{"env":{"1BAD":{"secret":"patch-s3cr3t"}}}"#;
    let args = ["set", "web", "--patch", "-"];
    let local = Target::Local(tempfile::tempdir().unwrap());
    one_service(&local);
    let (code, refused_locally) = ployz_with_stdin(&local, &args, patch);
    assert_eq!(code, Some(2), "{refused_locally}");
    let refused_locally: Value = serde_json::from_str(&refused_locally).unwrap();

    let (code, refused, written, cloud) = refused_by(one_service, &args, patch);
    assert_eq!((code, &refused), (Some(2), &refused_locally));
    assert_eq!(written, 0);
    cloud_never_saw(&cloud, "patch-s3cr3t");
}

#[test]
fn a_patch_s_secret_in_an_env_that_is_not_an_object_is_refused_as_the_store_does_and_never_sent() {
    let patch = r#"{"env":[{"TOKEN":{"secret":"array-s3cr3t"}}]}"#;
    let args = ["set", "web", "--patch", "-"];
    let local = Target::Local(tempfile::tempdir().unwrap());
    one_service(&local);
    let (code, refused_locally) = ployz_with_stdin(&local, &args, patch);
    assert_eq!(code, Some(2), "{refused_locally}");
    let refused_locally: Value = serde_json::from_str(&refused_locally).unwrap();
    assert_eq!(
        refused_locally["error"]["message"],
        json!("env is a JSON object of variables")
    );

    let (code, refused, written, cloud) = refused_by(one_service, &args, patch);
    assert_eq!((code, &refused), (Some(2), &refused_locally));
    assert_eq!(written, 0);
    cloud_never_saw(&cloud, "array-s3cr3t");
}

/// The in-process Store has no Cluster key to seal to, so a plain-text secret is refused
/// there as anywhere else, and nothing is written.
#[test]
fn a_local_store_refuses_a_plain_text_secret() {
    let local = Target::Local(tempfile::tempdir().unwrap());
    one_service(&local);
    let (code, refused) = ployz_with_stdin(
        &local,
        &["set", "web.env.TOKEN", "--secret"],
        "local-s3cr3t\n",
    );
    assert_eq!(code, Some(2), "{refused}");
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(
        refused["error"],
        json!({
            "code": "invalid_argument",
            "message": "TOKEN: secrets are sealed on your device",
            "details": { "variable": "TOKEN" },
            "cause": [],
        })
    );
    let missing = error(&local, &["get", "web.env.TOKEN"]);
    assert_eq!(missing["code"], json!("not_found"), "{missing}");
}

#[test]
fn env_sync_with_a_value_for_a_service_the_sync_brings_names_the_set_after_it() {
    fn branch_with_a_new_service(cloud: &Target) {
        one_service(cloud);
        ok(cloud, &["env", "branch", "fix-key", "--copy", "web"]);
        let api = [
            "service", "add", "api", "--image", "api:1", "--env", "fix-key",
        ];
        ok(cloud, &api);
        let key = ["set", "api.env.KEY", "--secret", "--env", "fix-key"];
        let (code, set) = ployz_with_stdin(cloud, &key, "branch-key\n");
        assert_eq!(code, Some(0), "{set}");
    }
    let args = [
        "env",
        "sync",
        "--to",
        "--env",
        "fix-key",
        "--value",
        "api.env.KEY",
    ];
    let (code, refused, written, cloud) =
        refused_by(branch_with_a_new_service, &args, "given-key\n");
    assert_eq!(code, Some(2), "{refused}");
    assert_eq!(
        refused["error"]["message"],
        json!("api.env.KEY: its Service isn't there yet, so nothing can be sealed for it")
    );
    assert_eq!(
        refused["error"]["details"]["next"],
        json!(
            "sync without --value first, then ployz set api.env.KEY --secret --env production --project shop"
        )
    );
    assert_eq!(written, 0);
    cloud_never_saw(&cloud, "given-key");
}

const MALFORMED_SECRET: &str = r#"{"env":{"TOKEN":{"secret":"malformed-s3cr3t","exported":true}}}"#;

/// The Store's refusal of [`MALFORMED_SECRET`], from a Local Store.
fn malformed_secret_refused_locally() -> Value {
    let local = Target::Local(tempfile::tempdir().unwrap());
    one_service(&local);
    let args = ["set", "web", "--patch", "-"];
    let (code, refused) = ployz_with_stdin(&local, &args, MALFORMED_SECRET);
    assert_eq!(code, Some(2), "{refused}");
    let refused: Value = serde_json::from_str(&refused).unwrap();
    assert_eq!(
        refused["error"]["message"],
        json!(r#"TOKEN: expected text or {"secret": true}"#)
    );
    refused
}

#[test]
fn a_patch_s_secret_beside_other_fields_is_refused_as_the_store_does_and_never_sent() {
    let refused_locally = malformed_secret_refused_locally();
    let args = ["set", "web", "--patch", "-"];
    let (code, refused, written, cloud) = refused_by(one_service, &args, MALFORMED_SECRET);
    assert_eq!((code, &refused), (Some(2), &refused_locally));
    assert_eq!(written, 0);
    cloud_never_saw(&cloud, "malformed-s3cr3t");
}
