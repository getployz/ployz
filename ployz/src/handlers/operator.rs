use std::{
    io::{IsTerminal, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use chrono::{DateTime, Local, Utc};
use clap::ArgMatches;
use crossterm::terminal;
use futures_util::StreamExt;
use ployz_core::{
    ExecRequestFrame, ExecResponseFrame, LogEntry, LogOrigin, LogStream, LogsOptions,
    select_service,
};
use tokio::io::copy_bidirectional;
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{LoadOptions, load_project},
    context::Transport,
    operator::{
        ProxyPorts, exec_options, merge_logs, parse_proxy_ports, parse_service_args, parse_tail,
        select_proxy_container, service_logs_use_compose,
    },
};

use super::{Error, leaf_matches, service::with_client_context, string_values};

pub fn exec(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = leaf
        .get_one::<String>("service")
        .cloned()
        .ok_or_else(|| "Service selector is required".to_owned())?;
    let container = leaf.get_one::<String>("container").cloned();
    let command = leaf
        .get_many::<String>("command")
        .map(|values| values.cloned().collect())
        .unwrap_or_default();
    let options = exec_options(
        command,
        crate::operator::ExecMode {
            detach: leaf.get_flag("detach"),
            no_tty: leaf.get_flag("no-tty"),
            stdout_terminal: std::io::stdout().is_terminal(),
            stdin_terminal: std::io::stdin().is_terminal(),
        },
    )
    .map_err(|error| error.to_string())?;
    with_client_context(root, None, |client| {
        Box::pin(async move {
            let tty = options.tty;
            let detach = options.detach;
            let mut session = client
                .open_exec(&service, container.as_deref(), options)
                .await
                .map_err(|error| error.to_string())?;
            let _raw = tty.then(RawTerminal::enable).transpose()?;
            if tty {
                send_terminal_size(&session.input).await?;
            }
            let stdin_task = (!detach).then(|| spawn_stdin(session.input.clone()));
            let resize_task = tty.then(|| spawn_resize(session.input.clone()));
            drop(session.input);
            let mut exit = 0;
            while let Some(payload) = session.output.next().await {
                match ExecResponseFrame::decode(&payload.map_err(|error| error.to_string())?)
                    .map_err(|error| error.to_string())?
                {
                    ExecResponseFrame::ExecId(_) => {}
                    ExecResponseFrame::Stdout(bytes) => std::io::stdout()
                        .write_all(&bytes)
                        .map_err(|error| error.to_string())?,
                    ExecResponseFrame::Stderr(bytes) => std::io::stderr()
                        .write_all(&bytes)
                        .map_err(|error| error.to_string())?,
                    ExecResponseFrame::Exit(code) => exit = code,
                    ExecResponseFrame::Error(error) => return Err(error.message.into()),
                }
            }
            if let Some(task) = stdin_task {
                task.abort();
            }
            if let Some(task) = resize_task {
                task.abort();
            }
            if !detach && exit != 0 {
                return Err(Error::Exit(u8::try_from(exit).unwrap_or(1)));
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
    let (args, context, compose_selection) = if service_logs_use_compose(&explicit) {
        let project = load_project(&LoadOptions {
            command: "logs".into(),
            files: string_values(leaf, "file")
                .into_iter()
                .map(Into::into)
                .collect(),
            all_profiles: true,
            ..Default::default()
        })
        .map_err(|error| error.to_string())?;
        for warning in &project.warnings {
            eprintln!("WARNING: {warning}");
        }
        if project.services.is_empty() {
            return Err("no Services found in Compose file(s)".into());
        }
        let args = project
            .services
            .keys()
            .map(|service| crate::operator::ServiceArg {
                service: service.clone(),
                containers: vec![],
            })
            .collect();
        let direct = leaf.get_one::<String>("connect").map(String::as_str);
        let explicit_context = leaf.get_one::<String>("context").map(String::as_str);
        let context = project
            .selected_context(explicit_context, direct)
            .map(ToOwned::to_owned);
        (args, context, true)
    } else {
        (
            parse_service_args(&explicit).map_err(|error| error.to_string())?,
            None,
            false,
        )
    };
    let options = log_options(leaf)?;
    let machines = string_values(leaf, "machine");
    let utc = leaf.get_flag("utc");
    with_client_context(root, context.as_deref(), |client| {
        Box::pin(async move {
            let cancellation = cancellation_on_ctrl_c();
            let _parent = cancellation.clone().drop_guard();
            let opened = client
                .open_service_logs(
                    &args,
                    &machines,
                    options,
                    compose_selection,
                    cancellation.clone(),
                )
                .await
                .map_err(|error| error.to_string())?;
            for service in opened.skipped_services {
                eprintln!("WARNING: Service {service:?} is not in the Cluster; skipping");
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
    let machines = string_values(leaf, "machine");
    let options = log_options(leaf)?;
    let utc = leaf.get_flag("utc");
    with_client_context(root, None, |client| {
        Box::pin(async move {
            let cancellation = cancellation_on_ctrl_c();
            let _parent = cancellation.clone().drop_guard();
            let inputs = client
                .open_machine_logs(&services, &machines, options, cancellation.clone())
                .await
                .map_err(|error| error.to_string())?;
            print_logs(merge_logs(inputs, cancellation), utc).await
        })
    })
}

pub fn proxy(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = leaf
        .get_one::<String>("service")
        .cloned()
        .ok_or_else(|| "Service selector is required".to_owned())?;
    let ports = parse_proxy_ports(
        leaf.get_one::<String>("port")
            .ok_or_else(|| "proxy port is required".to_owned())?,
    )
    .map_err(|error| error.to_string())?;
    with_client_context(root, None, |client| {
        Box::pin(async move { run_proxy(client, &service, ports).await })
    })
}

async fn run_proxy(
    client: &mut crate::connect::Client,
    service_selector: &str,
    ports: ProxyPorts,
) -> Result<(), Error> {
    if !matches!(client.connection().transport(), Transport::Ssh { .. }) {
        return Err(format!("proxy dialing is unsupported over {}", client.connection()).into());
    }
    let live = client
        .live_services()
        .await
        .map_err(|error| error.to_string())?;
    let service =
        select_service(&live.services, service_selector).map_err(|error| error.to_string())?;
    let container = select_proxy_container(service).map_err(|error| error.to_string())?;
    let address = container.address.ok_or_else(|| {
        format!(
            "Container {} has no address on the ployz Docker network",
            container.container_id
        )
    })?;
    let remote = SocketAddr::new(IpAddr::V4(address.0), ports.remote);
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        ports.local,
    ))
    .await
    .map_err(|error| error.to_string())?;
    println!(
        "{} -> {remote} ({service_selector}/{})",
        listener.local_addr().map_err(|error| error.to_string())?,
        container.container_id
    );
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (mut local, _) = result.map_err(|error| error.to_string())?;
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
                result.map_err(|error| error.to_string())?;
                return Ok(());
            }
        }
    }
}

fn log_options(matches: &ArgMatches) -> Result<LogsOptions, Error> {
    Ok(LogsOptions {
        follow: matches.get_flag("follow"),
        tail: parse_tail(
            matches
                .get_one::<String>("tail")
                .ok_or_else(|| "log tail is required".to_owned())?,
        )
        .map_err(|error| error.to_string())?,
        since: matches
            .get_one::<String>("since")
            .cloned()
            .unwrap_or_default(),
        until: matches
            .get_one::<String>("until")
            .cloned()
            .unwrap_or_default(),
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
        let entry = entry?;
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
        let output: &mut dyn Write = if entry.stream == LogStream::Stderr {
            &mut std::io::stderr()
        } else {
            &mut std::io::stdout()
        };
        output
            .write_all(prefix.as_bytes())
            .and_then(|()| output.write_all(&entry.message))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
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

impl RawTerminal {
    fn enable() -> Result<Self, String> {
        terminal::enable_raw_mode().map_err(|error| error.to_string())?;
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
    let (width, height) = terminal::size().map_err(|error| error.to_string())?;
    sender
        .send(
            ExecRequestFrame::Resize { width, height }
                .encode()
                .map_err(|error| error.to_string())?,
        )
        .await
        .map_err(|_| "exec request stream closed".into())
}

#[cfg(unix)]
fn spawn_stdin(
    sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use std::io::Read;

        let Ok(stdin) = std::fs::File::open("/dev/stdin") else {
            return;
        };
        let Ok(mut stdin) = tokio::io::unix::AsyncFd::new(stdin) else {
            return;
        };
        let mut buffer = [0_u8; 8192];
        loop {
            let Ok(mut ready) = stdin.readable_mut().await else {
                return;
            };
            let read = match ready.try_io(|fd| fd.get_mut().read(&mut buffer)) {
                Ok(Ok(read)) => read,
                Ok(Err(_)) => return,
                Err(_) => continue,
            };
            if read == 0 {
                return;
            }
            let frame = ExecRequestFrame::Stdin(buffer.get(..read).unwrap_or_default().to_vec());
            let Ok(payload) = frame.encode() else {
                return;
            };
            if sender.send(payload).await.is_err() {
                return;
            }
        }
    })
}

#[cfg(not(unix))]
fn spawn_stdin(
    sender: tokio::sync::mpsc::Sender<ployz_core::OpaquePayload>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;

        let mut stdin = tokio::io::stdin();
        let mut buffer = [0_u8; 8192];
        loop {
            let Ok(read) = stdin.read(&mut buffer).await else {
                return;
            };
            if read == 0 {
                return;
            }
            let Ok(payload) = ExecRequestFrame::Stdin(buffer[..read].to_vec()).encode() else {
                return;
            };
            if sender.send(payload).await.is_err() {
                return;
            }
        }
    })
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
