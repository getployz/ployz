use std::io::{self, IsTerminal, Write};

use ployz_core::ids::OperationId;
use ployz_sdk_types::DeployReserveRequest;

use crate::api_client::OperationApiClient;
use crate::commands::deploy::{DeployCommand, DeployOutput};
use crate::commands::deploy_render::{
    DeployTree, render_failure_block, render_frame, render_plain_lines, render_terminal,
};

use super::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig, api_error,
    operation_api_client, operation_replay_request, watch_operation_until_terminal_with,
};

pub(super) async fn execute_deploy(
    mut command: DeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let detach = command.detach;
    let warnings = command.warnings.join("\n");
    if !warnings.is_empty() {
        eprintln!("{warnings}");
    }
    let api = operation_api_client(config).await?;
    let reservation = api
        .deploy_reserve(&DeployReserveRequest {
            namespace_id: command.namespace_id.clone(),
        })
        .await
        .map_err(api_error)?;
    let receipts = crate::image_push::prepare_deploy_images(
        &api,
        &mut command.services,
        command.from_registry,
    )
    .await
    .map_err(|source| PloyzctlExecutionError::ImagePush { source })?;
    let receipt_output = receipts
        .iter()
        .map(crate::image_push::ImagePushReceipt::render)
        .collect::<String>();
    let registry_credentials = crate::registry_auth::deploy_registry_credentials(&command.services)
        .await
        .map_err(|source| PloyzctlExecutionError::RegistryAuth { source })?;
    let mut request = command.into_request(reservation.reservation_id);
    request.registry_credentials = registry_credentials;
    let accepted = api.deploy_submit(&request).await.map_err(api_error)?;
    if detach {
        return Ok(PloyzctlExecutionOutput {
            stdout: format!(
                "{receipt_output}{}",
                DeployOutput::from_accepted(accepted).render()
            ),
            stderr: String::new(),
        });
    }
    let mut output = watch_deploy_operation(&api, accepted.operation_id, config).await?;
    output.stdout.insert_str(0, &receipt_output);
    Ok(output)
}

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
