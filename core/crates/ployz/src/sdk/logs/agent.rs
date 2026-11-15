//! The lines an agent reads from a Machine's Log Store or its processes.
use futures_util::future::join_all;
use ployz_core::error_chain;
use ployz_core::{
    ContainerId, LiveLogTarget, LogBody, LogEntry, LogHistoryRequest, LogsOptions, MachineId,
    MachineLogService, MachineTarget, RpcError, RpcErrorCode, TailLogsRequest, op,
};
use ployz_store::tools::{LineCap, LogExcerpt, LogText, SealedValues, Withheld};
use std::collections::BTreeSet;
use std::time::Duration;

use super::stream::{container_env, frames, newest_lines};
use crate::connect::{Client, ConnectError};

/// How long reading one Machine's lines for an agent may take, env included.
const SERVICE_TAIL_READ: Duration = Duration::from_secs(10);

/// One Machine's lines for an agent.
#[derive(Debug)]
pub(crate) struct AgentTail {
    pub(crate) text: LogText,
    /// Each Container whose lines `text` leaves out because its env could not be read to
    /// scrub them, labelled as `ployz logs` prints it.
    pub(crate) withheld: Vec<String>,
}

impl From<LogText> for AgentTail {
    fn from(text: LogText) -> Self {
        Self {
            text,
            withheld: Vec::new(),
        }
    }
}

/// The newest `cap` lines `selector` picks from `machine`'s Log Store, across every
/// Container that wrote them, scrubbed of each Container's values for the `sealed` names
/// as its Machine reports them, or of every env value when `sealed` is None. The env is
/// read here and never leaves, and not at all when `sealed` names nothing. A Container
/// whose env cannot be read, as after a redeploy removed it, has its lines left out and
/// is named in [`AgentTail::withheld`]; the text is withheld when no Container's env can
/// be read. None when `selector` picks no lines there. The error says why the Machine
/// did not answer in time.
pub(crate) async fn service_tail(
    client: &Client,
    machine: MachineId,
    selector: LogHistoryRequest,
    cap: LineCap,
    sealed: Option<&BTreeSet<String>>,
) -> Result<Option<AgentTail>, RpcError> {
    let target = MachineTarget::from(&machine);
    let read = async {
        let service = selector.service.clone();
        let (newest, _) = newest_lines(client, &target, selector, LineCap::MAX).await?;
        if newest.is_empty() {
            return Ok(None);
        }
        if sealed.is_some_and(BTreeSet::is_empty) {
            let frames = frames(newest.into_iter().map(|(_, text)| text));
            return Ok(Some(AgentTail::from(LogText::Shown(LogExcerpt::scrub(
                frames,
                &SealedValues::none(),
                cap,
            )))));
        }
        let mut containers: Vec<ContainerId> = Vec::new();
        for (container, _) in &newest {
            if !containers.contains(container) {
                containers.push(*container);
            }
        }
        let envs = join_all(
            containers
                .iter()
                .map(|container| container_env(client, machine, *container)),
        )
        .await;
        let mut values = SealedValues::none();
        let mut readable = Vec::new();
        let mut withheld = Vec::new();
        for (container, env) in containers.iter().zip(envs) {
            let Some(env) = env else {
                let id = crate::ui::short_id(container.as_str());
                withheld.push(
                    service
                        .as_ref()
                        .map_or_else(|| id.to_owned(), |service| format!("{service}/{id}")),
                );
                continue;
            };
            values.extend(
                env.into_iter()
                    .filter(|(name, _)| sealed.is_none_or(|sealed| sealed.contains(name))),
            );
            readable.push(*container);
        }
        if readable.is_empty() {
            return Ok(Some(AgentTail {
                text: LogText::Withheld {
                    why: Withheld::NoSecretContext,
                },
                withheld,
            }));
        }
        let frames = frames(
            newest
                .into_iter()
                .filter(|(container, _)| readable.contains(container))
                .map(|(_, text)| text),
        );
        Ok(Some(AgentTail {
            text: LogText::Shown(LogExcerpt::scrub(frames, &values, cap)),
            withheld,
        }))
    };
    in_time("its Log Store", read).await
}

/// The newest `cap` lines `process` wrote on `machine`, from `since_nanos` on, with
/// secret-shaped values redacted. None when it wrote none in range.
pub(crate) async fn process_tail(
    client: &Client,
    machine: MachineId,
    process: MachineLogService,
    since_nanos: Option<i64>,
    cap: LineCap,
) -> Result<Option<LogText>, RpcError> {
    let target = MachineTarget::from(&machine);
    let request = op::TailLogs::into_request(TailLogsRequest {
        target: LiveLogTarget::Machine(process),
        options: LogsOptions {
            tail: i32::from(cap.get()),
            follow: false,
            since_nanos,
            until_unix_seconds: None,
        },
    })
    .encode()
    .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
    let read = async {
        let mut stream = client
            .open_stream::<op::TailLogs>(Some(&target), tokio_stream::once(request))
            .await
            .map_err(|error| RpcError::from(ConnectError::Rpc(error)))?;
        let mut frames = Vec::new();
        while let Some(payload) = stream
            .message()
            .await
            .map_err(|error| RpcError::from(ConnectError::from(error)))?
        {
            let entry = LogEntry::decode(&payload)
                .map_err(|error| error_chain::rpc_error(RpcErrorCode::InvalidArgument, &error))?;
            match entry.body {
                LogBody::Stdout(bytes) | LogBody::Stderr(bytes) => {
                    frames.push(String::from_utf8_lossy(&bytes).into_owned());
                }
                LogBody::Error(message) => return Err(RpcError::unavailable(message)),
                LogBody::Heartbeat => {}
            }
        }
        Ok((!frames.is_empty())
            .then(|| LogText::Shown(LogExcerpt::scrub(frames, &SealedValues::none(), cap))))
    };
    in_time(&format!("its {process} log"), read).await
}

/// `read`, or `unavailable` naming `what` when it takes longer than [`SERVICE_TAIL_READ`].
async fn in_time<T>(
    what: &str,
    read: impl Future<Output = Result<T, RpcError>>,
) -> Result<T, RpcError> {
    tokio::time::timeout(SERVICE_TAIL_READ, read)
        .await
        .map_err(|_| {
            RpcError::unavailable(format!(
                "{what} did not answer within {}s",
                SERVICE_TAIL_READ.as_secs()
            ))
        })?
}
