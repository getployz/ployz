//! Coarse operation listing and watching.

use ployz_core::corrosion::{CorrosionDeployOutcome, CorrosionDeployState, OperationDocument};
use ployz_core::{
    LensCollection, LensSnapshot, OperationLookupReply, OperationWatchEvent, OperationWatchRefusal,
    operation_watch_route,
};

use crate::commands::{OpsCommand, OpsListCommand, OpsWatchCommand};
use crate::mesh::http::{DEFAULT_MESH_SSE_IDLE_TIMEOUT, MAX_MESH_SSE_FRAME_BYTES, SseReply};
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: OpsCommand) -> Result<String, OpsExecutionError> {
    match command {
        OpsCommand::List(command) => list(command).await,
        OpsCommand::Watch(command) => {
            let stdout = std::io::stdout();
            watch_to(&command, &mut stdout.lock()).await?;
            Ok(String::new())
        }
    }
}

async fn list(command: OpsListCommand) -> Result<String, OpsExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let snapshot = remote.lens(LensCollection::Operations).await?;
    let LensSnapshot::Operations { mut rows } = snapshot else {
        return Err(OpsExecutionError::WrongLens);
    };
    rows.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));
    let mut output = String::from("ID\tKIND\tSTATE\tCONTROLLER\n");
    for row in rows {
        output.push_str(row.id.as_str());
        output.push_str("\tdeploy\t");
        output.push_str(operation_state(&row.document));
        output.push('\t');
        output.push_str(row.document.machine_id.as_str());
        output.push('\n');
    }
    Ok(output)
}

pub async fn watch_to(
    command: &OpsWatchCommand,
    output: &mut impl std::io::Write,
) -> Result<(), OpsExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_sse_with_refusal::<(), OperationWatchEvent, OperationWatchRefusal>(
            hyper::Method::GET,
            &operation_watch_route(&command.operation_id),
            None,
            DEFAULT_MESH_SSE_IDLE_TIMEOUT,
            MAX_MESH_SSE_FRAME_BYTES,
        )
        .await?;
    let mut stream = match reply {
        SseReply::Stream(stream) => stream,
        SseReply::Refused(OperationWatchRefusal::NotFound { operation_id }) => {
            return Err(OpsExecutionError::NotFound {
                operation_id: operation_id.to_string(),
            });
        }
    };
    let mut last = None;
    loop {
        let Some(envelope) = stream
            .next_event()
            .await
            .map_err(OperatorRemoteError::from)?
        else {
            return Err(OpsExecutionError::StreamEnded);
        };
        let expected = envelope.data.event_name();
        if envelope.event.as_deref() != Some(expected) {
            return Err(OpsExecutionError::UnexpectedEventName {
                expected,
                found: envelope.event,
            });
        }
        let (operation, terminal) = match envelope.data {
            OperationWatchEvent::State { operation } => (operation, false),
            OperationWatchEvent::Terminal { operation } => (operation, true),
        };
        let rendered = render_operation(&operation);
        if last.as_deref() != Some(rendered.as_str()) {
            writeln!(output, "{rendered}").map_err(OpsExecutionError::Output)?;
            last = Some(rendered);
        }
        if terminal {
            return if operation_terminal_succeeded(&operation.operation) {
                Ok(())
            } else {
                Err(OpsExecutionError::OperationFailed {
                    state: operation_state(&operation.operation),
                })
            };
        }
    }
}

fn render_operation(operation: &OperationLookupReply) -> String {
    format!(
        "{} deploy {}",
        operation.operation_id,
        operation_state(&operation.operation)
    )
}

fn operation_state(operation: &OperationDocument) -> &'static str {
    deploy_state(operation.deploy_state())
}

const fn deploy_state(state: &CorrosionDeployState) -> &'static str {
    match state {
        CorrosionDeployState::Created => "created",
        CorrosionDeployState::Terminal { outcome, .. } => match outcome {
            CorrosionDeployOutcome::Completed { warnings, .. } if warnings.is_empty() => {
                "completed"
            }
            CorrosionDeployOutcome::Completed { .. } => "completed-with-warnings",
            CorrosionDeployOutcome::Failed { .. } => "failed",
            CorrosionDeployOutcome::Interrupted => "interrupted-resubmit",
        },
    }
}

fn operation_terminal_succeeded(operation: &OperationDocument) -> bool {
    matches!(
        operation.deploy_state(),
        CorrosionDeployState::Terminal { outcome, .. } if outcome.is_success()
    )
}

#[derive(Debug, thiserror::Error)]
pub enum OpsExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error("operations lens returned the wrong collection")]
    WrongLens,
    #[error("operation stream event name was {found:?}, expected {expected}")]
    UnexpectedEventName {
        expected: &'static str,
        found: Option<String>,
    },
    #[error("operation stream ended before a terminal row")]
    StreamEnded,
    #[error("cannot write operation status: {0}")]
    Output(std::io::Error),
    #[error("operation {operation_id} was not found")]
    NotFound { operation_id: String },
    #[error("deploy finished unsuccessfully: {state}")]
    OperationFailed { state: &'static str },
}
