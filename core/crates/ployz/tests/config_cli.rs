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
    ConfigStore, DomainEvidence, Hostname, OrganizationId, RunnerId, SealingKey, Trusted, Written,
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
        .env_remove("PLOYZ_CLOUD_URL")
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

fn error(store: &Target, args: &[&str]) -> Value {
    failed(store, args, 1)
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
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let store = std::sync::Arc::new(
        ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap(),
    );
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            serve(&store, stream.unwrap()).unwrap();
        }
    });
    url
}

/// Cloud's worker: run an admitted Deployment in the background.
fn dispatch(store: &std::sync::Arc<ConfigStore>, who: Actor, written: &Written) {
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
            who,
            id,
            RunnerId::parse("cloud-worker").unwrap(),
            vec![unreachable],
            Ok(Default::default()),
        ));
    });
}

fn serve(store: &std::sync::Arc<ConfigStore>, mut stream: TcpStream) -> std::io::Result<()> {
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
            organization = Some(Actor {
                organization: OrganizationId::parse(value.trim()).unwrap(),
            });
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    let (status, reply) = match (organization, path.as_str()) {
        (None, _) => (401, json!({ "code": "UNAUTHORIZED" })),
        (Some(who), "/api/config/read") => answer(store.read_trusted(
            &who,
            &serde_json::from_slice(&body).unwrap(),
            &evidence(&who),
        )),
        (Some(who), "/api/config/write") => {
            let command: StoreCommand = serde_json::from_slice(&body).unwrap();
            let written = store.write_trusted(&who, &command, &evidence(&who));
            if let (StoreCommand::Admit(_) | StoreCommand::Start(_), Ok(written)) =
                (&command, &written)
            {
                dispatch(store, who, written);
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

/// What this Cloud knows for `who`: its GitHub, and its Cluster Domain
/// `acme.ployz.app`, ready. Only Organization `pro` may add custom domains.
fn evidence(who: &Actor) -> Trusted {
    Trusted {
        domains: DomainEvidence {
            custom_domains: who.organization.as_str() == "pro",
            cluster_domain: Some(ClusterDomain {
                name: Hostname::parse("acme.ployz.app").unwrap(),
                status: ClusterDomainStatus::Ready,
            }),
            ..DomainEvidence::default()
        },
        uploader: Some(who.organization.as_str().to_owned()),
        ..github()
    }
}

/// Every upload the fake Cloud took, by Deployment ID.
static UPLOADS: std::sync::Mutex<std::collections::BTreeMap<String, Vec<u8>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn github() -> Trusted {
    Trusted {
        repositories: vec![AuthorizedRepository {
            repository: "acme/web".into(),
            repository_id: 11,
            access: ServiceGitAccess::GithubInstallation { installation_id: 7 },
            default_branch: "main".into(),
            branches: vec!["dev".into()],
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
                "web.image",
                "web.maxRetries",
                "web.memLimit",
                "web.preDeployCommand",
                "web.replicas",
                "web.restartPolicy",
                "web.startCommand"
            ]))
        );
        assert_eq!(added.get("immediate"), Some(&json!([])));

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
            9
        );
        assert_eq!(
            got.pointer("/settings/6"),
            Some(&json!({ "path": "web.replicas", "value": 3, "default": 1, "apply": "staged" }))
        );
        assert_eq!(
            got.get("values"),
            Some(&json!({
                "image": "nginx:1",
                "maxRetries": 10,
                "replicas": 3,
                "restartPolicy": "unless-stopped",
                "startCommand": "nginx -g 'daemon off;'",
            }))
        );

        ok(store, &["unset", "web.replicas"]);
        let got = ok(store, &["get", "web.replicas"]);
        assert_eq!(got.pointer("/settings/0/value"), Some(&json!(1)));
    }
}

#[test]
fn an_agent_adds_lists_renames_and_removes_services() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["service", "add", "web", "--image", "nginx:1"]);
        let empty = ok(store, &["service", "add", "worker"]);
        assert_eq!(empty.pointer("/next"), Some(&json!("ployz diff")));

        let renamed = ok(store, &["service", "rename", "web", "frontend"]);
        assert_eq!(
            renamed.get("service"),
            Some(
                &json!({ "id": renamed["service"]["id"], "name": "frontend", "private_dns": "web" })
            )
        );
        assert_eq!(renamed.get("staged"), Some(&json!(["frontend"])));
        assert_eq!(renamed.get("next"), Some(&json!("ployz diff")));
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

        // The hidden Store stands for a self-hosted Cloud; hosted Cloud needs Pro.
        let args = ["domain", "add", "web", "App.Example.com"];
        if cloud {
            let refused = error(store, &args);
            assert_eq!(refused["code"], json!("unsupported"));
            assert_eq!(refused["details"]["next"], json!("ployz billing upgrade"));
        } else {
            assert_eq!(
                ok(store, &args)["domain"]["hostname"],
                json!("app.example.com")
            );
        }
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

        // Unconfirmed, it names what goes and the exact retry; nothing changes.
        let unconfirmed = error(store, &["env", "rm", "production"]);
        assert_eq!(unconfirmed["code"], json!("confirmation_required"));
        assert_eq!(unconfirmed["details"]["services"], json!(["web"]));
        assert_eq!(
            unconfirmed["details"]["next"],
            json!("ployz env rm production --confirm production")
        );
        let default = error(
            store,
            &["env", "rm", "production", "--confirm", "production"],
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
            &["env", "rm", "production", "--confirm", "production"],
        );
        assert_eq!(removed["environment"]["name"], json!("production"));
        assert_eq!(removed["deployment"], json!(null));
        let listed = ok(store, &["env", "ls"]);
        assert_eq!(listed["environments"].as_array().unwrap().len(), 1);
    }
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
            unconfirmed["details"]["next"],
            json!("ployz project rm shop --confirm shop")
        );
        failed(store, &["project", "rm", "shop", "--confirm", "blog"], 2);

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

        // Update and Own Copy wait until the Branch runs its Working State.
        let unsettled = error(store, &["env", "update", "--env", "fix-web"]);
        assert_eq!(unsettled["code"], json!("conflict"));
        assert_eq!(
            unsettled["details"]["next"],
            json!("ployz diff --project shop --env fix-web")
        );
        assert_eq!(
            error(store, &["env", "copy", "db", "--env", "fix-web"])["code"],
            json!("conflict")
        );
        assert_eq!(
            error(store, &["env", "update"])["code"],
            json!("invalid_argument")
        );
        let unkept = ok(store, &["env", "keep", "--env", "fix-web", "--off"]);
        assert_eq!(unkept["immediate"], json!(["kept"]));
        assert_eq!(unkept["branch"]["kept"], json!(false));
        assert!(unkept.get("next").is_none());

        // Save: review, then move the picked change into production with its version.
        ok(store, &["set", "web.image=web:2", "--env", "fix-web"]);
        let plan = ok(store, &["env", "save", "--plan", "--env", "fix-web"]);
        assert_eq!(plan["into"]["name"], json!("production"));
        assert_eq!(
            plan["rows"],
            json!([{ "row": "web.image", "conflict": false, "choice": null, "from": "web:2", "into": "web:1" }])
        );
        let version = plan["version"].as_str().unwrap();
        assert_eq!(
            plan["next"],
            json!(format!("ployz env save --version {version} --env fix-web"))
        );
        let stale = error(
            store,
            &["env", "save", "--env", "fix-web", "--version", "0:0"],
        );
        assert_eq!(
            stale["details"]["next"],
            json!("ployz env save --plan --env fix-web")
        );
        failed(
            store,
            &[
                "env",
                "save",
                "--env",
                "fix-web",
                "--only",
                "web.image=maybe",
            ],
            2,
        );
        let saved = ok(
            store,
            &[
                "env",
                "save",
                "--env",
                "fix-web",
                "--only",
                "web.image",
                "--version",
                version,
            ],
        );
        assert_eq!(saved["staged"], json!(["web"]));
        assert_eq!(saved["next"], json!("ployz deploy --env production"));
        // Conditional Saves are a PR Environment's; a take names a retained one.
        let withdraw = error(store, &["env", "save", "--env", "fix-web", "--withdraw"]);
        assert_eq!(withdraw["code"], json!("invalid_argument"));
        failed(
            store,
            &["env", "save", "--withdraw", "--only", "web.image"],
            2,
        );
        let take = error(
            store,
            &[
                "env",
                "save",
                "--take",
                "00000000-0000-4000-8000-000000000099",
            ],
        );
        assert_eq!(take["code"], json!("not_found"));
        assert_eq!(
            saved["close"],
            json!("ployz env rm fix-web --confirm fix-web")
        );
        assert_eq!(
            ok(store, &["get", "web.image"])["settings"][0]["value"],
            json!("web:2")
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
        assert_eq!(added["next"], json!("ployz diff"));
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
            }])
        );
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
        let bad_name = error(
            store,
            &["service", "add", "SECRET-CANARY", "--image", "nginx:1"],
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
        assert_eq!(added.get("next"), Some(&json!("ployz diff")));
        let set = ok(store, &["set", "web.replicas=3"]);
        assert_eq!(set.get("next"), Some(&json!("ployz diff")));

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
            Some(&json!(format!("ployz deploy --expect-version {version}")))
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

        // The Service is published but not deployed: discarding it unpublishes it too.
        let discarded = ok(store, &["discard", "web"]);
        assert_eq!(discarded.get("saved"), Some(&json!(2)));
        assert_eq!(discarded.get("next"), Some(&json!("ployz diff")));
        assert_eq!(ok(store, &["diff"]).get("changes"), Some(&json!([])));
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
            9
        );

        for patch in [r#"{"memLimit": null}"#, "not json"] {
            let refused = error(store, &["set", "web", "--patch", patch]);
            assert_eq!(
                refused.get("code"),
                Some(&json!("invalid_argument")),
                "{patch}"
            );
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
    assert_eq!(code, Some(1));
    assert_eq!(
        json.pointer("/error/details/did_you_mean"),
        Some(&json!("restartPolicy"))
    );
    assert!(json.pointer("/error/details/valid_children").is_some());
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
    }
}

#[test]
fn an_ambiguous_project_names_the_rerun() {
    for store in &targets() {
        ok(store, &["project", "new", "shop"]);
        ok(store, &["project", "new", "blog"]);
        let refused = error(store, &["env", "new", "staging"]);
        assert_eq!(refused.get("code"), Some(&json!("ambiguous")));
        assert_eq!(
            refused.get("details"),
            Some(&json!({
                "projects": ["blog", "shop"],
                "next": "ployz env new staging --project PROJECT",
            }))
        );

        // The hint is built from accepted words, so rejected values and raw `--` stay out.
        let next = |args: &[&str]| error(store, args)["details"]["next"].clone();
        assert_eq!(
            next(&["set", "--env", "production", "web.replicas=SECRET-CANARY"]),
            json!("ployz set 'web.replicas=VALUE' --project PROJECT --env production")
        );
        assert_eq!(
            next(&["get", "--", "web"]),
            json!("ployz get web --project PROJECT")
        );
        // Guard flags survive the rerun, so a guarded write never turns blind.
        assert_eq!(
            next(&["set", "web.replicas=2", "--expect", "3"]),
            json!("ployz set 'web.replicas=VALUE' --expect 3 --project PROJECT")
        );
        assert_eq!(
            next(&["publish", "--version", "4:1:none"]),
            json!("ployz publish --version 4:1:none --project PROJECT")
        );
        assert_eq!(
            next(&["get", "--all"]),
            json!("ployz get --all --project PROJECT")
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
        assert_eq!(deployed["nodes"][0]["outcome"], json!("not_applied"));
        let id = deployed["id"].as_str().unwrap().to_owned();
        assert_eq!(
            deployed["next"],
            json!(format!("ployz deployment show {id}"))
        );

        let shown = ok(store, &["deployment", "show", &id]);
        assert_eq!(shown["id"], json!(id));
        assert_eq!(shown.get("next"), None);

        // `status` says what is deploying, or what needs attention.
        let status = ok(store, &["status"]);
        let show = json!(format!("ployz deployment show {id}"));
        assert_eq!(status["next"], show);
        assert_eq!(status["deploying"], json!([]));
        assert_eq!(status["attention"][0]["reason"], json!("deployment_failed"));
        assert_eq!(status["attention"][0]["deployment"], json!(id));
        let listed = ok(store, &["deployment", "ls", "--limit", "1"]);
        assert_eq!(listed["deployments"][0]["id"], json!(id));
        assert_eq!(listed["next_cursor"], Value::Null);
        failed(store, &["deployment", "show", "not-an-id"], 2);

        // Detached, `deploy` returns the Deployment Cloud's runner runs at once.
        match store {
            Target::Local(_) => {
                failed(store, &["deploy", "--detach"], 2);
            }
            Target::Cloud { .. } => {
                let detached = ok(store, &["deploy", "--detach"]);
                let id = detached["id"].as_str().unwrap();
                assert_eq!(detached["number"], json!(2));
                assert_eq!(
                    detached["next"],
                    json!(format!("ployz deployment show {id}"))
                );
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
        assert_eq!(
            retried["next"],
            json!(format!("ployz deployment show {id}"))
        );

        // An ended Deployment can't start or be cancelled; the refusal shows it.
        let show = json!(format!("ployz deployment show {failed_id}"));
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
        std::fs::write(dir.join("Dockerfile"), "FROM scratch\n").unwrap();

        // No Server answers, so the Deployment doesn't apply.
        let (code, up) = in_dir(store, home.path(), &dir, &args, &[]);
        assert_eq!(code, Some(3), "{up}");
        assert_eq!(up["directory"], json!(dir.to_str().unwrap()));
        let deployment = &up["deployment"];
        assert_eq!(deployment["environment"]["project"], json!("my-shop"));
        assert_eq!(deployment["environment"]["name"], json!("production"));
        assert_eq!(deployment["upload"]["base"], json!(null), "{up}");
        let id = deployment["id"].as_str().unwrap();
        assert_eq!(up["next"], json!(format!("ployz deployment show {id}")));
        match store {
            Target::Local(_) => {
                assert_eq!(up["urls"], json!([]));
                assert_eq!(up.get("dashboard"), None);
            }
            Target::Cloud { url, .. } => {
                assert_eq!(up["urls"], json!(["https://my-shop.acme.ployz.app"]));
                let namespace = deployment["namespace"].as_str().unwrap();
                assert_eq!(
                    up["dashboard"],
                    json!(format!("{url}/cloud/alice/my-shop/{namespace}"))
                );
                assert!(UPLOADS.lock().unwrap().contains_key(id));
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
        assert_eq!(code, Some(1));
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
        assert!(diff.to_string().contains("web.rootDir"), "{diff}");
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
        failed(&cloud, &["github", "ls", "not a repo"], 1)["code"],
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
        assert_eq!(set["next"], json!("ployz diff"));
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
            "# app\nexport QUOTED=\"a b\\n\"\nRAW='lit ${{ x }}'\nTOKEN=rotated-s3cr3t\nLEVEL=info # comment\n",
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
                "web.env.LEVEL"
            ])
        );
        let env = ok(store, &["get", "web"])["values"]["env"].clone();
        assert_eq!(
            env,
            json!({
                "DB_HOST": "db",
                "LEVEL": "info",
                "QUOTED": "a b\n",
                "RAW": "lit ${{ x }}",
                "TOKEN": { "secret": true },
            })
        );
        failed(
            store,
            &["set", "web", "--from-env-file", "/nonexistent/.env"],
            2,
        );

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
/// Deployment it's for; Cloud names the uploader. With no upload to build from, Cloud's
/// runner says a new upload is the fix.
#[test]
fn an_agent_uploads_a_directory_to_cloud_and_is_told_when_to_upload_again() {
    let cloud = Target::Cloud {
        url: fake_cloud(),
        token: "ployz_alice",
    };
    ok(&cloud, &["project", "new", "shop"]);
    ok(&cloud, &["service", "add", "app"]);
    let (code, never) = ployz(Some(&cloud), &["deploy"]);
    assert_eq!(code, Some(3), "{never}");
    assert_eq!(never["outcome"]["needs_upload"], json!(["app"]), "{never}");
    assert_eq!(
        never["next"],
        json!("ployz up --project shop --env production")
    );

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
