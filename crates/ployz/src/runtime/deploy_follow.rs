use std::io::{self, IsTerminal, Write};

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
    let mut tree = DeployTree::new();
    let stdout = io::stdout();
    let mode = if stdout.is_terminal() {
        DeployOutputMode::Terminal
    } else {
        DeployOutputMode::Plain
    };
    let mut stdout = stdout.lock();
    let mut output = DeployProgressOutput::new(&mut stdout, mode);

    watch_operation_until_terminal_with(
        api,
        operation_replay_request(operation_id),
        config.ops_watch_timeout(),
        config.ops_watch_poll_interval(),
        |events| {
            tree.ingest_page(events);
            if matches!(mode, DeployOutputMode::Terminal) {
                tree.tick_spinner();
            }
            output.render_page(&tree)
        },
    )
    .await?;
    output.finish(&tree)?;

    Ok(PloyzctlExecutionOutput::stdout(String::new()))
}

#[derive(Clone, Copy)]
enum DeployOutputMode {
    Terminal,
    Plain,
}

struct DeployProgressOutput<'a, W> {
    stdout: &'a mut W,
    mode: DeployOutputMode,
    previous_frame_lines: usize,
    rendered_plain_bytes: usize,
}

impl<'a, W: Write> DeployProgressOutput<'a, W> {
    fn new(stdout: &'a mut W, mode: DeployOutputMode) -> Self {
        Self {
            stdout,
            mode,
            previous_frame_lines: 0,
            rendered_plain_bytes: 0,
        }
    }

    fn render_page(&mut self, tree: &DeployTree) -> Result<(), PloyzctlExecutionError> {
        match self.mode {
            DeployOutputMode::Terminal => {
                let terminal = render_terminal(tree);
                let frame = if terminal.is_empty() {
                    render_frame(tree)
                } else {
                    render_frame(tree) + "\n" + &terminal
                };
                redraw_frame(self.stdout, &frame, &mut self.previous_frame_lines)
            }
            DeployOutputMode::Plain => self.write_plain(&render_plain_lines(tree)),
        }
    }

    fn finish(&mut self, tree: &DeployTree) -> Result<(), PloyzctlExecutionError> {
        if matches!(self.mode, DeployOutputMode::Plain) {
            let failure = render_failure_block(tree);
            if !failure.is_empty() {
                self.stdout.write_all(b"\n").map_err(write_error)?;
                self.stdout
                    .write_all(failure.as_bytes())
                    .map_err(write_error)?;
                self.stdout.flush().map_err(write_error)?;
            }
        }
        Ok(())
    }

    fn write_plain(&mut self, rendered: &str) -> Result<(), PloyzctlExecutionError> {
        let Some(new_output) = rendered.get(self.rendered_plain_bytes..) else {
            return Ok(());
        };
        self.stdout
            .write_all(new_output.as_bytes())
            .map_err(write_error)?;
        self.stdout.flush().map_err(write_error)?;
        self.rendered_plain_bytes = rendered.len();
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_progress_writes_each_append_only_suffix_immediately() {
        let mut bytes = Vec::new();
        let mut output = DeployProgressOutput::new(&mut bytes, DeployOutputMode::Plain);

        output.write_plain("planning\n").expect("first page writes");
        output
            .write_plain("planning\ncontainer running\n")
            .expect("second page writes only its suffix");

        assert_eq!(bytes, b"planning\ncontainer running\n");
    }
}
