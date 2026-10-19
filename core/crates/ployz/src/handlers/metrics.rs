//! `ployz metrics`: a Service's or a Server's metric history.

use chrono::Utc;
use clap::{ArgGroup, ArgMatches, Command};
use ployz_core::{MachineTarget, QualifiedService, ServiceSelector, metrics::MetricRange};

use super::{
    Error,
    cloud_read::{self, MetricsAsked},
    leaf_matches,
    operator::{in_scope, scope},
    store::scoped,
    with_reader,
};
use crate::{
    cli::{
        base,
        metrics::{Window, report},
        positional, value,
    },
    sdk::{MetricSubject, metric_history},
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
    .arg(
        value("since", None)
            .default_value("1h")
            .help("Range start: 10m, 2h, 7d or an RFC 3339 timestamp"),
    )
    .group(
        ArgGroup::new("subject")
            .args(["service", "server"])
            .required(true),
    )
}

/// Print the subject's newest values and the range's average and peak.
pub fn metrics(root: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(root);
    let since = leaf
        .get_one::<String>("since")
        .expect("since has a default")
        .clone();
    let now = Utc::now();
    let since_seconds = crate::operator::parse_log_time("since", &since, now.timestamp())?
        .ok_or_else(|| Error::usage("--since needs a time, such as 1h"))?;
    let until_nanos = now.timestamp_nanos_opt().unwrap_or(i64::MAX);
    let range = MetricRange {
        since_nanos: since_seconds.saturating_mul(1_000_000_000),
        until_nanos,
    };
    if range.since_nanos >= range.until_nanos {
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
            let (service, unknown) = qualified(root, named)?;
            (
                Asked::Service {
                    service: service.clone(),
                    named: named.clone(),
                    unknown,
                },
                MetricsAsked::Service(service),
            )
        }
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
            let history = metric_history(client, subject, range, None).await?;
            let mut gaps = Gaps::default().named(
                machines
                    .iter()
                    .map(|observed| (observed.machine.id, &observed.machine.name)),
            );
            gaps.extend(&history.unanswered, &[]);
            let shown = report(
                &history,
                &name,
                Window {
                    since: &since,
                    until_nanos,
                },
            );
            if let Some(refusal) = unknown
                && shown.table.is_empty()
                && gaps.outcome().is_ok()
            {
                return Err(refusal);
            }
            ui::finish_fanout("metrics", &history, &gaps, || {
                ui::stream(&shown.heading);
                ui::rows(&shown.table);
                for note in &shown.notes {
                    ui::note(note);
                }
            })
        })
    })?;
    credential.map_or(Ok(()), |credential| {
        cloud_read::metrics(&credential, fallback, since_seconds)
    })
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

/// The Service `named` selects in the addressed Environment, and the refusal for
/// it if the Environment does not have it now.
fn qualified(root: &ArgMatches, named: &str) -> Result<(QualifiedService, Option<Error>), Error> {
    let scoped = scope(root)?;
    let selector = ServiceSelector::parse(named)?;
    let unknown = scoped.as_ref().and_then(|scoped| {
        scoped
            .unknown(&selector)
            .map(|name| scoped.no_service(&name))
    });
    let selector = in_scope(selector, scoped.as_ref())?;
    let service = QualifiedService::parse(selector.as_str()).map_err(|_| {
        Error::usage(format!(
            "No Environment is linked, so {named:?} names no Service; add --project and --env"
        ))
    })?;
    Ok((service, unknown))
}
