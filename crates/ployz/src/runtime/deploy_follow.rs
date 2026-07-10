use std::io::{self, IsTerminal, Write};
use std::time::Instant;

use ployz_core::ids::OperationId;

use crate::api_client::OperationApiClient;
use crate::commands::deploy_render::{
    DeployTree, render_failure_block, render_frame, render_plain_lines, render_terminal,
};

use super::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig,
    operation_replay_request, watch_operation_until_terminal_with,
};

pub(super) async fn watch_deploy_operation(
    api: &OperationApiClient,
    operation_id: OperationId,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let started_at = Instant::now();
    let mut tree = DeployTree::new();
    let stdout = io::stdout();
    let stdout_is_terminal = stdout.is_terminal();
    let mut stdout = stdout.lock();
    let mut previous_frame_lines = 0;
    let mut plain_output = String::new();
    let mut rendered_plain_bytes = 0;

    watch_operation_until_terminal_with(
        api,
        operation_replay_request(operation_id),
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
        |events| {
            tree.ingest_page(events, started_at.elapsed());
            if stdout_is_terminal {
                tree.tick_spinner();
                let terminal = render_terminal(&tree);
                let frame = if terminal.is_empty() {
                    render_frame(&tree)
                } else {
                    render_frame(&tree) + "\n" + &terminal
                };
                redraw_frame(&mut stdout, &frame, &mut previous_frame_lines)
            } else {
                // The streamed body must stay prefix-stable, so only the
                // append-only plain lines are diffed here; the failure
                // block is emitted exactly once after the watch ends.
                let rendered = render_plain_lines(&tree);
                if let Some(new_output) = rendered.get(rendered_plain_bytes..) {
                    plain_output.push_str(new_output);
                    rendered_plain_bytes = rendered.len();
                }
                Ok(())
            }
        },
    )
    .await?;

    if stdout_is_terminal {
        Ok(PloyzctlExecutionOutput::stdout(String::new()))
    } else {
        let failure = render_failure_block(&tree);
        if !failure.is_empty() {
            plain_output.push('\n');
            plain_output.push_str(&failure);
        }
        Ok(PloyzctlExecutionOutput::stdout(plain_output))
    }
}

fn redraw_frame(
    stdout: &mut impl Write,
    frame: &str,
    previous_frame_lines: &mut usize,
) -> Result<(), PloyzctlExecutionError> {
    if *previous_frame_lines > 0 {
        write!(stdout, "\x1b[{}A\x1b[0J", previous_frame_lines).map_err(write_error)?;
    }
    stdout.write_all(frame.as_bytes()).map_err(write_error)?;
    stdout.flush().map_err(write_error)?;
    *previous_frame_lines = frame.lines().count();
    Ok(())
}

fn write_error(error: io::Error) -> PloyzctlExecutionError {
    PloyzctlExecutionError::WriteDeployProgress {
        message: error.to_string(),
    }
}
