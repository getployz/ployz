//! PROTOTYPE — do not merge.

use std::{
    error::Error,
    fs::{self, OpenOptions},
    io::{self, ErrorKind, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use duroxide::{
    Client, OrchestrationContext, OrchestrationRegistry, OrchestrationStatus,
    providers::sqlite::SqliteProvider,
    runtime::{self, ObservabilityConfig, RuntimeOptions, registry::ActivityRegistry},
};
use serde::{Deserialize, Serialize};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const COORDINATOR: &str = "cluster-command-coordinator";
const TERMINAL_COORDINATOR: &str = "terminal-command-coordinator";
const COMMANDS: &str = "commands";
const SECRET: &str = "s3nt1nel-local-history-only-92b97";

#[derive(Clone, Deserialize, Serialize)]
struct ClusterCommand {
    id: u16,
    kind: CommandKind,
    secret: String,
}

#[derive(Clone, Deserialize, Serialize)]
enum CommandKind {
    Apply,
    CrashAfterEffect,
    Reject,
}

#[derive(Deserialize, Serialize)]
struct Outcome {
    id: u16,
    result: String,
}

struct Probe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    order: Mutex<Vec<u16>>,
    effect: PathBuf,
    abort_after_effect: bool,
}

struct Active<'a>(&'a AtomicUsize);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Probe {
    fn new(state_dir: &Path, abort_after_effect: bool) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            order: Mutex::new(Vec::new()),
            effect: state_dir.join("external-effect"),
            abort_after_effect,
        }
    }

    async fn execute(&self, command: ClusterCommand) -> std::result::Result<Outcome, String> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        let _active = Active(&self.active);

        match command.kind {
            CommandKind::Apply => tokio::time::sleep(Duration::from_millis(2)).await,
            CommandKind::Reject => return Err("domain rejected".into()),
            CommandKind::CrashAfterEffect => {
                match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&self.effect)
                {
                    Ok(mut file) => {
                        file.write_all(b"applied\n")
                            .map_err(|error| error.to_string())?;
                        file.sync_all().map_err(|error| error.to_string())?;
                        if self.abort_after_effect {
                            std::process::abort();
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
        }

        self.order
            .lock()
            .expect("order mutex poisoned")
            .push(command.id);
        Ok(Outcome {
            id: command.id,
            result: "applied".into(),
        })
    }
}

fn registries(probe: Arc<Probe>) -> (ActivityRegistry, OrchestrationRegistry) {
    let activities = ActivityRegistry::builder()
        .register_typed("execute-command", move |_ctx, command: ClusterCommand| {
            let probe = probe.clone();
            async move { probe.execute(command).await }
        })
        .build();

    let orchestrations = OrchestrationRegistry::builder()
        .register(
            "command-coordinator",
            |ctx: OrchestrationContext, _input: String| async move {
                loop {
                    let payload = ctx.dequeue_event(COMMANDS).await;
                    let command = match serde_json::from_str::<ClusterCommand>(&payload) {
                        Ok(command) => command,
                        Err(_) => {
                            ctx.set_custom_status(
                                r#"{"id":null,"result":"rejected:malformed_command"}"#,
                            );
                            continue;
                        }
                    };
                    let id = command.id;
                    let outcome = match ctx
                        .schedule_activity_typed::<ClusterCommand, Outcome>(
                            "execute-command",
                            &command,
                        )
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(_) => Outcome {
                            id,
                            result: "rejected:domain_rejected".into(),
                        },
                    };
                    ctx.set_custom_status(
                        serde_json::to_string(&outcome).expect("outcome serializes"),
                    );
                }
            },
        )
        .register(
            "terminal-coordinator",
            |_ctx: OrchestrationContext, _input: String| async { Ok("done".into()) },
        )
        .build();

    (activities, orchestrations)
}

fn runtime_options() -> RuntimeOptions {
    RuntimeOptions {
        dispatcher_min_poll_interval: Duration::from_millis(10),
        dispatcher_long_poll_timeout: Duration::from_millis(50),
        orchestration_concurrency: 1,
        worker_concurrency: 1,
        orchestrator_lock_timeout: Duration::from_secs(2),
        worker_lock_timeout: Duration::from_secs(2),
        observability: ObservabilityConfig {
            log_level: "error".into(),
            ..ObservabilityConfig::default()
        },
        ..RuntimeOptions::default()
    }
}

async fn open(
    state_dir: &Path,
    abort_after_effect: bool,
) -> Result<(
    Arc<SqliteProvider>,
    Arc<runtime::Runtime>,
    Client,
    Arc<Probe>,
)> {
    fs::create_dir_all(state_dir)?;
    fs::set_permissions(state_dir, fs::Permissions::from_mode(0o700))?;
    let database = database_path(state_dir);
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&database)?;
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600))?;

    let provider =
        Arc::new(SqliteProvider::new(&format!("sqlite://{}", database.display()), None).await?);
    let probe = Arc::new(Probe::new(state_dir, abort_after_effect));
    let (activities, orchestrations) = registries(probe.clone());
    let runtime = runtime::Runtime::start_with_options(
        provider.clone(),
        activities,
        orchestrations,
        runtime_options(),
    )
    .await;
    Ok((provider.clone(), runtime, Client::new(provider), probe))
}

fn command(id: u16, kind: CommandKind) -> ClusterCommand {
    ClusterCommand {
        id,
        kind,
        secret: SECRET.into(),
    }
}

async fn status(client: &Client) -> Result<OrchestrationStatus> {
    Ok(client.get_orchestration_status(COORDINATOR).await?)
}

async fn wait_running(client: &Client) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(status(client).await?, OrchestrationStatus::Running { .. }) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("coordinator did not become Running".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_terminal(client: &Client, instance: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if matches!(
            client.get_orchestration_status(instance).await?,
            OrchestrationStatus::Completed { .. } | OrchestrationStatus::Failed { .. }
        ) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("coordinator did not become terminal".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_outcome(client: &Client, expected_id: u16) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let OrchestrationStatus::Running {
            custom_status: Some(value),
            ..
        } = status(client).await?
        {
            let outcome: serde_json::Value = serde_json::from_str(&value)?;
            if outcome["id"] == expected_id {
                if value.contains(SECRET) {
                    return Err("secret escaped into public coordinator status".into());
                }
                return Ok(outcome["result"]
                    .as_str()
                    .ok_or("missing outcome result")?
                    .to_owned());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for outcome {expected_id}").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_status_result(client: &Client, expected: &str) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let OrchestrationStatus::Running {
            custom_status: Some(value),
            ..
        } = status(client).await?
        {
            let outcome: serde_json::Value = serde_json::from_str(&value)?;
            if outcome["result"] == expected {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for status {expected}").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_count(probe: &Probe, count: usize) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if probe.order.lock().expect("order mutex poisoned").len() == count {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {count} activities").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn database_paths(state_dir: &Path) -> [PathBuf; 3] {
    let database = database_path(state_dir);
    [
        database.clone(),
        PathBuf::from(format!("{}-wal", database.display())),
        PathBuf::from(format!("{}-shm", database.display())),
    ]
}

fn database_path(state_dir: &Path) -> PathBuf {
    state_dir.join("history.db")
}

async fn restrict_database_files(state_dir: &Path) -> Result<[u32; 3]> {
    let paths = database_paths(state_dir);
    let deadline = Instant::now() + Duration::from_secs(5);
    while paths.iter().any(|path| !path.exists()) {
        if Instant::now() >= deadline {
            return Err("SQLite DB/WAL/SHM were not all visible".into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let mut modes = [0; 3];
    for (index, path) in paths.iter().enumerate() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        modes[index] = fs::metadata(path)?.permissions().mode() & 0o777;
        if modes[index] != 0o600 {
            return Err(format!("{} has mode {:o}", path.display(), modes[index]).into());
        }
    }
    Ok(modes)
}

fn local_history_contains_secret(state_dir: &Path) -> Result<bool> {
    for path in database_paths(state_dir) {
        if path.exists()
            && fs::read(path)?
                .windows(SECRET.len())
                .any(|bytes| bytes == SECRET.as_bytes())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn effect_count(state_dir: &Path) -> Result<usize> {
    Ok(fs::read_to_string(state_dir.join("external-effect"))?
        .lines()
        .count())
}

async fn crash_child(state_dir: PathBuf) -> Result<()> {
    let (_provider, _runtime, client, _probe) = open(&state_dir, true).await?;
    wait_running(&client).await?;
    let _ = restrict_database_files(&state_dir).await?;
    println!("crash child: coordinator=Running before enqueue");
    io::stdout().flush()?;
    client
        .enqueue_event_typed(
            COORDINATOR,
            COMMANDS,
            &command(102, CommandKind::CrashAfterEffect),
        )
        .await?;
    tokio::time::sleep(Duration::from_secs(10)).await;
    Err("crash activity was not dispatched".into())
}

async fn run() -> Result<()> {
    let state_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("PROTOTYPE-duroxide-state");
    if state_dir.exists() {
        fs::remove_dir_all(&state_dir)?;
    }

    let (provider, runtime, client, probe) = open(&state_dir, false).await?;
    match status(&client).await? {
        OrchestrationStatus::NotFound => {
            println!("admission: coordinator=NotFound -> reject");
        }
        _ => return Err("fresh coordinator unexpectedly exists".into()),
    }
    client
        .start_orchestration(TERMINAL_COORDINATOR, "terminal-coordinator", "")
        .await?;
    wait_terminal(&client, TERMINAL_COORDINATOR).await?;
    println!("admission: coordinator=Completed -> reject");
    client
        .start_orchestration(COORDINATOR, "command-coordinator", "")
        .await?;
    wait_running(&client).await?;
    println!("admission: coordinator=Running before enqueue -> accept");

    let modes = restrict_database_files(&state_dir).await?;
    println!(
        "permissions: directory=0700 db={:04o} wal={:04o} shm={:04o}",
        modes[0], modes[1], modes[2]
    );

    for id in 0..100 {
        client
            .enqueue_event_typed(COORDINATOR, COMMANDS, &command(id, CommandKind::Apply))
            .await?;
    }
    wait_count(&probe, 100).await?;
    let order = probe.order.lock().expect("order mutex poisoned").clone();
    if order != (0..100).collect::<Vec<_>>() {
        return Err("commands did not execute in FIFO order".into());
    }
    if probe.max_active.load(Ordering::SeqCst) != 1 {
        return Err("activities overlapped".into());
    }
    println!("fifo: commands=100 order=0..99 max_active=1");

    runtime.shutdown(Some(200)).await;
    drop(client);
    drop(provider);

    let (provider, runtime, client, _probe) = open(&state_dir, false).await?;
    wait_running(&client).await?;
    client
        .enqueue_event_typed(COORDINATOR, COMMANDS, &command(100, CommandKind::Apply))
        .await?;
    if wait_outcome(&client, 100).await? != "applied" {
        return Err("resumed command failed".into());
    }
    println!("restart: same SQLite file coordinator=Running next=applied");
    runtime.shutdown(Some(200)).await;
    drop(client);
    drop(provider);

    let child = ProcessCommand::new(std::env::current_exe()?)
        .arg("--crash-child")
        .arg(&state_dir)
        .output()?;
    if child.status.success() {
        return Err("crash child unexpectedly exited successfully".into());
    }
    let child_output = String::from_utf8(child.stdout)?;
    if child_output.contains(SECRET) || !child_output.contains("coordinator=Running before enqueue")
    {
        return Err("crash-child output was missing or exposed the secret".into());
    }
    print!("{child_output}");

    tokio::time::sleep(Duration::from_millis(2200)).await;
    let (provider, runtime, client, _probe) = open(&state_dir, false).await?;
    if wait_outcome(&client, 102).await? != "applied" {
        return Err("redelivered crash command failed".into());
    }
    if effect_count(&state_dir)? != 1 {
        return Err("idempotent external effect ran more than once".into());
    }
    println!("crash window: child=aborted redelivery=applied external_effect_count=1");

    client
        .enqueue_event_typed(COORDINATOR, COMMANDS, &command(103, CommandKind::Reject))
        .await?;
    let rejected = wait_outcome(&client, 103).await?;
    client
        .enqueue_event_typed(COORDINATOR, COMMANDS, &command(104, CommandKind::Apply))
        .await?;
    let next = wait_outcome(&client, 104).await?;
    if rejected != "rejected:domain_rejected" || next != "applied" {
        return Err("domain outcome was not total".into());
    }
    println!("domain failure: outcome=rejected:domain_rejected next=applied");

    client
        .enqueue_event(COORDINATOR, COMMANDS, "{not-json")
        .await?;
    wait_status_result(&client, "rejected:malformed_command").await?;
    if !matches!(status(&client).await?, OrchestrationStatus::Running { .. }) {
        return Err("malformed input terminated the coordinator".into());
    }
    println!("malformed input: outcome=rejected:malformed_command coordinator=Running");

    let public_status = format!("{:?}", status(&client).await?);
    if public_status.contains(SECRET) || !local_history_contains_secret(&state_dir)? {
        return Err("secret boundary check failed".into());
    }
    println!("secret boundary: public_status=redacted root-local_history=contains_payload");

    let modes = restrict_database_files(&state_dir).await?;
    println!(
        "final permissions: db={:04o} wal={:04o} shm={:04o}",
        modes[0], modes[1], modes[2]
    );
    runtime.shutdown(Some(200)).await;
    drop(provider);
    println!("VERDICT: stock Duroxide passed the node-local FIFO qualification");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() == Some(std::ffi::OsStr::new("--crash-child")) {
        let state_dir = args.next().ok_or("missing crash-child state directory")?;
        return crash_child(PathBuf::from(state_dir)).await;
    }
    run().await
}
