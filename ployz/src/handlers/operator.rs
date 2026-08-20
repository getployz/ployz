use std::{
    io::{IsTerminal, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use chrono::{DateTime, Local, Utc};
use clap::ArgMatches;
use crossterm::terminal;
use futures_util::StreamExt;
use ployz_core::{
    ContainerSelector, ExecRequestFrame, ExecResponseFrame, FanoutSelector, LogBody, LogEntry,
    LogOrigin, LogsOptions, QualifiedService, ServiceSelector, select_service,
};
use tokio::io::copy_bidirectional;
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{LoadOptions, load_project},
    context::Transport,
    operator::{
        ExecMode, ProxyPorts, exec_options, merge_logs, open_exec, open_machine_logs,
        open_service_logs, parse_log_time, parse_proxy_ports, parse_service_args, parse_tail,
        select_proxy_container, service_logs_use_compose,
    },
};

use super::{Error, leaf_matches, string_values, with_client_context};

pub fn exec(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = ServiceSelector::parse(
        leaf.get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
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
    with_client_context(root, None, |client| {
        Box::pin(async move {
            let tty = options.tty;
            let detach = options.detach;
            let mut session = open_exec(client, &service, container.as_ref(), options).await?;
            let _raw = tty.then(RawTerminal::enable).transpose()?;
            if tty {
                send_terminal_size(&session.input).await?;
            }
            if !detach {
                spawn_stdin(session.input.clone());
            }
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
                    ExecResponseFrame::Exit(code) => exit = code,
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

pub fn service_logs(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let explicit = leaf
        .get_many::<String>("service-or-container")
        .map(|values| values.cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    service_logs_with(root, explicit)
}

pub(super) fn caddy_logs(root: &ArgMatches) -> Result<(), Error> {
    service_logs_with(root, vec![QualifiedService::system_caddy().to_string()])
}

fn service_logs_with(root: &ArgMatches, explicit: Vec<String>) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let options = log_options(leaf)?;
    let (args, context, compose_selection) = if service_logs_use_compose(&explicit) {
        let project = load_project(&LoadOptions {
            command: "logs".into(),
            files: string_values(leaf, "file")
                .into_iter()
                .map(Into::into)
                .collect(),
            all_profiles: true,
            ..Default::default()
        })?;
        for warning in &project.warnings {
            eprintln!("WARNING: {warning}");
        }
        if project.services.is_empty() {
            return Err(Error::usage("no Services found in Compose file(s)"));
        }
        let mut args = Vec::new();
        for service in project.services.keys() {
            args.push(crate::operator::ServiceArg {
                service: ServiceSelector::parse(service.as_str())?,
                containers: vec![],
            });
        }
        let direct = leaf.get_one::<String>("connect").map(String::as_str);
        let explicit_context = leaf.get_one::<String>("context").map(String::as_str);
        let context = project
            .selected_context(explicit_context, direct)
            .map(ToOwned::to_owned);
        (args, context, true)
    } else {
        (parse_service_args(&explicit)?, None, false)
    };
    let machines = parse_fanout_selectors(string_values(leaf, "machine"))?;
    let utc = leaf.get_flag("utc");
    with_client_context(root, context.as_deref(), |client| {
        Box::pin(async move {
            let cancellation = cancellation_on_ctrl_c();
            let _parent = cancellation.clone().drop_guard();
            let opened = open_service_logs(
                client,
                &args,
                &machines,
                options,
                compose_selection,
                cancellation.clone(),
            )
            .await?;
            for service in opened.skipped_services {
                eprintln!("WARNING: Service {service} is not in the Cluster; skipping");
            }
            print_logs(merge_logs(opened.inputs, cancellation), utc).await
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
    with_client_context(root, None, |client| {
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

pub fn proxy(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = ServiceSelector::parse(
        leaf.get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
    let ports = parse_proxy_ports(
        leaf.get_one::<String>("port")
            .ok_or_else(|| Error::usage("proxy port is required"))?,
    )?;
    with_client_context(root, None, |client| {
        Box::pin(async move { run_proxy(client, &service, ports).await })
    })
}

async fn run_proxy(
    client: &mut crate::connect::Client,
    service_selector: &ServiceSelector,
    ports: ProxyPorts,
) -> Result<(), Error> {
    if !matches!(client.connection().transport(), Transport::Ssh { .. }) {
        return Err(Error::usage(format!(
            "proxy dialing is unsupported over {}",
            client.connection()
        )));
    }
    let live = client.live_services().await?;
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
    println!(
        "{} -> {remote} ({service_selector}/{})",
        listener.local_addr()?,
        container.container_id
    );
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (mut local, _) = result?;
                let client = client.clone();
                let remote = remote.to_string();
                tokio::spawn(async move {
                    match client.dial_proxy("tcp", &remote).await {
                        Ok(mut upstream) => {
                            if let Err(error) = copy_bidirectional(&mut local, &mut upstream).await {
                                eprintln!("WARNING: proxy connection to {remote} failed: {error}");
                            }
                        }
                        Err(error) => eprintln!("WARNING: proxy connection to {remote} failed: {error}"),
                    }
                });
            }
            result = tokio::signal::ctrl_c() => {
                result?;
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

fn cancellation_on_ctrl_c() -> CancellationToken {
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = signal.cancelled() => {}
            result = tokio::signal::ctrl_c() => if result.is_ok() {
                signal.cancel();
            }
        }
    });
    cancellation
}

async fn print_logs(
    mut entries: tokio::sync::mpsc::Receiver<Result<LogEntry, String>>,
    utc: bool,
) -> Result<(), Error> {
    while let Some(entry) = entries.recv().await {
        let entry = entry.map_err(Error::usage)?;
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

struct RawTerminal;

fn write_stdout_frame(output: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
    output.write_all(bytes)?;
    output.flush()
}

impl RawTerminal {
    fn enable() -> Result<Self, Error> {
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

fn spawn_stdin(sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>) {
    spawn_stdin_reader(std::io::stdin(), sender);
}

fn spawn_stdin_reader(
    mut stdin: impl std::io::Read + Send + 'static,
    sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) {
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
            if sender.blocking_send(payload).is_err() {
                return;
            }
        }
    }));
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

        spawn_stdin_reader(file, sender);
        let mut actual = Vec::new();
        while let Some(payload) = receiver.recv().await {
            let ExecRequestFrame::Stdin(bytes) = ExecRequestFrame::decode(&payload).unwrap() else {
                panic!("unexpected stdin frame")
            };
            actual.extend(bytes);
        }
        assert_eq!(actual, input.to_vec());
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

    #[test]
    fn stalled_stdin_reader_does_not_block_runtime_shutdown() {
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

        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(async {
                let (sender, _receiver) = tokio::sync::mpsc::channel(1);
                spawn_stdin_reader(
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
