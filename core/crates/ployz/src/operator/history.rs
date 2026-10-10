//! `ployz logs` history: each asked Server's Log Store, merged by timestamp.
//!
//! A Server that fails stops contributing and is returned as a failure; the
//! others keep printing.

use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{
    ContainerId, HistoryContainer, HistoryContainerKind, HistoryGapReason, HistoryRow,
    HistoryStream, LOG_HISTORY_PAGE_LIMIT, LogDirection, LogHistoryRequest, MachineFailure,
    MachineId, MachineName, MachineTarget, RpcError, RpcErrorCode, op,
};
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError};

/// The store sends a heartbeat each second while it reads.
const ROW_DEADLINE: Duration = Duration::from_secs(10);

/// Which stored containers one read asks a Server for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HistorySelector {
    pub namespace: Option<String>,
    pub service: Option<String>,
    pub deployment: Option<String>,
    pub container_id: Option<ContainerId>,
}

/// Nanosecond bounds: `since` inclusive, `until` exclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HistoryWindow {
    /// The last `lines` rows of the range, across every Server.
    Last {
        lines: usize,
        since: Option<i64>,
        until: Option<i64>,
    },
    /// Every row of the range, read a page at a time.
    All {
        since: Option<i64>,
        until: Option<i64>,
    },
}

#[derive(Clone, Debug)]
pub struct HistoryRecord {
    pub machine: MachineName,
    pub container: Arc<HistoryContainer>,
    pub event: HistoryEvent,
}

#[derive(Clone, Debug)]
pub enum HistoryEvent {
    Line {
        ts: i64,
        stream: HistoryStream,
        text: Vec<u8>,
    },
    Gap {
        from: i64,
        to: i64,
        reason: HistoryGapReason,
    },
    Exit {
        ts: i64,
        exit_code: Option<i64>,
        oom_killed: bool,
    },
}

impl HistoryRecord {
    #[must_use]
    pub fn ts(&self) -> i64 {
        match self.event {
            HistoryEvent::Line { ts, .. } | HistoryEvent::Exit { ts, .. } => ts,
            HistoryEvent::Gap { from, .. } => from,
        }
    }
}

/// One Server and one selector, read in ascending time.
struct Source {
    machine_id: MachineId,
    machine: MachineName,
    selector: HistorySelector,
    containers: HashMap<ContainerId, Arc<HistoryContainer>>,
    buffered: VecDeque<HistoryRecord>,
    cursor: Option<String>,
    exhausted: bool,
}

/// Reads `selectors` on every Server in `machines` and hands each record to
/// `emit` in timestamp order. Returns the Servers that failed.
///
/// # Errors
///
/// Returns the first error `emit` returns.
pub async fn read_history<E>(
    client: &Client,
    machines: &[(MachineId, MachineName)],
    selectors: &[HistorySelector],
    window: HistoryWindow,
    cancellation: &CancellationToken,
    mut emit: impl FnMut(HistoryRecord) -> Result<(), E>,
) -> Result<Vec<MachineFailure<RpcError>>, E> {
    let mut sources = machines
        .iter()
        .flat_map(|(machine_id, machine)| {
            selectors.iter().map(|selector| Source {
                machine_id: *machine_id,
                machine: machine.clone(),
                selector: selector.clone(),
                containers: HashMap::new(),
                buffered: VecDeque::new(),
                cursor: None,
                exhausted: false,
            })
        })
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    match window {
        HistoryWindow::Last {
            lines,
            since,
            until,
        } => {
            let reads = sources
                .iter_mut()
                .map(|source| read_last(client, source, lines, since, until));
            let results = tokio::select! {
                () = cancellation.cancelled() => return Ok(failures),
                results = join_all(reads) => results,
            };
            let mut records = Vec::new();
            for (source, result) in sources.iter_mut().zip(results) {
                match result {
                    Ok(()) => records.extend(source.buffered.drain(..)),
                    Err(error) => fail(&mut failures, source.machine_id, error),
                }
            }
            records.sort_by_key(HistoryRecord::ts);
            let skip = records.len().saturating_sub(lines);
            for record in records.into_iter().skip(skip) {
                emit(record)?;
            }
        }
        HistoryWindow::All { since, until } => loop {
            let refills = sources
                .iter_mut()
                .filter(|source| source.buffered.is_empty() && !source.exhausted)
                .map(|source| async move {
                    let result = read_forward(client, source, since, until).await;
                    (source.machine_id, result)
                });
            let results = tokio::select! {
                () = cancellation.cancelled() => return Ok(failures),
                results = join_all(refills) => results,
            };
            for (machine_id, result) in results {
                if let Err(error) = result {
                    fail(&mut failures, machine_id, error);
                }
            }
            sources.retain(|source| {
                !source.buffered.is_empty()
                    || !source.exhausted
                        && !failures.iter().any(|f| f.machine_id == source.machine_id)
            });
            // Emit while every open source has a row to compare against.
            while sources
                .iter()
                .all(|source| !source.buffered.is_empty() || source.exhausted)
            {
                let Some(next) = sources
                    .iter_mut()
                    .filter(|source| !source.buffered.is_empty())
                    .min_by_key(|source| source.buffered.front().map(HistoryRecord::ts))
                else {
                    return Ok(failures);
                };
                if let Some(record) = next.buffered.pop_front() {
                    emit(record)?;
                }
            }
        },
    }
    Ok(failures)
}

fn fail(failures: &mut Vec<MachineFailure<RpcError>>, machine_id: MachineId, error: RpcError) {
    if !failures
        .iter()
        .any(|failure| failure.machine_id == machine_id)
    {
        failures.push(MachineFailure { machine_id, error });
    }
}

async fn read_last(
    client: &Client,
    source: &mut Source,
    lines: usize,
    since: Option<i64>,
    until: Option<i64>,
) -> Result<(), RpcError> {
    loop {
        let remaining = lines.saturating_sub(source.buffered.len());
        if remaining == 0 {
            break;
        }
        let limit = u16::try_from(remaining)
            .unwrap_or(u16::MAX)
            .min(LOG_HISTORY_PAGE_LIMIT);
        let next = read_page(client, source, LogDirection::Backward, limit, since, until).await?;
        if next.is_none() {
            break;
        }
        source.cursor = next;
    }
    source.buffered.make_contiguous().reverse();
    Ok(())
}

async fn read_forward(
    client: &Client,
    source: &mut Source,
    since: Option<i64>,
    until: Option<i64>,
) -> Result<(), RpcError> {
    let next = read_page(
        client,
        source,
        LogDirection::Forward,
        LOG_HISTORY_PAGE_LIMIT,
        since,
        until,
    )
    .await;
    match next {
        Ok(Some(cursor)) => source.cursor = Some(cursor),
        Ok(None) => source.exhausted = true,
        Err(error) => {
            source.exhausted = true;
            return Err(error);
        }
    }
    Ok(())
}

/// Appends one page to the source's buffer; returns the next page's cursor.
async fn read_page(
    client: &Client,
    source: &mut Source,
    direction: LogDirection,
    limit: u16,
    since: Option<i64>,
    until: Option<i64>,
) -> Result<Option<String>, RpcError> {
    let request = LogHistoryRequest {
        namespace: source.selector.namespace.clone(),
        service: source.selector.service.clone(),
        deployment: source.selector.deployment.clone(),
        container_id: source.selector.container_id,
        since_nanos: since,
        until_nanos: until,
        direction,
        limit,
        cursor: source.cursor.clone(),
    };
    let payload = op::LogHistory::into_request(request)
        .encode()
        .map_err(|error| RpcError::from(ConnectError::Codec(error)))?;
    let mut stream = client
        .log_history_stream(&MachineTarget::from(&source.machine_id), payload)
        .await
        .map_err(|error| RpcError::from(ConnectError::Rpc(error)))?;
    loop {
        let message = tokio::time::timeout(ROW_DEADLINE, stream.message())
            .await
            .map_err(|_| unavailable("the Server's Log Store stopped answering"))?
            .map_err(|status| RpcError::from(ConnectError::from(status)))?
            .ok_or_else(|| unavailable("the Server's Log Store ended a page early"))?;
        let row = HistoryRow::decode(&message)
            .map_err(|error| crate::ui::rpc_error(RpcErrorCode::Internal, &error))?;
        let (container_id, event) = match row {
            HistoryRow::Container(container) => {
                source
                    .containers
                    .insert(container.container_id, Arc::new(container));
                continue;
            }
            HistoryRow::Heartbeat => continue,
            HistoryRow::End { next } => return Ok(next),
            HistoryRow::Error(message) => return Err(unavailable(&message)),
            HistoryRow::Line {
                container_id,
                ts,
                stream,
                text,
            } => (container_id, HistoryEvent::Line { ts, stream, text }),
            HistoryRow::Gap {
                container_id,
                from,
                to,
                reason,
            } => (container_id, HistoryEvent::Gap { from, to, reason }),
            HistoryRow::Exit {
                container_id,
                ts,
                exit_code,
                oom_killed,
            } => (
                container_id,
                HistoryEvent::Exit {
                    ts,
                    exit_code,
                    oom_killed,
                },
            ),
        };
        let container = Arc::clone(
            source
                .containers
                .entry(container_id)
                .or_insert_with(|| Arc::new(undescribed(container_id))),
        );
        source.buffered.push_back(HistoryRecord {
            machine: source.machine.clone(),
            container,
            event,
        });
    }
}

fn undescribed(container_id: ContainerId) -> HistoryContainer {
    HistoryContainer {
        container_id,
        namespace: None,
        service: None,
        deployment: None,
        replica: String::new(),
        kind: HistoryContainerKind::Service,
    }
}

fn unavailable(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: message.to_owned(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}
