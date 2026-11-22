//! `logs`, `metrics` and `ps` for a credential Cloud refuses Server access: Cloud reads
//! the Servers instead, under the read permission each needs and the credential's
//! Environment ceiling.

use std::{collections::BTreeSet, time::Duration};

use chrono::Utc;
use ployz_agent_tools::{
    CloudQuery, LineCap, LogText, LogsQuery, MetricsQuery, MetricsSince, MetricsSubject, Timestamp,
    cloud::{ContainersAnswer, LogsAnswer, MetricsAnswer, ServersAnswer},
};
use ployz_core::{MachineName, QualifiedService, RpcErrorCode, ServiceName, ServiceSelector};
use reqwest::Method;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use super::{Error, operator::Scoped};
use crate::cloud;
use crate::operator::history::{self, StoredLine, StoredPage};
use crate::{
    cloud::account::{Credential, StoreCallError},
    cloud::login::LoginError,
    ui::{self, Table},
};

const NO_SCOPE: &str = "This token reads through Cloud, which reads one Environment at a time; add --project and --env";

fn ask<T: DeserializeOwned>(credential: &Credential, query: CloudQuery) -> Result<T, Error> {
    refusable(credential, query)?.map_err(Error::from)
}

/// Every Server of the credential's Organization, as Cloud last saw it.
///
/// # Errors
/// Returns Cloud's refusal, or a Cloud failure.
pub(super) fn servers(credential: &Credential) -> Result<ServersAnswer, Error> {
    ask(credential, CloudQuery::Servers { only: None })
}

/// [`ask`], keeping Cloud's refusal as Cloud sent it.
fn refusable<T: DeserializeOwned>(
    credential: &Credential,
    query: CloudQuery,
) -> Result<Result<T, StoreCallError>, Error> {
    Ok(super::runtime()?.block_on(cloud::account::approved_answer(
        credential,
        Method::POST,
        "agent/ask",
        &json!({ "ask": query }),
        None,
    )))
}

/// Cloud's refusal when it holds the credential to Environments other than `scoped`'s.
/// With no Service, `logs` reads nothing and asks only for this. Any other answer leaves
/// the logs empty, since Cloud also refuses a credential its own Environment before a
/// Deployment fixes the Namespace, naming that Environment among those it holds.
fn held_elsewhere(credential: &Credential, scoped: &Scoped) -> Result<(), Error> {
    let asked = CloudQuery::Containers {
        namespace: scoped.namespace.clone(),
        service: None,
    };
    match refusable::<ContainersAnswer>(credential, asked)? {
        Err(StoreCallError::Refused(refused))
            if refused.code.as_str() == ployz_core::FORBIDDEN_CODE
                && refused
                    .details
                    .get("environments")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|held| {
                        !held
                            .iter()
                            .any(|held| held.as_str() == Some(scoped.environment.as_str()))
                    }) =>
        {
            Err(StoreCallError::Refused(refused).into())
        }
        _ => Ok(()),
    }
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

pub(super) fn logs(credential: &Credential, asked: LogsAsked<'_>) -> Result<(), Error> {
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
    if services.is_empty() {
        held_elsewhere(credential, &scoped)?;
    }
    let unknown = |name: &ServiceName| {
        ServiceSelector::parse(name.as_str())
            .ok()
            .and_then(|selector| scoped.unknown(&selector))
    };
    let now = Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX);
    let since = asked
        .since_nanos
        .and_then(|since| u64::try_from(now.saturating_sub(since)).ok())
        .map(Duration::from_nanos);
    let mut read = Vec::new();
    for (name, runtime) in services {
        let answer: LogsAnswer = ask(
            credential,
            CloudQuery::Logs(LogsQuery {
                namespace: scoped.namespace.clone(),
                service: runtime,
                deployment: asked.deployment.clone(),
                server: None,
                lines: line_cap(asked.tail),
                since,
            }),
        )?;
        read.push((name, answer));
    }
    // A name the Environment lacks that no Server answered for, when every Server answered.
    let refusal = read
        .iter()
        .find(|(name, answer)| {
            answer.servers.is_empty()
                && answer.seen.unanswered.is_empty()
                && unknown(name).is_some()
        })
        .map(|(name, _)| scoped.no_service(name));
    // A Server that did not answer may hold the lines, so it outranks the refusal, as
    // on the direct path.
    if read
        .iter()
        .all(|(_, answer)| answer.servers.is_empty() && answer.seen.unanswered.is_empty())
        && let Some(refusal) = refusal
    {
        return Err(refusal);
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
                            "{service}@{}: logs withheld: {}.",
                            log.server,
                            why.reason()
                        ));
                    }
                }
            }
            for note in &answer.notes {
                ui::note(note);
            }
        }
    })?;
    ui::unanswered(
        read.iter()
            .flat_map(|(_, answer)| &answer.seen.unanswered)
            .collect::<BTreeSet<_>>(),
    )?;
    refusal.map_or(Ok(()), Err)
}

/// The newest `want` stored lines of the removed Services `removed`, from `since` up to
/// `until`, oldest first.
pub(super) fn stored_logs(
    credential: &Credential,
    scoped: &Scoped,
    removed: &[ServiceName],
    want: usize,
    since: Option<i64>,
    until: Option<i64>,
) -> Result<Stored, Error> {
    let runtime = super::runtime()?;
    let mut lines = Vec::new();
    let mut missing = Vec::new();
    for name in removed {
        let mut kept = Vec::new();
        let mut cursor = None::<String>;
        loop {
            let limit = want.saturating_sub(kept.len()).clamp(1, history::PAGE);
            let mut asked = json!({
                "environment": {
                    "project": scoped.project.to_string(),
                    "environment": scoped.environment.to_string(),
                },
                "service": name.to_string(),
                "limit": limit,
            });
            if let (Some(cursor), Some(asked)) = (&cursor, asked.as_object_mut()) {
                asked.insert("cursor".into(), json!(cursor));
            }
            let page: StoredPage = runtime
                .block_on(cloud::account::read_answer(credential, "logs/read", &asked))
                .map_err(Error::from)?;
            let oldest = page.lines.last().map(|line| line.timestamp);
            kept.extend(
                page.lines
                    .into_iter()
                    .filter(|line| until.is_none_or(|until| line.timestamp < until)),
            );
            let reached = since
                .zip(oldest)
                .is_some_and(|(since, oldest)| oldest < since);
            cursor = page.cursor;
            if reached || kept.len() >= want || cursor.is_none() {
                break;
            }
        }
        if kept.is_empty() {
            missing.push(name.clone());
        }
        lines.extend(kept);
    }
    Ok(Stored {
        lines: history::window(lines, since, until, want),
        missing,
    })
}

/// What Cloud kept of the removed Services asked for.
pub(super) struct Stored {
    pub(super) lines: Vec<StoredLine>,
    /// The names it holds no line for.
    pub(super) missing: Vec<ServiceName>,
}

/// `metrics` for a Service, by its runtime name, or a Server, by name.
pub(super) enum MetricsAsked {
    Service {
        service: QualifiedService,
        /// The refusal to give when nothing answers for a Service the Environment
        /// does not have now.
        unknown: Option<Error>,
    },
    Server(String),
}

/// A Cloud that answers no CLI route has no history to read, so the Servers answer instead.
fn offers_no_cli_access(error: &StoreCallError) -> bool {
    matches!(error, StoreCallError::Cloud(LoginError::Unsupported(_)))
}

/// What Cloud did with a `metrics` read.
pub(super) enum CloudMetrics {
    Shown,
    /// This Cloud keeps no metrics; its refusal, for when nothing else can answer.
    NotEnabled(Error),
}

pub(super) fn metrics(
    credential: &Credential,
    asked: MetricsAsked,
    since_seconds: i64,
) -> Result<CloudMetrics, Error> {
    let (subject, name, unknown) = match asked {
        MetricsAsked::Service { service, unknown } => (
            MetricsSubject::Service {
                namespace: service.namespace.clone(),
                service: service.name.clone(),
            },
            service.name.to_string(),
            unknown,
        ),
        MetricsAsked::Server(named) => {
            let only = MachineName::parse(named.as_str())
                .map_err(|_| Error::usage(format!("{named:?} is not a Server name")))?;
            let servers: ServersAnswer =
                match refusable(credential, CloudQuery::Servers { only: Some(only) })? {
                    Ok(answer) => answer,
                    Err(error) if offers_no_cli_access(&error) => {
                        return Ok(CloudMetrics::NotEnabled(error.into()));
                    }
                    Err(error) => return Err(error.into()),
                };
            let server = match servers.servers.as_slice() {
                [] => return Err(Error::usage(format!("No Server is named {named:?}"))),
                [server] => server,
                [_, _, ..] => {
                    return Err(Error::usage(format!(
                        "More than one Server is named {named:?}"
                    )));
                }
            };
            (
                MetricsSubject::Server {
                    server: server.reference(),
                },
                named,
                None,
            )
        }
    };
    let answer: MetricsAnswer = match refusable(
        credential,
        CloudQuery::Metrics(MetricsQuery {
            subject,
            since: MetricsSince::At {
                at: Timestamp(since_seconds),
            },
            deployment: None,
        }),
    )? {
        Ok(answer) => answer,
        Err(StoreCallError::Refused(refused)) if refused.code == RpcErrorCode::Conflict => {
            return Ok(CloudMetrics::NotEnabled(
                StoreCallError::Refused(refused).into(),
            ));
        }
        Err(error) if offers_no_cli_access(&error) => {
            return Ok(CloudMetrics::NotEnabled(error.into()));
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(refusal) = unknown
        && answer.metrics.is_empty()
        && answer.seen.unanswered.is_empty()
    {
        return Err(refusal);
    }
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
    })?;
    ui::unanswered(&answer.seen.unanswered).map(|()| CloudMetrics::Shown)
}

pub(super) fn processes(
    credential: &Credential,
    scoped: Option<Scoped>,
    sort: super::service::ProcessOrder,
) -> Result<(), Error> {
    use super::service::ProcessOrder;
    let scoped = scoped.ok_or_else(|| Error::usage(NO_SCOPE))?;
    let mut answer: ContainersAnswer = ask(
        credential,
        CloudQuery::Containers {
            namespace: scoped.namespace.clone(),
            service: None,
        },
    )?;
    answer.containers.sort_by_cached_key(|container| {
        let service = scoped.service_name(&container.service).to_string();
        let server = container.server.to_string();
        match sort {
            ProcessOrder::Machine => (server, service, String::new()),
            ProcessOrder::Health => (word(&container.health), service, server),
            ProcessOrder::Service => (service, server, String::new()),
        }
    });
    let mut table = Table::new(
        ["SERVICE", "SERVER", "STATE", "HEALTH", "RESTARTS", "IMAGE"],
        "No containers run in this Environment.",
    );
    for container in &answer.containers {
        table.row([
            scoped.service_name(&container.service).to_string(),
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
    ui::finish(&json!({ "containers": answer }), || ui::rows(&table))?;
    ui::unanswered(&answer.seen.unanswered)
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
