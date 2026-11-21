//! `ployz metrics`: what a Service or a Server uses now, or its history through Cloud.

use chrono::Utc;
use clap::{ArgGroup, ArgMatches, Command};
use ployz_core::{MachineTarget, QualifiedService, ServiceSelector};

use super::{
    Error,
    cloud_read::{self, CloudMetrics, MetricsAsked},
    leaf_matches,
    operator::{in_scope, scope},
    route,
    store::scoped,
    with_reader,
};
use crate::{
    cli::{
        base,
        metrics::{HISTORY_NOTE, NOT_ENABLED_NOTE, report},
        positional, value,
    },
    operator::input::TIME_FORMATS,
    sdk::{MetricSubject, metrics_live},
    ui::{self, Gaps},
};

pub(super) fn command(command: Command) -> Command {
    scoped(base(
        command,
        "Show CPU, memory, network and disk for a Service or a Server",
    ))
    .arg(positional("service", false).help("Service name"))
    .arg(
        value("server", None)
            .value_name("NAME")
            .help("A Server's own totals instead of a Service"),
    )
    .arg(value("since", None).default_value("1h").help(format!(
        "Range start, for history in Ployz Cloud: {TIME_FORMATS}"
    )))
    .group(
        ArgGroup::new("subject")
            .args(["service", "server"])
            .required(true),
    )
}

/// Print the subject's history from Cloud: its newest values and the range's average and
/// peak. Without a Cloud connection, or on a Cloud that keeps no metrics, print what it
/// uses now, read from the Servers.
pub fn metrics(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let since = leaf
        .get_one::<String>("since")
        .expect("since has a default")
        .clone();
    let now = Utc::now();
    let since_seconds = crate::operator::input::parse_log_time("since", &since, now.timestamp())?
        .ok_or_else(|| Error::usage("--since needs a time, such as 1h"))?;
    if since_seconds >= now.timestamp() {
        return Err(Error::usage(
            "--since is in the future; use 1h or an earlier time",
        ));
    }
    let (asked, fallback) = match leaf.get_one::<String>("server") {
        Some(server) => (
            Asked::Server(MachineTarget::parse(server.as_str())?),
            MetricsAsked::Server(server.clone()),
        ),
        None => {
            let named = leaf
                .get_one::<String>("service")
                .expect("the subject group requires a Service or a Server");
            let (service, refusal) = qualified(root, named)?;
            (
                Asked::Service {
                    service: service.clone(),
                    named: named.clone(),
                    unknown: refusal(),
                },
                MetricsAsked::Service {
                    service,
                    unknown: refusal(),
                },
            )
        }
    };
    let unread = match route::cloud_runs(&super::runtime()?, leaf)? {
        Some(credential) => match cloud_read::metrics(&credential, fallback, since_seconds)? {
            CloudMetrics::Shown => return Ok(()),
            CloudMetrics::NotEnabled(refusal) => Unread::NotEnabled(refusal),
        },
        None => Unread::Ask(fallback),
    };
    let note = match unread {
        Unread::Ask(_) => HISTORY_NOTE,
        Unread::NotEnabled(_) => NOT_ENABLED_NOTE,
    };
    let credential = with_reader(root, |client| {
        Box::pin(async move {
            let machines = client.machines().await?;
            let (subject, name, unknown) = match asked {
                Asked::Service {
                    named,
                    service,
                    unknown,
                } => (MetricSubject::Service { service }, named, unknown),
                Asked::Server(target) => {
                    let machine = &crate::cluster::visible_machine(&target, &machines)?.machine;
                    (
                        MetricSubject::Machine {
                            machine_id: machine.id,
                        },
                        machine.name.to_string(),
                        None,
                    )
                }
            };
            let live = metrics_live(client, subject, &machines).await;
            let mut gaps = Gaps::default().named(
                machines
                    .iter()
                    .map(|observed| (observed.machine.id, &observed.machine.name)),
            );
            gaps.extend(&live.unanswered, &[]);
            let table = report(&live, &name);
            if let Some(refusal) = unknown
                && table.is_empty()
                && gaps.outcome().is_ok()
            {
                return Err(refusal);
            }
            ui::finish_fanout("metrics", &live, &gaps, || {
                ui::stream(&name);
                ui::rows(&table);
                ui::note(note);
            })
        })
    })?;
    let Some(credential) = credential else {
        return Ok(());
    };
    match unread {
        Unread::Ask(asked) => match cloud_read::metrics(&credential, asked, since_seconds)? {
            CloudMetrics::Shown => Ok(()),
            CloudMetrics::NotEnabled(refusal) => Err(refusal),
        },
        Unread::NotEnabled(refusal) => Err(refusal),
    }
}

/// Why the history was not shown before the Servers were read.
enum Unread {
    /// Cloud was not asked yet.
    Ask(MetricsAsked),
    NotEnabled(Error),
}

/// What the user selected, by name.
enum Asked {
    Service {
        named: String,
        service: QualifiedService,
        /// The refusal to give when nothing answers for a Service the Environment
        /// does not have now.
        unknown: Option<Error>,
    },
    Server(MachineTarget),
}

/// The Service `named` selects in the addressed Environment, and what gives the
/// refusal for it if the Environment does not have it now.
fn qualified(
    root: &ArgMatches,
    named: &str,
) -> Result<(QualifiedService, impl Fn() -> Option<Error>), Error> {
    let scoped = scope(root)?;
    let selector = ServiceSelector::parse(named)?;
    let resolved = in_scope(selector.clone(), scoped.as_ref())?;
    let unknown = move || {
        scoped.as_ref().and_then(|scoped| {
            scoped
                .unknown(&selector)
                .map(|name| scoped.no_service(&name))
        })
    };
    let selector = resolved;
    let service = QualifiedService::parse(selector.as_str()).map_err(|_| {
        Error::usage(format!(
            "No Environment is linked, so {named:?} names no Service; add --project and --env"
        ))
    })?;
    Ok((service, unknown))
}
