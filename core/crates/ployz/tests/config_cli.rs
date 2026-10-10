//! Authoring through the Config Store: the JSON contract of `project new`, `env new`,
//! `service add`, `get`, `set`, `unset`, `diff`, `publish`, `discard`, `schema` and
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

use ployz_core::RpcError;
use ployz_core::config::ServiceGitAccess;
use ployz_store::{
    Actor, AuthorizedRepository, ClusterDomain, ClusterDomainStatus, Command as StoreCommand,
    ConfigStore, DomainEvidence, Hostname, OrganizationId, Principal, RunnerId, SealingKey,
    Trusted, Written,
};
use serde_json::{Value, json};

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

/// Cloud's `/api/config` contract over one in-memory Store: the bearer
/// `ployz_<org>` acts in Organization `<org>`; any other caller is refused 401.
/// Its GitHub: installation 7 grants `acme/web`, with branches `main` and `dev`.
/// Its worker runs each admitted Deployment with Cloud's runner, on a Cluster
/// whose only Server never answers.
fn fake_cloud() -> String {
    fake_cloud_with_dispatch(dispatch)
}

fn fake_cloud_with_dispatch(worker: fn(&std::sync::Arc<ConfigStore>, &Written)) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let store = std::sync::Arc::new(
        ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap(),
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            serve(&store, stream.unwrap(), worker).unwrap();
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
    mut stream: TcpStream,
    worker: fn(&std::sync::Arc<ConfigStore>, &Written),
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    let path = request.split(' ').nth(1).unwrap_or_default().to_owned();
    let (mut length, mut organization) = (0, None);
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
    let (status, reply) = match (organization, path.as_str()) {
        (None, _) => (401, json!({ "code": "UNAUTHORIZED" })),
        (Some(who), "/api/config/read") => answer(store.read_trusted(
            &who,
            &serde_json::from_slice::<ployz_store::Query>(&body).unwrap(),
            &evidence(),
        )),
        (Some(who), "/api/config/write") => {
            let command: StoreCommand = serde_json::from_slice(&body).unwrap();
            let written = store.write_trusted(&who, &command, &evidence());
            if let (StoreCommand::Admit(_) | StoreCommand::Start(_), Ok(written)) =
                (&command, &written)
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
        (Some(who), "/api/cli/organizations") => {
            let id = who.organization.as_str();
            let organization = json!({ "id": id, "slug": id, "name": id, "current": true });
            (200, json!({ "organizations": [organization] }))
        }
        (Some(_), "/api/cli/github") => (
            200,
            json!({
                "install_url": "https://github.com/apps/ployz/installations/new",
                "linked": true, "ready": true,
                "installations": [{ "id": 7, "account": "acme", "account_type": "Organization", "repositories": 1 }],
                "repositories": [{ "repository": "acme/web", "private": true, "default_branch": "main", "installation": 7 }],
            }),
        ),
        (Some(_), "/api/cli/github/branches?repository=acme/web") => (
            200,
            json!({ "repository": "acme/web", "access": "installation", "default_branch": "main", "branches": ["dev", "main"] }),
        ),
        (Some(_), "/api/cli/github/7") => (
            200,
            json!({
                "disconnected": { "id": 7, "account": "acme" },
                "uninstall_url": "https://github.com/organizations/acme/settings/installations/7",
            }),
        ),
        (Some(_), _) => (404, json!({ "code": "NOT_FOUND" })),
    };
    let reply = reply.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    )
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
        // Its Cluster has one Server, which never answers.
        servers: Some(1),
        ..github()
    }
}

/// Every upload the fake Cloud took, by Deployment ID.
static UPLOADS: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

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

fn answer<T: serde::Serialize>(result: Result<T, RpcError>) -> (u16, Value) {
    match result {
        Ok(value) => (200, serde_json::to_value(value).unwrap()),
        Err(error) => (409, json!({ "error": error })),
    }
}

#[test]
fn an_agent_creates_and_edits_an_image_service() {
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
                "web.startCommand"
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
            12
        );
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

        // Healthcheck and Private DNS set and unset like any Setting.
        let value = |path: &str| ok(store, &["get", path])["settings"][0]["value"].clone();
        ok(store, &["set", "web.healthcheck=/up"]);
        assert_eq!(
            value("web.healthcheck"),
            json!({ "path": "/up", "timeoutSeconds": 300 })
        );
        let patch = json!({"healthcheck":{"command":"pg_isready","timeoutSeconds":60}}).to_string();
        ok(store, &["set", "web", "--patch", &patch]);
        assert_eq!(
            value("web.healthcheck"),
            json!({ "command": "pg_isready", "timeoutSeconds": 60 })
        );
        ok(store, &["unset", "web.healthcheck"]);
        assert_eq!(value("web.healthcheck"), Value::Null);
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
        assert_eq!(value("web.healthcheck")["path"], "/café?escaped=%0A");
        ok(store, &["unset", "web.healthcheck"]);
        ok(store, &["set", "web.privateDns=front"]);
        assert_eq!(value("web.privateDns"), json!("front"));
        ok(store, &["unset", "web.privateDns"]);
        assert_eq!(value("web.privateDns"), json!("web"));

        let text = "alpha\nweb.env.SPOOF = true\t\u{1b}[31mcafé";
        ok(store, &["set", &format!("web.env.DISPLAY={text}")]);
        assert_eq!(value("web.env.DISPLAY"), text);
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
        assert!(human.contains("alpha\\nweb.env.SPOOF = true\\t\\u{1b}[31mcafé"));

        // Unsetting the image disconnects the source: the Service is empty again.
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
        let cloud = matches!(store, Target::Cloud { .. });
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);

        let generated = ok(store, &["domain", "add", "web", "--port", "8080"]);
        assert_eq!(generated["domain"]["kind"], json!("generated"));
        assert_eq!(generated["domain"]["prefix"], json!("web"));
        // Only Cloud holds a Cluster Domain, so only it names the hostname.
        let hostname = if cloud {
            json!("web.acme.ployz.app")
        } else {
            json!(null)
        };
        assert_eq!(generated["domain"]["hostname"], hostname);
        assert_eq!(generated["staged"], json!(["web"]));
        assert_eq!(generated["next"], json!("ployz deploy"));
        let again = ok(store, &["domain", "add", "web"]);
        assert_eq!((&again["staged"], again.get("next")), (&json!([]), None));

        // Its generated prefix changes, staged; another generated domain can't take it.
        let set = ok(store, &["domain", "set", "web", "Shop"]);
        assert_eq!(set["domain"]["prefix"], json!("shop"));
        assert_eq!(set["staged"], json!(["web"]));
        assert_eq!(set["next"], json!("ployz deploy"));
        ok(store, &["service", "add", "api", "--image", "api:1"]);
        ok(store, &["domain", "add", "api"]);
        let taken = error(store, &["domain", "set", "api", "shop"]);
        assert_eq!(taken["code"], json!("conflict"));
        let label = failed(store, &["domain", "set", "api", "not.one-label"], 2);
        assert!(!label["message"].as_str().unwrap().contains("not.one-label"));
        ok(store, &["domain", "set", "web", "web"]);

        assert_eq!(
            ok(store, &["domain", "add", "web", "App.Example.com"])["domain"]["hostname"],
            json!("app.example.com")
        );
        let usage = failed(store, &["domain", "add", "web", "not a host"], 2);
        assert!(!usage["message"].as_str().unwrap().contains("not a host"));

        let listed = ok(store, &["domain", "ls"]);
        let first = &listed["domains"][0];
        assert_eq!(first["status"], json!("setting_up"));
        assert_eq!(first["action"], json!({ "type": "deploy" }));
        assert_eq!(listed["next"], json!("ployz deploy"));
        let checked = ok(store, &["domain", "check", "web"]);
        assert_eq!(
            checked["domain"]["reason"],
            json!("Live after your next deploy")
        );
        let missing = error(store, &["domain", "check", "nope"]);
        assert_eq!(missing["code"], json!("not_found"));

        let removed = ok(store, &["domain", "rm", "web"]);
        assert_eq!(removed["domain"]["port"], json!(8080));
        assert_eq!(removed["next"], json!("ployz deploy"));
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

        // Unconfirmed, it names what goes and the exact retry; nothing changes.
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
        assert_eq!(default["code"], json!("conflict"));
        assert_eq!(
            default["details"]["next"],
            json!("ployz env default ENV --project shop")
        );

        let moved = ok(store, &["env", "default", "staging"]);
        assert_eq!(moved["environments"][1]["default"], json!(true));
        // Nothing of production ever ran, so it goes without a Server.
        let removed = ok(
            store,
            &["env", "rm", "production", "--confirm", "shop/production"],
        );
        assert_eq!(removed["environment"]["name"], json!("production"));
        assert_eq!(removed["deployment"], json!(null));
        let listed = ok(store, &["env", "ls"]);
        assert_eq!(listed["environments"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn a_project_removal_reports_applied_environments_before_a_later_refusal() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        let Written::Deployment(admitted) = written else {
            return;
        };
        // Keep the second authored Deployment queued, so production refuses removal.
        if admitted.number == 2 && !admitted.remove {
            return;
        }
        let runner = RunnerId::parse("successful-worker").unwrap();
        let claimed = store.claim(&admitted.id, &runner).unwrap();
        let preview = serde_json::from_value(json!({
            "namespace": claimed.intent.namespace, "operations": [],
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
        let outcome = serde_json::from_value(json!({"type": "success", "completed": []})).unwrap();
        store
            .record(
                &admitted.id,
                &runner,
                ployz_store::RunEvidence::Executed {
                    progress: Vec::new(),
                    outcome: Box::new(outcome),
                    removed: Vec::new(),
                },
            )
            .unwrap();
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
    let (exit, result) = ployz(
        Some(&cloud),
        &["project", "rm", "shop", "--confirm", "shop"],
    );
    assert_eq!(exit, Some(3), "{result}");
    assert_eq!(result["applied"].as_array().unwrap().len(), 1);
    assert_eq!(result["applied"][0]["status"], "applied");
    assert_eq!(result["applied"][0]["remove"], true);
    assert_eq!(result["follow_up_error"]["code"], "conflict");
    assert!(
        result["follow_up_error"]["details"]["next"]
            .as_str()
            .unwrap()
            .contains("deployment show")
    );
    let listed = ok(&cloud, &["deployment", "ls", "--env", "before-root"]);
    assert_eq!(listed["deployments"][0]["id"], result["applied"][0]["id"]);
    assert_eq!(listed["deployments"][0]["status"], "applied");
    // Retrying now refuses before new progress and keeps the ordinary error contract.
    let refused = error(&cloud, &["project", "rm", "shop", "--confirm", "shop"]);
    assert_eq!(refused["code"], "conflict");
    assert_eq!(refused["details"], result["follow_up_error"]["details"]);
    assert_eq!(
        ok(&cloud, &["project", "ls"])["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
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

        // Unconfirmed, it names every Environment with what goes, and the exact retry.
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
        assert_eq!(removed["environments"], json!(["staging", "production"]));
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

        // An Own Copy of a node it owns already is refused.
        assert_eq!(
            error(store, &["env", "copy", "db", "--env", "fix-web"])["code"],
            json!("conflict")
        );
        let unkept = ok(store, &["env", "keep", "--env", "fix-web", "--off"]);
        assert_eq!(unkept["branch"]["kept"], json!(false));
        assert!(unkept.get("next").is_none());

        // Conditional Syncs are a PR Environment's; a take names a retained one.
        let at_merge = error(
            store,
            &["env", "sync", "--to", "--env", "fix-web", "--at-merge"],
        );
        assert_eq!(at_merge["code"], json!("invalid_argument"));
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
        assert_eq!(take["code"], json!("not_found"));
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
        // `get` lists them by RowId, in RowId order.
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
        assert_eq!(whole.len(), 18);
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
            synced["when"],
            json!({ "kind": "now", "staged": ["web"], "closing": false })
        );
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
        assert_eq!(closed["when"]["closing"], json!(true));
        let listed = ok(store, &["env", "ls"]);
        assert_eq!(listed["environments"].as_array().unwrap().len(), 1);
    }
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
        assert_eq!(synced["when"]["staged"], json!(["web"]));
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
        // A rename is staged; a name another Volume has is refused.
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

        // Nothing deployed loses data, so there is nothing to accept, and --yes is no bypass.
        let accept = error(store, &["deploy", "--accept-volume-loss", "data"]);
        assert_eq!(accept["code"], json!("invalid_argument"));
        failed(store, &["deploy", "--yes"], 2);
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
        ok(store, &["publish"]);
        ok(store, &["set", "nginx.preDeployCommand=Jenje"]);
        let diff = ok(store, &["diff"]);
        let nginx = &diff["changes"][0];
        assert_eq!(nginx["lifecycle"], json!("create"), "{diff}");
        assert_eq!(
            nginx["settings"][0]["path"],
            json!("nginx.preDeployCommand")
        );
        ok(store, &["discard", "nginx.preDeployCommand"]);
        let diff = ok(store, &["diff"]);
        assert_eq!(diff["changes"][0]["settings"], json!([]), "{diff}");
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
            Some(&json!("ployz env new preview --project shop"))
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
fn an_agent_reviews_publishes_and_discards() {
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
            diff.pointer("/changes/0/settings/0/path"),
            Some(&json!("web.replicas"))
        );
        assert_eq!(
            diff.get("next"),
            Some(&json!(format!("ployz publish --version {version}")))
        );

        // A review taken before another edit is stale.
        ok(store, &["set", "web.startCommand=nginx"]);
        let stale = error(store, &["publish", "--version", &version]);
        assert_eq!(stale.get("code"), Some(&json!("conflict")));
        assert_eq!(stale.pointer("/details/next"), Some(&json!("ployz diff")));
        let fresh = stale
            .pointer("/details/diff/version")
            .unwrap()
            .as_str()
            .unwrap();

        let published = ok(store, &["publish", "--version", fresh]);
        assert_eq!(published.get("saved"), Some(&json!(1)));
        assert_eq!(published.get("created"), Some(&json!(true)));
        assert_eq!(published.get("next"), Some(&json!("ployz deploy")));
        let diff = ok(store, &["diff"]);
        assert_eq!(diff.get("published"), Some(&json!(true)));
        let version = diff["version"].as_str().unwrap();
        assert_eq!(
            diff.get("next"),
            Some(&json!(format!("ployz deploy --expect-version {version}"))),
            "published is not yet deployed"
        );

        let discarded = ok(store, &["discard", "web"]);
        assert_eq!(discarded.get("saved"), Some(&json!(1)));
        assert_eq!(discarded.get("next"), Some(&json!("ployz diff")));
        let inverse = ok(store, &["diff"]);
        assert_eq!(inverse.get("changes"), Some(&json!([])));
        assert_eq!(inverse.get("draft_count"), Some(&json!(1)));
        assert_eq!(inverse.get("published"), Some(&json!(false)));
        assert_eq!(
            inverse.pointer("/draft_changes/0/lifecycle"),
            Some(&json!("delete"))
        );
        let version = inverse["version"].as_str().unwrap();
        assert_eq!(
            inverse.get("next"),
            Some(&json!(format!("ployz publish --version {version}")))
        );
        assert_eq!(
            ok(store, &["history"])["revisions"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let saved = ok(store, &["publish", "--version", version]);
        assert_eq!(saved.get("saved"), Some(&json!(2)));
        assert_eq!(saved.get("created"), Some(&json!(true)));
        let after = ok(store, &["diff"]);
        assert_eq!(after.get("changes"), Some(&json!([])));
        assert_eq!(after.get("draft_count"), Some(&json!(0)));
        assert_eq!(after.get("published"), Some(&json!(true)));
        assert_eq!(
            ok(store, &["history"])["revisions"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(ok(store, &["publish"]).get("created"), Some(&json!(false)));
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
            12
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
            let (exit, output) = piped(store, &["set", "web", "--patch", "-"], patch);
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
        // The Environment's own variables and mounts complete too.
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
        Some(&json!("ployz token new"))
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
fn deployment_text_shows_the_deepest_cause_and_json_keeps_the_chain() {
    fn worker(store: &std::sync::Arc<ConfigStore>, written: &Written) {
        let Written::Deployment(admitted) = written else {
            return;
        };
        let runner = RunnerId::parse("failing-worker").unwrap();
        let claimed = store.claim(&admitted.id, &runner).unwrap();
        let cause = match admitted.number {
            3 => json!([]),
            _ => json!(["Writing Docker metadata failed", "No space left on device"]),
        };
        let evidence = if admitted.number == 2 {
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
        assert_eq!(code, Some(3), "{deployed}");
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
        assert_eq!(stale["details"]["next"], json!("ployz diff"));

        let (code, deployed) = ployz(
            Some(store),
            &[
                "deploy",
                "--connect",
                "tcp://127.0.0.1:1",
                "--ssh-timeout",
                "1",
            ],
        );
        assert_eq!(deployed["saved"], json!(1));
        // Its runner (this CLI, or Cloud's that `deploy` follows) finds no Server
        // answering, so it records that nothing ran.
        assert_eq!(code, Some(3), "{deployed}");
        assert_eq!(deployed["status"], json!("failed"));
        assert_eq!(deployed["outcome"]["type"], json!("not_executed"));
        assert_eq!(deployed["nodes"][0]["outcome"], json!("not_attempted"));
        let id = deployed["id"].as_str().unwrap().to_owned();
        assert_eq!(
            deployed["next"],
            json!(format!("ployz deployment show {}", deployed["number"]))
        );

        let shown = ok(store, &["deployment", "show", &id]);
        assert_eq!(shown["id"], json!(id));
        assert_eq!(shown.get("next"), None);

        // `status` says what is deploying, or what needs attention.
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
        // Its number, as everything prints it, names it too.
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
    let unreachable = ["--connect", "tcp://127.0.0.1:1", "--ssh-timeout", "1"];
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "app"]);
        let mut args = vec!["deploy", "--upload", dir];
        args.extend(unreachable);
        match store {
            Target::Local(_) => {
                // The CLI runs it; no Server answers, so nothing was built or ran.
                let (code, deployed) = ployz(Some(store), &args);
                assert_eq!(code, Some(3), "{deployed}");
                let upload = &deployed["upload"];
                assert_eq!(upload["base"], json!({"commit": head, "changed": true}));
                let digest = upload["digest"].as_str().unwrap();
                assert!(ployz_core::is_lower_hex(digest, 64), "{digest}");
                // A later deploy without a new upload reuses the latest one's images.
                let mut again = vec!["deploy"];
                again.extend(unreachable);
                let (code, redeployed) = ployz(Some(store), &again);
                assert_eq!(code, Some(3), "{redeployed}");
                assert_eq!(&redeployed["upload"], upload);
            }
            Target::Cloud { .. } => {
                // Cloud takes the upload first and names who sent it; its runner reaches no Server.
                let (code, deployed) = ployz(Some(store), &args);
                assert_eq!(code, Some(3), "{deployed}");
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
fn an_agent_retries_a_failed_deployment_and_reads_its_logs() {
    let unreachable = ["--connect", "tcp://127.0.0.1:1", "--ssh-timeout", "1"];
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let (code, deployed) = ployz(Some(store), &[&["deploy"][..], &unreachable].concat());
        assert_eq!(code, Some(3), "{deployed}");
        let failed_id = deployed["id"].as_str().unwrap().to_owned();
        // Saved since: a retry still ships what the failed one froze.
        ok(store, &["set", "web.replicas=2"]);
        ok(store, &["publish"]);

        let (code, retried) = ployz(
            Some(store),
            &[&["deployment", "retry", &failed_id][..], &unreachable].concat(),
        );
        assert_eq!(code, Some(3), "{retried}");
        assert_eq!(
            (&retried["number"], &retried["saved"], &retried["status"]),
            (&json!(2), &json!(1), &json!("failed"))
        );
        let id = retried["id"].as_str().unwrap();
        assert_ne!(id, failed_id);
        assert_eq!(retried["next"], json!("ployz deployment show 2"));

        // An ended Deployment can't start or be cancelled; the refusal shows it.
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
                "retry",
                "00000000-0000-4000-8000-000000000999",
            ],
        );
        assert_eq!(missing["code"], json!("not_found"));
        failed(store, &["deployment", "cancel", "not-an-id"], 2);

        // Logs of a Deployment look in its Environment; this Cluster never answers.
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

    let args = ["up", "--connect", "tcp://127.0.0.1:1", "--ssh-timeout", "1"];
    for store in &targets() {
        let home = tempfile::tempdir().unwrap();
        let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let dir = first.path().canonicalize().unwrap().join("My Shop");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\nEXPOSE 80\n").unwrap();

        // No Server answers, so the Deployment doesn't apply.
        let (code, up) = in_dir(store, home.path(), &dir, &args, &[]);
        assert_eq!(code, Some(3), "{up}");
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
        assert_eq!(code, Some(3), "{again}");
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
        // Its root Dockerfile builds it.
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
        let (code, taken) = in_dir(store, home.path(), &other, &args, &[]);
        assert_eq!(code, Some(1), "{taken}");
        assert_eq!(
            taken["error"]["details"]["next"],
            json!("ployz up --project my-shop")
        );
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

        // Renaming the Project moves this device's links with it.
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
        assert_eq!(status.pointer("/staged/published"), Some(&json!(false)));
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

        // Nothing yet: status still says who, and what to do first.
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
            Some(&json!("ployz env new preview --project blog"))
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

        // An empty Service connects a repository by setting it; unset disconnects it.
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
fn github_lists_branches_and_disconnects_in_cloud() {
    let [_, cloud] = targets();
    let listed = ok(&cloud, &["github", "ls"]);
    assert_eq!(listed["repositories"][0]["repository"], "acme/web");
    assert!(listed.get("next").is_none(), "{listed}");
    let branches = ok(&cloud, &["github", "ls", "acme/web"]);
    assert_eq!(branches["branches"], json!(["dev", "main"]));
    let missing = error(&cloud, &["github", "ls", "acme/nope"]);
    assert_eq!(missing["code"], "not_found");
    assert_eq!(
        failed(&cloud, &["github", "ls", "not a repo"], 2)["code"],
        "invalid_argument"
    );
    let removed = ok(&cloud, &["github", "disconnect", "7"]);
    assert_eq!(removed["disconnected"]["account"], "acme");
    assert_eq!(
        error(&cloud, &["github", "disconnect", "8"])["code"],
        "not_found"
    );
    let pending = ok(&cloud, &["github", "connect"]);
    assert_eq!(pending["status"], "pending");
    assert_eq!(pending["next"], "ployz github connect --wait");
}

/// Run `ployz --json ARGS` against `store` with `input` on stdin; returns (exit code, stdout).
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

fn piped(store: &Target, args: &[&str], input: &str) -> (Option<i32>, String) {
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
    for store in &targets() {
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
        let (code, sealed) = piped(store, &["set", "web.env.TOKEN", "--secret"], "s3cr3t\n");
        assert_eq!(code, Some(0), "{sealed}");
        assert!(!sealed.contains("s3cr3t"));
        assert_eq!(
            ok(store, &["get", "web.env.TOKEN"])["settings"][0]["value"],
            json!({ "secret": true })
        );
        let refused = error(store, &["set", "web.env.TOKEN=plain-text"]);
        assert_eq!(refused["code"], json!("invalid_argument"));
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
        let (_, deployed) = ployz(
            Some(store),
            &[
                "deploy",
                "--connect",
                "tcp://127.0.0.1:1",
                "--ssh-timeout",
                "1",
            ],
        );
        assert!(!deployed.to_string().contains("s3cr3t"), "{deployed}");
    }
}

#[test]
fn a_synced_secret_arrives_without_its_value_and_deploy_says_which_to_set() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "web:1"]);
        ok(store, &["env", "branch", "fix-web", "--copy", "web"]);
        let secret = ["set", "web.env.API_KEY", "--secret"];
        let (code, set) = piped(
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
                &json!({ "held": false })
            )
        );
        ok(store, &["env", "sync", "--to", "--env", "fix-web"]);

        let refused = error(store, &["deploy"]);
        assert_eq!(refused["code"], json!("conflict"), "{refused}");
        assert_eq!(refused["details"]["secrets"], json!(["web.env.API_KEY"]));
        assert_eq!(
            refused["details"]["next"],
            json!("ployz set web.env.API_KEY --secret --project shop --env production")
        );
        let (code, set) = piped(store, &secret, "prod-key\n");
        assert_eq!(code, Some(0), "{set}");
        // With its own value, production's Deploy is admitted.
        let (_, deployed) = ployz(Some(store), &["deploy"]);
        assert_eq!(deployed["number"], json!(1), "{deployed}");

        // A sync can give the receiver its value, a line of stdin per --value.
        ok(store, &["env", "branch", "fix-key", "--copy", "web"]);
        let other = ["set", "web.env.OTHER_KEY", "--secret", "--env", "fix-key"];
        let (code, set) = piped(store, &other, "branch-key\n");
        assert_eq!(code, Some(0), "{set}");
        let to = ["env", "sync", "--to", "--env", "fix-key"];
        let (code, short) = piped(
            store,
            &[
                &to[..],
                &["--value", "web.env.OTHER_KEY", "--value", "web.image"],
            ]
            .concat(),
            "given-key\n",
        );
        assert_eq!(code, Some(2), "{short}");
        let (code, synced) = piped(
            store,
            &[&to[..], &["--value", "web.env.OTHER_KEY"]].concat(),
            "given-key\n",
        );
        assert_eq!(code, Some(0), "{synced}");
        assert!(!synced.contains("given-key"), "{synced}");
        let (_, deployed) = ployz(Some(store), &["deploy"]);
        assert_eq!(deployed["number"], json!(2), "{deployed}");
    }
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
        let (code, first) = piped(store, &["set", path, "--secret"], "first-token\n");
        assert_eq!(code, Some(0), "{first}");
        let first: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["staged"], json!([path]));
        assert_eq!(first["immediate"], json!([path]));
        assert_eq!(
            ok(store, &["get", path])["settings"][0]["value"],
            json!({ "secret": true })
        );
        let body = r#"{"registryCredential":{"username":"octocat","secret":"second-token"}}"#;
        let (code, rotated) = piped(store, &["set", "web", "--patch", "-"], body);
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
    // Its only Server never answers, so nothing runs; the upload is Cloud's already.
    let (code, uploaded) = ployz(Some(&cloud), &["deploy", "--upload", dir]);
    assert_eq!(code, Some(3), "{uploaded}");
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

/// Only the in-process Store: Cloud reports the pull request, which the test does
/// here through the Store itself.
#[test]
fn an_agent_syncs_a_pr_environment_at_its_merge_and_withdraws_it() {
    let store = Target::Local(tempfile::tempdir().unwrap());
    let Target::Local(dir) = &store else {
        unreachable!("a local Store")
    };
    ok(&store, &["project", "new", "shop"]);
    let local = ConfigStore::open(
        &format!("sqlite:{}", dir.path().join("store.db").display()),
        SealingKey::from_file(&dir.path().join("store.db.key")).unwrap(),
    )
    .unwrap();
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
    ok(&store, &["publish"]);
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
        bot: false,
        head_branch: ployz_store::BranchName::parse("search").unwrap(),
        head: ployz_store::CommitSha::parse("1".repeat(40)).unwrap(),
        target_branch: ployz_store::BranchName::parse("main").unwrap(),
        commits: 1,
        open: true,
        merge_commit: None,
        merge_reached: None,
        updated: ployz_store::GithubTimestamp::parse("2026-09-29T10:00:01Z").unwrap(),
    };
    local
        .system(
            &who.organization,
            &ployz_store::SystemEvent::PullRequest(opened),
            &Trusted::default(),
        )
        .unwrap();
    drop(local);

    ok(&store, &["set", "--env", "pr-5", "web.env.MODE=fast"]);
    let (code, set) = piped(
        &store,
        &["set", "web.env.TOKEN", "--secret", "--env", "pr-5"],
        "pr-token\n",
    );
    assert_eq!(code, Some(0), "{set}");
    // From a PR Environment, --to with no value is its only Destination, at the merge.
    let plan = ok(&store, &["env", "sync", "--to", "--plan", "--env", "pr-5"]);
    assert_eq!(
        (&plan["into"]["name"], &plan["at_merge"]),
        (&json!("production"), &json!(5))
    );
    assert_eq!(labels(&plan["rows"]), ["web.env.MODE", "web.env.TOKEN"]);
    // Asked for, --at-merge stays in the command the plan offers next.
    let plan = ok(
        &store,
        &[
            "env",
            "sync",
            "--to",
            "--at-merge",
            "--plan",
            "--env",
            "pr-5",
        ],
    );
    assert!(
        plan["next"].as_str().unwrap().contains(" --at-merge "),
        "{plan}"
    );
    let synced = ok(&store, &["env", "sync", "--to", "--env", "pr-5"]);
    let conditional_sync = &synced["when"]["conditional_sync"];
    assert_eq!(
        (&synced["when"]["kind"], &conditional_sync["state"]),
        (&json!("at_merge"), &json!("standing"))
    );
    assert_eq!(
        labels(&conditional_sync["rows"]),
        ["web.env.MODE", "web.env.TOKEN"]
    );
    assert!(synced.get("next").is_none());

    // Production holds its own value for the secret, named as the plan names it.
    let (code, held) = piped(
        &store,
        &["set", "web.env.TOKEN", "--secret", "--at-merge", "5"],
        "prod-token\n",
    );
    assert_eq!(code, Some(0), "{held}");
    assert!(!held.contains("-token"), "{held}");
    let (code, unheld) = piped(
        &store,
        &["set", "web.env.MODE", "--secret", "--at-merge", "5"],
        "prod-token\n",
    );
    assert_eq!(code, Some(1), "{unheld}");

    let sync = synced["sync"].as_str().unwrap();
    let undone = ok(&store, &["env", "sync", "--undo", sync]);
    assert_eq!(undone["into"]["name"], json!("production"));
    let plan = ok(
        &store,
        &[
            "env",
            "sync",
            "--to",
            "--at-merge",
            "--plan",
            "--env",
            "pr-5",
        ],
    );
    assert_eq!(labels(&plan["rows"]), ["web.env.MODE", "web.env.TOKEN"]);
    failed(&store, &["env", "sync", "--undo", sync, "--only", "web"], 2);
    // Only a PR Environment syncs at a merge.
    let refused = error(&store, &["env", "sync", "--to", "pr-5", "--at-merge"]);
    assert_eq!(refused["code"], json!("invalid_argument"));
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
