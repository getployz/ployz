use std::{
    io::{IsTerminal, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use chrono::{DateTime, Local, Utc};
use clap::ArgMatches;
use clap::{Arg, ArgAction, Command};
use crossterm::terminal;
use futures_util::StreamExt;
use ployz_core::{
    ContainerSelector, EnvironmentValues, ExecRequestFrame, ExecResponseFrame, FanoutSelector,
    LogBody, LogEntry, LogOrigin, LogsOptions, Namespace, RpcErrorCode, ServiceSelector,
    select_service,
};
use ployz_store::{DeploymentId, NamespaceQuery};
use tokio::io::copy_bidirectional;

use crate::{
    cli::{base, env, log_flags, positional, switch, trailing, value},
    cloud_account::StoreCallError,
    cloud_login::LoginError,
    context::Transport,
    operator::{
        ExecMode, ProxyPorts, ServiceArg, exec_options, merge_logs, open_exec, open_machine_logs,
        open_service_logs, parse_log_time, parse_proxy_ports, parse_service_args, parse_tail,
        select_proxy_container,
    },
};

use super::{
    Error, cancellation_on_ctrl_c, leaf_matches, store::scoped, string_values, with_client,
};

pub(crate) fn exec_command() -> Command {
    scoped(base("exec", "Run a command in a Service's container"))
        .arg(value("container", None))
        .arg(switch("detach", Some('d')))
        .arg(switch("no-tty", Some('T')))
        .arg(positional("service", true))
        .arg(trailing("command"))
}

pub(crate) fn logs_command() -> Command {
    scoped(log_flags(base(
        "logs",
        "Show Service logs; every Service of the Environment when none is named",
    )))
    .arg(
        Arg::new("service-or-container")
            .value_name("SERVICE[:CONTAINER]")
            .num_args(0..)
            .action(ArgAction::Append),
    )
    .arg(
        value("deployment", None)
            .value_name("ID")
            .help("Only the running containers this Deployment created, in its Environment"),
    )
}

pub(crate) fn ps_command() -> Command {
    scoped(base(
        "ps",
        "List the Environment's containers across Servers",
    ))
    .arg(
        value("sort", None)
            .default_value("service")
            .value_parser(["service", "machine", "health"]),
    )
}

/// The Namespace the addressed Project and Environment run in, so a bare Service name
/// means that Environment's Service. `None` keeps the whole Cluster in view: nothing
/// was asked for, and no Config Store is reachable, it has no Project yet, or
/// `--connect`/`--context` name a Cluster directly.
pub(super) fn scope(root: &ArgMatches, words: &[&str]) -> Result<Option<Namespace>, Error> {
    let leaf = leaf_matches(root);
    let environment = super::store::environment(leaf)?;
    let asked = environment.project.is_some() || environment.environment.is_some();
    let direct = ["connect", "context"]
        .into_iter()
        .any(|id| matches!(leaf.try_get_one::<String>(id), Ok(Some(_))));
    if !asked && direct && std::env::var(env::STORE).is_err() {
        return Ok(None);
    }
    let Some(store) = super::store::reachable(root)? else {
        return if asked {
            Err(LoginError::SignedOut.into())
        } else {
            Ok(None)
        };
    };
    match store.namespace(&NamespaceQuery { environment }) {
        Ok(view) => Ok(Some(view.namespace)),
        Err(StoreCallError::Refused(error)) if !asked && error.code == RpcErrorCode::NotFound => {
            Ok(None)
        }
        Err(error) => Err(super::store::failed(leaf, words)(error)),
    }
}

/// `selector` in `namespace`: a bare Service Name becomes that Namespace's Service.
pub(super) fn in_scope(
    selector: ServiceSelector,
    namespace: Option<&Namespace>,
) -> Result<ServiceSelector, Error> {
    Ok(match namespace {
        Some(namespace) => selector.with_namespace(namespace)?,
        None => selector,
    })
}

pub fn exec(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = ServiceSelector::parse(
        leaf.get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
    let service = in_scope(service, scope(root, &["exec"])?.as_ref())?;
    let container = leaf
        .get_one::<String>("container")
        .filter(|selector| !selector.is_empty())
        .map(|selector| ContainerSelector::parse(selector.as_str()))
        .transpose()?;
    let command = leaf
        .get_many::<String>("command")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let options = exec_options(
        command,
        ExecMode::resolve(
            leaf.get_flag("detach"),
            leaf.get_flag("no-tty"),
            std::io::stdout().is_terminal(),
            std::io::stdin().is_terminal(),
        )?,
    );
    with_client(root, |client| {
        Box::pin(async move {
            let tty = options.tty;
            let detach = options.detach;
            let mut session = open_exec(client, &service, container.as_ref(), options).await?;
            let _raw = tty.then(RawTerminal::enable).transpose()?;
            if tty {
                send_terminal_size(&session.input).await?;
            }
            let _stdin =
                (!detach && stdin_is_readable()).then(|| spawn_stdin(session.input.clone()));
            let resize_task = tty.then(|| spawn_resize(session.input.clone()));
            drop(session.input);
            let mut exit = 0;
            while let Some(payload) = session.output.next().await {
                match ExecResponseFrame::decode(&payload?)? {
                    ExecResponseFrame::ExecId(_) => {}
                    ExecResponseFrame::Stdout(bytes) => {
                        write_stdout_frame(&mut std::io::stdout(), &bytes)?
                    }
                    ExecResponseFrame::Stderr(bytes) => std::io::stderr().write_all(&bytes)?,
                    ExecResponseFrame::Exit(code) => {
                        exit = code;
                        break;
                    }
                    ExecResponseFrame::Error(error) => return Err(error.into()),
                }
            }
            if let Some(task) = resize_task {
                task.abort();
            }
            if !detach && exit != 0 {
                return Err(Error::exit(u8::try_from(exit).unwrap_or(1)));
            }
            Ok(())
        })
    })
}

/// Stream Service logs; every Service of the Environment when none is named.
pub fn logs(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let named = string_values(leaf, "service-or-container");
    let options = log_options(leaf)?;
    let deployment = leaf
        .get_one::<String>("deployment")
        .is_some()
        .then(|| super::deploy::deployment_id(leaf, "deployment"))
        .transpose()?;
    // A Deployment names its Environment, whatever the scope says.
    let namespace = match &deployment {
        Some(id) => Some(
            super::store::store(root)?
                .deployment(id)
                .map_err(super::store::failed(leaf, &["logs"]))?
                .namespace,
        ),
        None => scope(root, &["logs"])?,
    };
    let args = parse_service_args(&named)?
        .into_iter()
        .map(|arg| {
            Ok(ServiceArg {
                service: in_scope(arg.service, namespace.as_ref())?,
                ..arg
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let machines = parse_fanout_selectors(string_values(leaf, "machine"))?;
    let utc = leaf.get_flag("utc");
    with_client(root, |client| {
        Box::pin(async move {
            let cancellation = cancellation_on_ctrl_c();
            let _parent = cancellation.clone().drop_guard();
            let inputs = open_service_logs(
                client,
                &args,
                namespace.as_ref(),
                &machines,
                options,
                cancellation.clone(),
                deployment.as_ref().map(DeploymentId::as_str),
            )
            .await?;
            print_logs(merge_logs(inputs, cancellation), utc).await
        })
    })
}

pub fn machine_logs(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let services = leaf
        .get_many::<String>("service")
        .map(|values| values.cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let machines = parse_fanout_selectors(string_values(leaf, "machine"))?;
    let options = log_options(leaf)?;
    let utc = leaf.get_flag("utc");
    with_client(root, |client| {
        Box::pin(async move {
            let cancellation = cancellation_on_ctrl_c();
            let _parent = cancellation.clone().drop_guard();
            let inputs =
                open_machine_logs(client, &services, &machines, options, cancellation.clone())
                    .await?;
            print_logs(merge_logs(inputs, cancellation), utc).await
        })
    })
}

/// Forward a loopback port to a healthy container of the Service until interrupted.
pub fn port_forward(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = ServiceSelector::parse(
        leaf.get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
    let service = in_scope(service, scope(root, &["service", "port-forward"])?.as_ref())?;
    let ports = parse_proxy_ports(
        leaf.get_one::<String>("port")
            .ok_or_else(|| Error::usage("port is required"))?,
    )?;
    with_client(root, |client| {
        Box::pin(async move { run_port_forward(client, &service, ports).await })
    })
}

async fn run_port_forward(
    client: &mut crate::connect::Client,
    service_selector: &ServiceSelector,
    ports: ProxyPorts,
) -> Result<(), Error> {
    if matches!(client.connection().transport(), Transport::Tcp(_)) {
        return Err(Error::coded(
            ployz_core::RpcErrorCode::Unsupported,
            format!("port-forward is unsupported over {}", client.connection()),
        ));
    }
    let live = client.live_services(EnvironmentValues::Redacted).await?;
    let services = live.services();
    let service = select_service(&services, service_selector)?;
    let container = select_proxy_container(service)?.as_observation();
    let address = container.address.ok_or_else(|| {
        Error::usage(format!(
            "Container {} has no address on the ployz Docker network",
            container.container_id
        ))
    })?;
    let remote = SocketAddr::new(IpAddr::V4(address.0), ports.remote);
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        ports.local,
    ))
    .await?;
    let local = listener.local_addr()?;
    if crate::output::json() {
        crate::output::emit_line(&serde_json::json!({
            "local": local,
            "remote": remote,
            "service": service_selector.to_string(),
            "container": container.container_id,
        }))?;
    } else {
        crate::output::say!(
            "{local} -> {remote} ({service_selector}/{}); Ctrl-C stops",
            container.container_id
        );
    }
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (mut local, _) = result?;
                let client = client.clone();
                let remote = remote.to_string();
                connections.spawn(async move {
                    match client.dial_proxy("tcp", &remote).await {
                        Ok(mut upstream) => {
                            if let Err(error) = copy_bidirectional(&mut local, &mut upstream).await {
                                eprintln!("WARNING: port-forward connection to {remote} failed: {error}");
                            }
                        }
                        Err(error) => eprintln!("WARNING: port-forward connection to {remote} failed: {error}"),
                    }
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            result = tokio::signal::ctrl_c() => {
                result?;
                // Dropping the set aborts every open connection and its tunnel.
                return Ok(());
            }
        }
    }
}

fn parse_fanout_selectors(values: Vec<String>) -> Result<Vec<FanoutSelector>, Error> {
    Ok(values
        .iter()
        .map(|selector| FanoutSelector::parse(selector.as_str()))
        .collect::<Result<Vec<_>, _>>()?)
}

fn log_options(matches: &ArgMatches) -> Result<LogsOptions, Error> {
    let now = Utc::now().timestamp();
    Ok(LogsOptions {
        follow: matches.get_flag("follow"),
        tail: parse_tail(
            matches
                .get_one::<String>("tail")
                .ok_or_else(|| Error::usage("log tail is required"))?,
        )?,
        since_unix_seconds: parse_log_time(
            matches
                .get_one::<String>("since")
                .map(String::as_str)
                .unwrap_or(""),
            now,
        )?,
        until_unix_seconds: parse_log_time(
            matches
                .get_one::<String>("until")
                .map(String::as_str)
                .unwrap_or(""),
            now,
        )?,
    })
}

async fn print_logs(
    mut entries: tokio::sync::mpsc::Receiver<Result<LogEntry, String>>,
    utc: bool,
) -> Result<(), Error> {
    while let Some(entry) = entries.recv().await {
        let entry = entry.map_err(Error::unavailable)?;
        if crate::output::json() {
            if let Some(line) = log_line(&entry) {
                crate::output::emit_line(&line)?;
            }
            continue;
        }
        let timestamp = timestamp(&entry, utc);
        let (service_name, service_id, container, hook) = match &entry.metadata.origin {
            LogOrigin::Service {
                service_id,
                service_name,
                container_id,
                hook,
            } => (
                service_name.as_str(),
                service_id.as_str(),
                format!("/{container_id}"),
                hook.as_deref()
                    .map_or(String::new(), |hook| format!(" ({hook})")),
            ),
            LogOrigin::Machine { service } => (
                service.as_str(),
                service.as_str(),
                String::new(),
                String::new(),
            ),
        };
        let prefix = format!(
            "{timestamp} {} {}/{}{}{} | ",
            entry.metadata.machine_name, service_name, service_id, container, hook,
        );
        let Some((message, stderr)) = printable_log_bytes(&entry.body) else {
            continue;
        };
        let output: &mut dyn Write = if stderr {
            &mut std::io::stderr()
        } else {
            &mut std::io::stdout()
        };
        output
            .write_all(prefix.as_bytes())
            .and_then(|()| output.write_all(message))?;
    }
    Ok(())
}

/// One `--json` log line; heartbeats and stream errors carry no log text.
fn log_line(entry: &LogEntry) -> Option<serde_json::Value> {
    let (message, stderr) = printable_log_bytes(&entry.body)?;
    let (service, service_id, container_id, hook) = match &entry.metadata.origin {
        LogOrigin::Service {
            service_id,
            service_name,
            container_id,
            hook,
        } => (
            service_name.as_str(),
            Some(service_id),
            Some(container_id),
            hook.as_deref(),
        ),
        LogOrigin::Machine { service } => (service.as_str(), None, None, None),
    };
    Some(serde_json::json!({
        "timestamp": timestamp(entry, true),
        "machine": entry.metadata.machine_name,
        "service": service,
        "service_id": service_id,
        "container_id": container_id,
        "hook": hook,
        "stream": if stderr { "stderr" } else { "stdout" },
        "message": String::from_utf8_lossy(message),
    }))
}

fn printable_log_bytes(body: &LogBody) -> Option<(&[u8], bool)> {
    match body {
        LogBody::Stdout(bytes) => Some((bytes, false)),
        LogBody::Stderr(bytes) => Some((bytes, true)),
        LogBody::Heartbeat | LogBody::Error(_) => None,
    }
}

fn timestamp(entry: &LogEntry, utc: bool) -> String {
    let seconds = entry.timestamp_unix_nanos.div_euclid(1_000_000_000);
    let nanos = entry.timestamp_unix_nanos.rem_euclid(1_000_000_000) as u32;
    let Some(timestamp) = DateTime::<Utc>::from_timestamp(seconds, nanos) else {
        return "0000-00-00T00:00:00Z".into();
    };
    if utc {
        timestamp.to_rfc3339()
    } else {
        timestamp.with_timezone(&Local).to_rfc3339()
    }
}

pub(super) struct RawTerminal;

fn write_stdout_frame(output: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
    output.write_all(bytes)?;
    output.flush()
}

impl RawTerminal {
    pub(super) fn enable() -> Result<Self, Error> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

async fn send_terminal_size(
    sender: &tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) -> Result<(), Error> {
    let (width, height) = terminal::size()?;
    sender
        .send(ExecRequestFrame::Resize { width, height }.encode()?)
        .await
        .map_err(|_| Error::usage("exec request stream closed"))
}

/// Reading terminal stdin from a background process group raises SIGTTIN and
/// stops the whole CLI, so a finished remote command could never terminate the
/// session — `timeout(1)` and most CI harnesses run the CLI in a background
/// group. Skip the reader there; exec then behaves as if stdin were closed.
// ponytail: checked once at exec start; a later Ctrl-Z/bg still stops a
// foreground-started reader, which is ordinary job control.
fn stdin_is_readable() -> bool {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return true;
    }
    rustix::termios::tcgetpgrp(&stdin).is_ok_and(|owner| owner == rustix::process::getpgrp())
}

#[must_use]
struct StdinReader {
    forward: tokio::task::JoinHandle<()>,
}

impl Drop for StdinReader {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

fn spawn_stdin(sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>) -> StdinReader {
    spawn_stdin_reader(std::io::stdin(), sender)
}

fn spawn_stdin_reader(
    mut stdin: impl std::io::Read + Send + 'static,
    sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) -> StdinReader {
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let forward = tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if sender.send(payload).await.is_err() {
                return;
            }
        }
    });
    // ponytail: a stalled reader can linger until CLI exit; add cancellable OS I/O if exec becomes reusable.
    drop(std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        while let Ok(read) = stdin.read(&mut buffer) {
            if read == 0 {
                return;
            }
            let frame = ExecRequestFrame::Stdin(buffer.split_at(read).0.to_vec());
            let Ok(payload) = frame.encode() else {
                return;
            };
            if tx.blocking_send(payload).is_err() {
                return;
            }
        }
    }));
    StdinReader { forward }
}

#[cfg(unix)]
fn spawn_resize(
    sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(mut resize) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())
        else {
            return;
        };
        while resize.recv().await.is_some() {
            let Ok((width, height)) = terminal::size() else {
                return;
            };
            let Ok(payload) = (ExecRequestFrame::Resize { width, height }).encode() else {
                return;
            };
            if sender.send(payload).await.is_err() {
                return;
            }
        }
    })
}

#[cfg(not(unix))]
fn spawn_resize(
    _sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {})
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufWriter, Read},
        sync::mpsc,
        time::Duration,
    };

    use super::*;

    #[tokio::test]
    async fn redirected_regular_file_stdin_is_framed() {
        let input = include_bytes!("../../Cargo.toml");
        let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);

        let _reader = spawn_stdin_reader(file, sender);
        let mut actual = Vec::new();
        while let Some(payload) = receiver.recv().await {
            let ExecRequestFrame::Stdin(bytes) = ExecRequestFrame::decode(&payload).unwrap() else {
                panic!("unexpected stdin frame")
            };
            actual.extend(bytes);
        }
        assert_eq!(actual, input.to_vec());
    }

    #[tokio::test]
    async fn dropping_stdin_reader_closes_the_request_channel_while_read_is_stalled() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let reader = spawn_stdin_reader(
            StalledRead {
                started: started_tx,
                release: release_rx,
            },
            sender,
        );
        tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(1)))
            .await
            .unwrap()
            .unwrap();
        drop(reader);
        let closed = tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await;
        release_tx.send(()).unwrap();
        assert!(
            matches!(closed, Ok(None)),
            "request stream stayed open after the stdin reader was dropped"
        );
    }

    #[test]
    fn streamed_stdout_frame_is_flushed() {
        let mut output = BufWriter::new(Vec::new());

        write_stdout_frame(&mut output, b"ready").unwrap();

        assert!(output.buffer().is_empty());
        assert_eq!(output.get_ref(), b"ready");
    }

    #[test]
    fn print_logs_never_writes_heartbeat_or_error_as_stdout() {
        assert_eq!(
            printable_log_bytes(&LogBody::Stdout(b"out".to_vec())),
            Some((b"out".as_slice(), false))
        );
        assert_eq!(
            printable_log_bytes(&LogBody::Stderr(b"err".to_vec())),
            Some((b"err".as_slice(), true))
        );
        assert_eq!(printable_log_bytes(&LogBody::Heartbeat), None);
        assert_eq!(printable_log_bytes(&LogBody::Error("nope".into())), None);
    }

    struct StalledRead {
        started: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Read for StalledRead {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            self.started.send(()).unwrap();
            self.release.recv().unwrap();
            Ok(0)
        }
    }

    #[test]
    fn stalled_stdin_reader_does_not_block_runtime_shutdown() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (sender, _receiver) = tokio::sync::mpsc::channel(1);
                let _reader = spawn_stdin_reader(
                    StalledRead {
                        started: started_tx,
                        release: release_rx,
                    },
                    sender,
                );
            });
            drop(runtime);
            finished_tx.send(()).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let shutdown = finished_rx.recv_timeout(Duration::from_secs(1));
        release_tx.send(()).unwrap();
        worker.join().unwrap();

        assert!(
            shutdown.is_ok(),
            "runtime waited for the stalled stdin read"
        );
    }
}
