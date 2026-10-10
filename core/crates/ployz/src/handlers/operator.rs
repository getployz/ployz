use std::{
    io::{IsTerminal, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
};

use std::collections::HashMap;

use chrono::{DateTime, Local, SecondsFormat, Utc};
use clap::{Arg, ArgAction, Command};
use clap::{ArgMatches, parser::ValueSource};
use crossterm::terminal;
use futures_util::StreamExt;
use ployz_core::{
    ContainerId, ContainerSelector, EnvironmentValues, ExecRequestFrame, ExecResponseFrame,
    FanoutSelector, HistoryContainerKind, HistoryGapReason, HistoryStream, LogBody, LogEntry,
    LogOrigin, LogsOptions, Namespace, RpcErrorCode, ServiceSelector, log_level, select_service,
};
use ployz_store::{DeploymentId, NamespaceQuery};
use tokio::io::copy_bidirectional;

use crate::{
    cli::{base, log_flags, positional, switch, trailing, value},
    cloud_account::StoreCallError,
    cloud_login::LoginError,
    context::Transport,
    operator::{
        ExecMode, OperatorError, PreparedHistory, ProxyPorts, ServiceArg, asked_machines,
        exec_options,
        history::{HistoryEvent, HistoryRecord, HistoryWindow, read_history},
        history_selectors, merge_logs, observe_service_logs, open_exec, open_machine_logs,
        open_service_logs, parse_log_time, parse_proxy_ports, parse_service_args, parse_tail,
        prepare_service_history, select_proxy_container,
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
        value("deployment", None).value_name("ID").help(
            "Only the containers this Deployment created, in its Environment, running or not",
        ),
    )
    .arg(
        switch("build", None)
            .requires("deployment")
            .help("The Deployment's build logs instead: its Git Services' builds, or those named"),
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
pub(super) fn scope(root: &ArgMatches) -> Result<Option<Scoped>, Error> {
    let leaf = leaf_matches(root);
    let environment = super::store::environment(leaf)?;
    let asked = environment.project.is_some() || environment.environment.is_some();
    let direct = ["connect", "context"]
        .into_iter()
        .any(|id| matches!(leaf.try_get_one::<String>(id), Ok(Some(_))));
    if !asked && direct && !super::store::local_mode() {
        return Ok(None);
    }
    let Some(store) = super::store::reachable(root)? else {
        return if asked {
            Err(LoginError::SignedOut.into())
        } else {
            Ok(None)
        };
    };
    match store.try_read(&NamespaceQuery { environment }) {
        Ok(view) => Ok(Some(Scoped {
            namespace: view.namespace,
            services: view.services,
        })),
        Err(StoreCallError::Refused(error)) if !asked && error.code == RpcErrorCode::NotFound => {
            Ok(None)
        }
        Err(error) => Err(store.fail(error)),
    }
}

/// The Namespace a live command acts in, and its Services' runtime names by the
/// names they have now.
pub(super) struct Scoped {
    pub(super) namespace: Namespace,
    services: std::collections::BTreeMap<ployz_core::ServiceName, ployz_core::ServiceName>,
}

/// `selector` in `scoped`: a bare Service Name becomes that Namespace's Service, by
/// its runtime name, so a renamed Service is still found.
pub(super) fn in_scope(
    selector: ServiceSelector,
    scoped: Option<&Scoped>,
) -> Result<ServiceSelector, Error> {
    let Some(scoped) = scoped else {
        return Ok(selector);
    };
    let runtime = ployz_core::ServiceName::parse(selector.as_str())
        .ok()
        .and_then(|name| scoped.services.get(&name))
        .map(|runtime| ServiceSelector::parse(runtime.to_string()))
        .transpose()?;
    Ok(runtime
        .unwrap_or(selector)
        .with_namespace(&scoped.namespace)?)
}

pub fn exec(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = ServiceSelector::parse(
        leaf.get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
    let service = in_scope(service, scope(root)?.as_ref())?;
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
            let mut exit = None;
            while let Some(payload) = session.output.next().await {
                match ExecResponseFrame::decode(&payload?)? {
                    ExecResponseFrame::ExecId(_) => {
                        if detach {
                            exit = Some(0);
                        }
                    }
                    ExecResponseFrame::Stdout(bytes) => {
                        write_stdout_frame(&mut std::io::stdout(), &bytes)?
                    }
                    ExecResponseFrame::Stderr(bytes) => std::io::stderr().write_all(&bytes)?,
                    ExecResponseFrame::Exit(code) => {
                        exit = Some(code);
                        break;
                    }
                    ExecResponseFrame::Error(error) => return Err(error.into()),
                }
            }
            if let Some(task) = resize_task {
                task.abort();
            }
            let exit = exit.ok_or_else(|| {
                Error::coded(
                    RpcErrorCode::Unavailable,
                    if detach {
                        "Exec stream ended before command start was confirmed"
                    } else {
                        "Exec stream ended without an exit status"
                    },
                )
            })?;
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
    let store = leaf
        .get_one::<String>("deployment")
        .is_some()
        .then(|| super::store::store(root))
        .transpose()?;
    let deployment = store
        .as_ref()
        .map(|store| super::deploy::deployment_id(leaf, store, "deployment"))
        .transpose()?;
    if let Some(id) = deployment.as_ref().filter(|_| leaf.get_flag("build")) {
        return build_logs(root, id, &named);
    }
    // A Deployment names its Environment, whatever the scope says.
    let namespace = match (&deployment, &store) {
        (Some(id), Some(store)) => {
            let view = store.read(&ployz_store::DeploymentQuery { id: id.clone() })?;
            Some(Scoped {
                namespace: view.namespace,
                services: view.runtime_names,
            })
        }
        _ => scope(root)?,
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
    let window = history_window(leaf, &options);
    with_client(root, |client| {
        Box::pin(async move {
            let started = Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX);
            let cancellation = cancellation_on_ctrl_c();
            let _parent = cancellation.clone().drop_guard();
            if let Some(prepared) = prepare_service_history(
                client,
                &machines,
                &args,
                &options,
                deployment.as_ref().map(DeploymentId::as_str),
            )
            .await?
            {
                return print_prepared_history(client, &prepared, window, &cancellation, utc).await;
            }
            let scope = observe_service_logs(client, &machines).await?;
            let names = &scope.unanswered.names;
            let named = || names.iter().map(|(machine_id, name)| (*machine_id, name));
            let mut gaps = crate::ui::Gaps::default().named(named());
            gaps.extend(&scope.unanswered.failures, &scope.unanswered.omissions);
            // Before reading, so a missing Service or Container still names the Server that did not answer.
            gaps.warn();
            let namespace = namespace.as_ref().map(|scoped| &scoped.namespace);
            let deployment = deployment.as_ref().map(DeploymentId::as_str);
            let selectors = history_selectors(&scope, &args, namespace, deployment)?;
            let mut printed = Printed::default();
            let failures = read_history(
                client,
                &scope.answered(),
                &selectors,
                window,
                &cancellation,
                |record| {
                    printed.record(&record);
                    print_history(&record, utc)
                },
            )
            .await?;
            let mut missed = crate::ui::Gaps::default().named(named());
            missed.extend(&failures, &[]);
            missed.warn();
            gaps.extend(&failures, &[]);
            if crate::ui::json() && gaps.outcome().is_err() {
                crate::ui::emit_line(&gaps)?;
            }
            if options.follow && !cancellation.is_cancelled() {
                let since = printed
                    .since(started)
                    .max(options.since_nanos.unwrap_or(i64::MIN));
                let live = LogsOptions {
                    tail: -1,
                    since_nanos: Some(since),
                    ..options
                };
                let deployments = scope.deployments();
                match open_service_logs(
                    client,
                    &scope,
                    &args,
                    namespace,
                    live,
                    cancellation.clone(),
                    deployment,
                )
                .await
                {
                    Ok(inputs) => {
                        let entries = merge_logs(inputs, cancellation);
                        print_logs(entries, utc, &deployments, |entry| printed.covers(entry))
                            .await?;
                    }
                    // Nothing runs to follow; the history above is the whole log.
                    Err(
                        OperatorError::NoServices
                        | OperatorError::NoDeploymentContainers
                        | OperatorError::NoContainersOnMachines { .. }
                        | OperatorError::NotRunning { .. },
                    ) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            gaps.outcome()
        })
    })
}

async fn print_prepared_history(
    client: &crate::connect::Client,
    prepared: &PreparedHistory,
    window: HistoryWindow,
    cancellation: &tokio_util::sync::CancellationToken,
    utc: bool,
) -> Result<(), Error> {
    let named = || {
        prepared
            .targets
            .iter()
            .chain(&prepared.omissions)
            .map(|(id, name)| (*id, name))
    };
    let mut gaps = crate::ui::Gaps::default().named(named());
    let omissions = prepared
        .omissions
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>();
    gaps.extend(&[], &omissions);
    gaps.warn();
    let failures = read_history(
        client,
        &prepared.targets,
        &prepared.selectors,
        window,
        cancellation,
        |record| print_history(&record, utc),
    )
    .await?;
    let mut missed = crate::ui::Gaps::default().named(named());
    missed.extend(&failures, &[]);
    missed.warn();
    gaps.extend(&failures, &[]);
    if crate::ui::json() && gaps.outcome().is_err() {
        crate::ui::emit_line(&gaps)?;
    }
    gaps.outcome()
}

/// Print Deployment `id`'s build logs: each Service in `named`, or every build.
fn build_logs(root: &ArgMatches, id: &DeploymentId, named: &[String]) -> Result<(), Error> {
    let store = super::store::store(root)?;
    let services = if named.is_empty() {
        store
            .read(&ployz_store::DeploymentQuery { id: id.clone() })?
            .builds
            .into_iter()
            .map(|build| build.service)
            .collect()
    } else {
        super::store::service_names(leaf_matches(root), "service-or-container")?
    };
    let builds = services
        .into_iter()
        .map(|service| {
            store.read(&ployz_store::BuildLogQuery {
                deployment: id.clone(),
                service,
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    crate::ui::finish(&serde_json::json!({ "builds": builds }), || {
        if builds.is_empty() {
            crate::ui::note("That Deployment built nothing.");
        }
        for build in &builds {
            crate::ui::stream(format_args!(
                "== {} from {}: {}",
                build.build.service,
                build
                    .build
                    .commit
                    .as_ref()
                    .map_or("the upload", ployz_store::CommitSha::as_str),
                super::store::word(&build.build.status)
            ));
            crate::ui::stream(format_args!("{}", build.log));
        }
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
            // A Server that can't take RPC is skipped, so it is named, not silently missing.
            let observed = client.machines().await?;
            let asked = asked_machines(&observed, &machines)?;
            let skipped = observed
                .iter()
                .filter(|machine| asked.contains(&machine.machine.id) && !machine.invites_rpc())
                .map(|machine| machine.machine.id)
                .collect::<Vec<_>>();
            let mut gaps = crate::ui::Gaps::default().named(
                observed
                    .iter()
                    .map(|machine| (machine.machine.id, &machine.machine.name)),
            );
            gaps.extend(&[], &skipped);
            gaps.warn();
            let inputs =
                open_machine_logs(client, &services, &machines, options, cancellation.clone())
                    .await?;
            print_logs(
                merge_logs(inputs, cancellation),
                utc,
                &HashMap::new(),
                |_| false,
            )
            .await?;
            gaps.outcome()
        })
    })
}

/// Forward a loopback port to a container of the Service that may serve, until interrupted.
pub fn port_forward(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let service = ServiceSelector::parse(
        leaf.get_one::<String>("service")
            .cloned()
            .ok_or_else(|| Error::usage("Service selector is required"))?,
    )?;
    let service = in_scope(service, scope(root)?.as_ref())?;
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
            container.display_name
        ))
    })?;
    let remote = SocketAddr::new(IpAddr::V4(address.0), ports.remote);
    let listener = tokio::net::TcpListener::bind(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        ports.local,
    ))
    .await?;
    let local = listener.local_addr()?;
    if crate::ui::json() {
        crate::ui::emit_line(&serde_json::json!({
            "local": local,
            "remote": remote,
            "service": service_selector.to_string(),
            "container": container.container_id,
        }))?;
    } else {
        crate::ui::stream(format_args!(
            "Forwarding {local} to {service_selector} ({}) port {}.",
            container.display_name, ports.remote
        ));
        crate::ui::note("Ctrl-C stops it.");
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
                                crate::ui::warn_cause(format_args!("port-forward connection to {remote} failed"), &error);
                            }
                        }
                        Err(error) => crate::ui::warn_cause(format_args!("port-forward connection to {remote} failed"), &error),
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
    let options = log_window(matches, now)?;
    // A window with no time in it would print nothing and look like an empty log.
    let since = options.since_nanos.map(|since| since.div_euclid(NANOS));
    match (since, options.until_unix_seconds) {
        (Some(since), Some(until)) if until < since => Err(Error::usage(
            "--until is before --since, so no line fits; swap them",
        )),
        (Some(since), _) if !options.follow && since > now => Err(Error::usage(
            "--since is in the future, so no line fits yet; add --follow to wait for them",
        )),
        _ => Ok(options),
    }
}

const NANOS: i64 = 1_000_000_000;

fn log_window(matches: &ArgMatches, now: i64) -> Result<LogsOptions, Error> {
    let time = |name: &str| {
        parse_log_time(
            matches
                .get_one::<String>(name)
                .map(String::as_str)
                .unwrap_or(""),
            now,
        )
    };
    Ok(LogsOptions {
        follow: matches.get_flag("follow"),
        tail: parse_tail(
            matches
                .get_one::<String>("tail")
                .ok_or_else(|| Error::usage("log tail is required"))?,
        )?,
        since_nanos: time("since")?.map(|since| since.saturating_mul(NANOS)),
        until_unix_seconds: time("until")?,
    })
}

/// `-n` reads the last lines of the window; `--since` alone, or `-n all`,
/// reads all of it.
fn history_window(matches: &ArgMatches, options: &LogsOptions) -> HistoryWindow {
    let since = options.since_nanos;
    let until = options
        .until_unix_seconds
        .map(|until| until.saturating_mul(NANOS));
    let explicit = matches.value_source("tail") == Some(ValueSource::CommandLine);
    match usize::try_from(options.tail) {
        Ok(lines) if explicit || since.is_none() => HistoryWindow::Last {
            lines,
            since,
            until,
        },
        _ => HistoryWindow::All { since, until },
    }
}

fn print_history(record: &HistoryRecord, utc: bool) -> Result<(), Error> {
    if crate::ui::json() {
        crate::ui::emit_line(&history_line(record))?;
        return Ok(());
    }
    let container = &record.container;
    let id = container.container_id.as_str();
    let service = container
        .service
        .as_deref()
        .unwrap_or(container.replica.as_str());
    let hook = if container.kind == HistoryContainerKind::PreDeployHook {
        " (pre-deploy)"
    } else {
        ""
    };
    let prefix = format!(
        "{} {} {service}/{}{hook} | ",
        format_time(record.ts(), utc),
        record.machine,
        id.get(..12).unwrap_or(id),
    );
    match &record.event {
        HistoryEvent::Line { stream, text, .. } => {
            let output: &mut dyn Write = if *stream == HistoryStream::Stderr {
                &mut std::io::stderr()
            } else {
                &mut std::io::stdout()
            };
            output.write_all(prefix.as_bytes())?;
            output.write_all(text)?;
            output.write_all(b"\n")?;
        }
        HistoryEvent::Gap { to, reason, .. } => {
            let why = match reason {
                HistoryGapReason::NotCaptured => "not captured",
                HistoryGapReason::Corrupt => "unreadable",
            };
            crate::ui::note(format_args!(
                "{prefix}lines until {} are missing: {why}",
                format_time(*to, utc)
            ));
        }
        HistoryEvent::Exit {
            exit_code,
            oom_killed,
            ..
        } => {
            let code =
                exit_code.map_or_else(|| "an unknown code".to_owned(), |code| code.to_string());
            let oom = if *oom_killed {
                ", killed out of memory"
            } else {
                ""
            };
            crate::ui::note(format_args!("{prefix}exited with {code}{oom}"));
        }
    }
    Ok(())
}

/// One `--json` history record. A line carries the live fields too.
fn history_line(record: &HistoryRecord) -> serde_json::Value {
    let container = &record.container;
    let ts = record.ts();
    let mut line = serde_json::json!({
        "timestamp": format_time(ts, true),
        "ts": json_time(ts),
        "machine": record.machine,
        "namespace": container.namespace,
        "service": container.service,
        "deployment": container.deployment,
        "replica": container.replica,
        "container": container.container_id,
        "container_id": container.container_id,
        "hook": (container.kind == HistoryContainerKind::PreDeployHook).then_some("pre-deploy"),
    });
    let fields = match &record.event {
        HistoryEvent::Line { stream, text, .. } => serde_json::json!({
            "stream": stream,
            "level": log_level(text),
            "line": String::from_utf8_lossy(text),
            "message": format!("{}\n", String::from_utf8_lossy(text)),
        }),
        HistoryEvent::Gap {
            from, to, reason, ..
        } => serde_json::json!({
            "gap": { "from": json_time(*from), "to": json_time(*to), "reason": reason },
        }),
        HistoryEvent::Exit {
            exit_code,
            oom_killed,
            ..
        } => serde_json::json!({
            "exit": { "code": exit_code, "oom_killed": oom_killed },
        }),
    };
    if let (Some(line), serde_json::Value::Object(fields)) = (line.as_object_mut(), fields) {
        line.extend(fields);
    }
    line
}

/// What history printed, so `-f` follows on without repeating or skipping a
/// line. Each container resumes after its own last printed line: one Server's
/// store can lag another's, so a single cut for all of them drops lines.
#[derive(Debug, Default)]
struct Printed {
    containers: HashMap<ContainerId, i64>,
    first: Option<i64>,
}

impl Printed {
    fn record(&mut self, record: &HistoryRecord) {
        if let HistoryEvent::Line { ts, .. } | HistoryEvent::Exit { ts, .. } = record.event {
            let container = self
                .containers
                .entry(record.container.container_id)
                .or_insert(ts);
            *container = (*container).max(ts);
            self.first = Some(self.first.map_or(ts, |first| first.min(ts)));
        }
    }

    /// Where the live read starts: the start of what history printed, so a
    /// container whose store had none of its lines yet still shows them all.
    fn since(&self, started: i64) -> i64 {
        self.first.unwrap_or(started)
    }

    /// A live line history already printed. A container history printed
    /// nothing from is never covered.
    fn covers(&self, entry: &LogEntry) -> bool {
        let printed = match &entry.metadata.origin {
            LogOrigin::Service { container_id, .. } => self.containers.get(container_id),
            LogOrigin::Machine { .. } => None,
        };
        printed.is_some_and(|ts| entry.timestamp_unix_nanos <= *ts)
    }
}

async fn print_logs(
    mut entries: tokio::sync::mpsc::Receiver<Result<LogEntry, String>>,
    utc: bool,
    deployments: &HashMap<ContainerId, String>,
    printed: impl Fn(&LogEntry) -> bool,
) -> Result<(), Error> {
    while let Some(entry) = entries.recv().await {
        let entry = entry.map_err(Error::unavailable)?;
        if printed(&entry) {
            continue;
        }
        if crate::ui::json() {
            if let Some(line) = log_line(&entry, deployments) {
                crate::ui::emit_line(&line)?;
            }
            continue;
        }
        let timestamp = format_time(entry.timestamp_unix_nanos, utc);
        // The Service and a `ps`-length container ID; --json carries the full IDs.
        let (service_name, container, hook) = match &entry.metadata.origin {
            LogOrigin::Service {
                service_name,
                container_id,
                hook,
                ..
            } => (
                service_name.as_str(),
                format!(
                    "/{}",
                    container_id
                        .as_str()
                        .get(..12)
                        .unwrap_or(container_id.as_str())
                ),
                hook.as_deref()
                    .map_or(String::new(), |hook| format!(" ({hook})")),
            ),
            LogOrigin::Machine { service } => (service.as_str(), String::new(), String::new()),
        };
        let prefix = format!(
            "{timestamp} {} {}{}{} | ",
            entry.metadata.machine_name, service_name, container, hook,
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
fn log_line(
    entry: &LogEntry,
    deployments: &HashMap<ContainerId, String>,
) -> Option<serde_json::Value> {
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
    let line = message.strip_suffix(b"\n").unwrap_or(message);
    Some(serde_json::json!({
        "timestamp": format_time(entry.timestamp_unix_nanos, true),
        "ts": json_time(entry.timestamp_unix_nanos),
        "machine": entry.metadata.machine_name,
        "service": service,
        "service_id": service_id,
        "container": container_id,
        "container_id": container_id,
        "deployment": container_id.and_then(|id| deployments.get(id)),
        "hook": hook,
        "stream": if stderr { "stderr" } else { "stdout" },
        "level": log_level(line),
        "line": String::from_utf8_lossy(line),
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

fn format_time(nanos: i64, utc: bool) -> String {
    let Some(time) = utc_time(nanos) else {
        return "0000-00-00T00:00:00Z".into();
    };
    if utc {
        time.to_rfc3339()
    } else {
        time.with_timezone(&Local).to_rfc3339()
    }
}

/// `--json` time: RFC 3339 in UTC with all nine fraction digits, so it sorts as text.
fn json_time(nanos: i64) -> Option<String> {
    utc_time(nanos).map(|time| time.to_rfc3339_opts(SecondsFormat::Nanos, true))
}

fn utc_time(nanos: i64) -> Option<DateTime<Utc>> {
    let seconds = nanos.div_euclid(NANOS);
    let fraction = u32::try_from(nanos.rem_euclid(NANOS)).ok()?;
    DateTime::<Utc>::from_timestamp(seconds, fraction)
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

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn a_log_window_with_no_time_in_it_is_refused() {
        let options = |args: &[&str]| {
            let root = crate::cli::command().try_get_matches_from(args).unwrap();
            log_options(leaf_matches(&root)).is_ok()
        };
        assert!(!options(&[
            "ployz", "logs", "web", "--since", "1m", "--until", "5m"
        ]));
        assert!(!options(&["ployz", "logs", "web", "--since", "2999-01-01"]));
        assert!(options(&[
            "ployz",
            "logs",
            "web",
            "--since",
            "2999-01-01",
            "-f"
        ]));
        assert!(options(&[
            "ployz", "logs", "web", "--since", "5m", "--until", "1m"
        ]));
    }

    #[test]
    fn since_reads_the_whole_window_and_tail_reads_its_end() {
        let window = |args: &[&str]| {
            let root = crate::cli::command().try_get_matches_from(args).unwrap();
            let leaf = leaf_matches(&root);
            history_window(leaf, &log_options(leaf).unwrap())
        };
        assert_eq!(
            window(&["ployz", "logs", "web"]),
            HistoryWindow::Last {
                lines: 100,
                since: None,
                until: None
            }
        );
        let HistoryWindow::All {
            since: Some(since), ..
        } = window(&["ployz", "logs", "web", "--since", "2h"])
        else {
            panic!("--since alone reads the whole window");
        };
        assert_eq!(since % NANOS, 0);
        assert!(matches!(
            window(&["ployz", "logs", "web", "--since", "2h", "-n", "5"]),
            HistoryWindow::Last {
                lines: 5,
                since: Some(_),
                ..
            }
        ));
        assert!(matches!(
            window(&["ployz", "logs", "web", "-n", "all"]),
            HistoryWindow::All { since: None, .. }
        ));
    }

    #[test]
    fn a_server_whose_history_failed_makes_the_command_partial() {
        let mut gaps = crate::ui::Gaps::default();
        assert!(gaps.outcome().is_ok());
        let failure = ployz_core::MachineFailure {
            machine_id: ployz_core::MachineId::parse("1".repeat(32)).unwrap(),
            error: crate::ui::rpc_error(
                RpcErrorCode::Unavailable,
                &std::io::Error::other("ployz-observe is down"),
            ),
        };
        gaps.extend(&[failure.clone(), failure], &[]);
        assert_eq!(gaps.failures.len(), 1);
        let partial = gaps.outcome().unwrap_err();
        assert_eq!(partial.printed_exit(), Some(crate::ui::PARTIAL_EXIT));
    }

    #[test]
    fn follow_resumes_each_container_after_its_own_last_printed_line() {
        let container = |c: char| ContainerId::parse(c.to_string().repeat(64)).unwrap();
        let machine = ployz_core::MachineName::parse("machine-1").unwrap();
        let mut printed = Printed::default();
        assert_eq!(printed.since(7), 7);
        for (id, ts) in [('a', 100), ('b', 50)] {
            printed.record(&HistoryRecord {
                machine: machine.clone(),
                container: std::sync::Arc::new(ployz_core::HistoryContainer {
                    container_id: container(id),
                    namespace: None,
                    service: None,
                    deployment: None,
                    replica: String::new(),
                    kind: HistoryContainerKind::Service,
                }),
                event: HistoryEvent::Line {
                    ts,
                    stream: HistoryStream::Stdout,
                    text: Vec::new(),
                },
            });
        }
        // Live starts where history did, so a container it missed loses nothing.
        assert_eq!(printed.since(7), 50);
        let live = |id: char, ts: i64| LogEntry {
            metadata: ployz_core::LogMetadata {
                origin: LogOrigin::Service {
                    service_id: ployz_core::ServiceId::parse("1".repeat(32)).unwrap(),
                    service_name: ployz_core::ServiceName::parse("web").unwrap(),
                    container_id: container(id),
                    hook: None,
                },
                machine_id: ployz_core::MachineId::parse("2".repeat(32)).unwrap(),
                machine_name: machine.clone(),
            },
            timestamp_unix_nanos: ts,
            body: LogBody::Stdout(Vec::new()),
        };
        assert!(!printed.covers(&live('b', 60)));
        assert!(printed.covers(&live('b', 50)));
        assert!(printed.covers(&live('a', 60)));
        assert!(!printed.covers(&live('a', 101)));
        // A container history printed nothing from shows every live line.
        assert!(!printed.covers(&live('c', 50)));
        assert!(!printed.covers(&live('c', 90)));
    }

    #[test]
    fn a_renamed_service_is_found_by_its_runtime_name() {
        let name = |text: &str| ployz_core::ServiceName::parse(text).unwrap();
        let scoped = Scoped {
            namespace: Namespace::parse("shop-production").unwrap(),
            services: [(name("api2"), name("api"))].into(),
        };
        let resolve = |selector: &str| {
            in_scope(ServiceSelector::parse(selector).unwrap(), Some(&scoped))
                .unwrap()
                .to_string()
        };
        assert_eq!(resolve("api2"), "shop-production/api");
        assert_eq!(resolve("web"), "shop-production/web");
        assert_eq!(resolve("other-ns/api2"), "other-ns/api2");
    }
}
