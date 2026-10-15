//! `ployz metrics`: a Service's or a Server's metric history.

use chrono::Utc;
use clap::{ArgGroup, ArgMatches, Command};
use ployz_core::{MachineTarget, QualifiedService, ServiceSelector, metrics::MetricRange};

use super::{
    Error, leaf_matches,
    operator::{in_scope, scope},
    store::scoped,
    with_client,
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
    let since_seconds = crate::operator::parse_log_time(&since, now.timestamp())?
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
    let asked = match leaf.get_one::<String>("server") {
        Some(server) => Asked::Server(MachineTarget::parse(server.as_str())?),
        None => {
            let named = leaf
                .get_one::<String>("service")
                .expect("the subject group requires a Service or a Server");
            Asked::Service {
                service: qualified(root, named)?,
                named: named.clone(),
            }
        }
    };
    with_client(root, |client| {
        Box::pin(async move {
            let machines = client.machines().await?;
            let (subject, name) = match asked {
                Asked::Service { named, service } => (MetricSubject::Service { service }, named),
                Asked::Server(target) => {
                    let machine = &crate::cluster::visible_machine(&target, &machines)?.machine;
                    (
                        MetricSubject::Machine {
                            machine_id: machine.id,
                        },
                        machine.name.to_string(),
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
            ui::finish_fanout("metrics", &history, &gaps, || {
                ui::stream(&shown.heading);
                ui::rows(&shown.table);
                for note in &shown.notes {
                    ui::note(note);
                }
            })
        })
    })
}

/// What the user selected, by name.
enum Asked {
    Service {
        named: String,
        service: QualifiedService,
    },
    Server(MachineTarget),
}

/// The Service `named` selects in the addressed Environment.
fn qualified(root: &ArgMatches, named: &str) -> Result<QualifiedService, Error> {
    let selector = in_scope(ServiceSelector::parse(named)?, scope(root)?.as_ref())?;
    QualifiedService::parse(selector.as_str()).map_err(|_| {
        Error::usage(format!(
            "No Environment is linked, so {named:?} names no Service; add --project and --env"
        ))
    })
}
