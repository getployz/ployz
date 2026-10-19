//! `logs`, `metrics` and `ps` for a credential Cloud refuses Server access: Cloud reads
//! the Servers instead, under the read permission each needs and the credential's
//! Environment ceiling.

use std::time::Duration;

use chrono::Utc;
use ployz_core::{MachineName, QualifiedService, ServiceName};
use ployz_store::{
    EnvironmentRef, ServiceQuery,
    tools::{
        CloudQuery, LineCap, LogText, LogsQuery, MetricsQuery, MetricsSince, MetricsSubject,
        Timestamp,
        cloud::{ContainersAnswer, LogsAnswer, MetricsAnswer, ServersAnswer},
        sealed_env,
    },
};
use reqwest::Method;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{Error, operator::Scoped};
use crate::{
    cloud_account::{self, Credential},
    ui::{self, Table},
};

const NO_SCOPE: &str = "This token reads through Cloud, which reads one Environment at a time; add --project and --env";

fn ask<T: DeserializeOwned>(credential: &Credential, query: CloudQuery) -> Result<T, Error> {
    super::runtime()?
        .block_on(cloud_account::approved_answer(
            credential,
            Method::POST,
            "agent/ask",
            &json!({ "ask": query }),
            None,
        ))
        .map_err(Error::from)
}

/// What `logs` asked for, as Cloud can answer it.
pub(super) struct LogsAsked<'a> {
    pub(super) scoped: Option<Scoped>,
    pub(super) named: &'a [String],
    pub(super) deployment: Option<ployz_store::DeploymentId>,
    pub(super) tail: i32,
    pub(super) since_nanos: Option<i64>,
    /// `--follow`, `--until`, `--machine` or a container selector: Cloud serves none.
    pub(super) unservable: bool,
}

/// The newest `tail` lines, held to what one Cloud read returns.
fn line_cap(tail: i32) -> LineCap {
    let lines = if tail < 0 {
        LineCap::MAX
    } else {
        u16::try_from(tail.clamp(1, i32::from(LineCap::MAX))).unwrap_or(LineCap::MAX)
    };
    LineCap::new(lines).unwrap_or_default()
}

pub(super) fn logs(
    root: &clap::ArgMatches,
    credential: &Credential,
    asked: LogsAsked<'_>,
) -> Result<(), Error> {
    if asked.unservable {
        return Err(Error::usage(
            "This token reads logs through Cloud, which serves the newest lines only; drop --follow, --until, --machine and container selectors",
        ));
    }
    let scoped = asked.scoped.ok_or_else(|| Error::usage(NO_SCOPE))?;
    let services = if asked.named.is_empty() {
        scoped.services.clone().into_iter().collect::<Vec<_>>()
    } else {
        asked
            .named
            .iter()
            .map(|named| {
                let name = ServiceName::parse(named.as_str())
                    .map_err(|_| Error::usage(format!("{named:?} is not a Service name")))?;
                let runtime = scoped.services.get(&name).cloned().unwrap_or(name.clone());
                Ok((name, runtime))
            })
            .collect::<Result<Vec<_>, Error>>()?
    };
    let environment = EnvironmentRef {
        project: Some(scoped.environment.project.clone()),
        environment: Some(scoped.environment.name.clone()),
    };
    let store = super::store::store(root)?;
    let now = Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX);
    let since = asked
        .since_nanos
        .and_then(|since| u64::try_from(now.saturating_sub(since)).ok())
        .map(Duration::from_nanos);
    let mut read = Vec::new();
    for (name, runtime) in services {
        let sealed = store
            .try_read(&ServiceQuery {
                environment: environment.clone(),
                service: name.clone(),
            })
            .ok()
            .map(|view| sealed_env(&view.values));
        let answer: LogsAnswer = ask(
            credential,
            CloudQuery::Logs(LogsQuery {
                environment: scoped.environment.id.clone(),
                namespace: scoped.namespace.clone(),
                service: runtime,
                deployment: asked.deployment.clone(),
                server: None,
                lines: line_cap(asked.tail),
                since,
                sealed,
            }),
        )?;
        read.push((name, answer));
    }
    let shown = read
        .iter()
        .map(|(service, answer)| json!({ "service": service, "logs": answer }))
        .collect::<Vec<_>>();
    ui::finish(&json!({ "logs": shown }), || {
        for (service, answer) in &read {
            for log in &answer.servers {
                match &log.text {
                    LogText::Shown(excerpt) => {
                        for line in excerpt.lines() {
                            ui::stream(format_args!("{service}@{}  {line}", log.server));
                        }
                    }
                    LogText::Withheld { why } => {
                        ui::note(format_args!(
                            "{service}@{}: logs withheld ({})",
                            log.server,
                            word(why)
                        ));
                    }
                }
            }
            for note in &answer.notes {
                ui::note(note);
            }
        }
    })
}

/// `metrics` for a Service, by its runtime name, or a Server, by name.
pub(super) enum MetricsAsked {
    Service(QualifiedService),
    Server(String),
}

pub(super) fn metrics(
    credential: &Credential,
    asked: MetricsAsked,
    since_seconds: i64,
) -> Result<(), Error> {
    let (subject, name) = match asked {
        MetricsAsked::Service(service) => (
            MetricsSubject::Service {
                namespace: service.namespace.clone(),
                service: service.name.clone(),
            },
            service.name.to_string(),
        ),
        MetricsAsked::Server(named) => {
            let only = MachineName::parse(named.as_str())
                .map_err(|_| Error::usage(format!("{named:?} is not a Server name")))?;
            let servers: ServersAnswer = ask(credential, CloudQuery::Servers { only: Some(only) })?;
            let server = servers
                .servers
                .first()
                .ok_or_else(|| Error::usage(format!("No Server is named {named:?}")))?;
            (
                MetricsSubject::Server {
                    server: server.reference(),
                },
                named,
            )
        }
    };
    let answer: MetricsAnswer = ask(
        credential,
        CloudQuery::Metrics(MetricsQuery {
            subject,
            since: MetricsSince::At {
                at: Timestamp(since_seconds),
            },
            deployment: None,
        }),
    )?;
    let mut table = Table::new(
        ["METRIC", "NOW", "AVG", "PEAK"],
        "No Server reported a reading in this range.",
    );
    let dash = |value: &Option<String>| value.clone().unwrap_or_else(|| "-".to_owned());
    for row in &answer.metrics {
        table.row([
            row.metric.clone(),
            dash(&row.now),
            dash(&row.avg),
            dash(&row.peak),
        ]);
    }
    ui::finish(&json!({ "metrics": answer }), || {
        ui::stream(&name);
        ui::rows(&table);
        ui::note(format_args!(
            "{} OOM kills, {} exits, {} restarts",
            answer.oom_kills, answer.exits, answer.restarts
        ));
        for note in &answer.notes {
            ui::note(note);
        }
    })
}

pub(super) fn processes(
    credential: &Credential,
    scoped: Option<Scoped>,
    sort: &str,
) -> Result<(), Error> {
    let scoped = scoped.ok_or_else(|| Error::usage(NO_SCOPE))?;
    let mut answer: ContainersAnswer = ask(
        credential,
        CloudQuery::Containers {
            namespace: scoped.namespace,
            service: None,
        },
    )?;
    answer.containers.sort_by_cached_key(|container| {
        let service = container.service.to_string();
        let server = container.server.to_string();
        match sort {
            "machine" => (server, service, String::new()),
            "health" => (word(&container.health), service, server),
            _ => (service, server, String::new()),
        }
    });
    let mut table = Table::new(
        ["SERVICE", "SERVER", "STATE", "HEALTH", "RESTARTS", "IMAGE"],
        "No containers run in this Environment.",
    );
    for container in &answer.containers {
        table.row([
            container.service.to_string(),
            container.server.to_string(),
            word(&container.state),
            container
                .health
                .as_ref()
                .map_or_else(|| "-".to_owned(), word),
            container
                .restarts
                .map_or_else(|| "-".to_owned(), |restarts| restarts.to_string()),
            container.image.clone(),
        ]);
    }
    ui::finish(&json!({ "containers": answer }), || {
        ui::rows(&table);
        if !answer.seen.unanswered.is_empty() {
            let unanswered = answer
                .seen
                .unanswered
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            ui::note(format_args!("Not answered: {}", unanswered.join(", ")));
        }
    })
}

/// A unit enum's wire word, such as `running`.
fn word<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(word)) => word,
        _ => "-".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_is_held_to_one_cloud_read() {
        let cap = |tail| line_cap(tail).get();
        assert_eq!(cap(-1), LineCap::MAX);
        assert_eq!(cap(0), 1);
        assert_eq!(cap(50), 50);
        assert_eq!(cap(5_000), LineCap::MAX);
    }
}
