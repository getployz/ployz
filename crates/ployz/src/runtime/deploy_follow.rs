use std::io::{self, IsTerminal, Write};

use ployz_core::deploy::DeployReservationId;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::ops::ReplayedOperationEvent;
use ployz_sdk_types::{AcceptedOperation, DeployReserveRequest, DeploySubmitRequest};

use crate::api_client::OperationApiClient;
use crate::commands::deploy::{DeployCommand, DeployOutput};
use crate::commands::deploy_render::{
    DeployTree, render_failure_block, render_frame, render_plain_lines, render_terminal,
};

use super::{
    CommandExit, PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig, api_error,
    deploy_history, nats_connect_config, operation_api_client_with_connect,
    operation_replay_request, watch_operation_until_terminal_with,
};

pub(super) async fn execute_deploy(
    mut command: DeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = config.clone().with_cluster_context_from_disk()?;
    let detach = command.detach;
    let namespace_id = command.namespace_id.clone();
    let warnings = command.warnings.join("\n");
    if !warnings.is_empty() {
        eprintln!("{warnings}");
    }
    let connect = nats_connect_config(&config)?;
    let api = operation_api_client_with_connect(&config, connect).await?;
    let reservation_id = reserve_deploy(&api, command.namespace_id.clone()).await?;
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
    let request = command.into_request(reservation_id);
    let accepted = submit_deploy(&api, request).await?;
    if detach {
        return Ok(PloyzctlExecutionOutput {
            stdout: format!(
                "{receipt_output}{}",
                DeployOutput::from_accepted(accepted).render()
            ),
            stderr: String::new(),
            exit: CommandExit::Success,
        });
    }
    follow_accepted_deploy(
        &api,
        accepted.operation_id,
        &config,
        namespace_id,
        receipt_output,
    )
    .await
}

pub(super) async fn reserve_deploy(
    api: &OperationApiClient,
    namespace_id: NamespaceId,
) -> Result<DeployReservationId, PloyzctlExecutionError> {
    api.deploy_reserve(&DeployReserveRequest { namespace_id })
        .await
        .map(|reservation| reservation.reservation_id)
        .map_err(api_error)
}

pub(super) async fn submit_deploy(
    api: &OperationApiClient,
    mut request: DeploySubmitRequest,
) -> Result<AcceptedOperation, PloyzctlExecutionError> {
    request.registry_credentials =
        crate::registry_auth::deploy_registry_credentials(&request.target.services)
            .await
            .map_err(|source| PloyzctlExecutionError::RegistryAuth { source })?;
    api.deploy_submit(&request).await.map_err(api_error)
}

pub(super) async fn follow_accepted_deploy(
    api: &OperationApiClient,
    operation_id: OperationId,
    config: &PloyzctlRuntimeConfig,
    namespace_id: NamespaceId,
    output_prefix: String,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let history = deploy_history::stream(config, namespace_id);
    let (mut output, events) = watch_deploy_operation(api, operation_id.clone(), config).await?;
    let history_result = deploy_history::record_terminal_success(history, operation_id, &events);
    append_history_warning(&mut output, history_result);
    output.stdout.insert_str(0, &output_prefix);
    Ok(output)
}

fn append_history_warning(
    output: &mut PloyzctlExecutionOutput,
    history_result: Result<(), deploy_history::DeployHistoryRuntimeError>,
) {
    if let Err(error) = history_result {
        output.stderr.push_str(&format!(
            "warning: deploy succeeded but local history was not updated: {error}\n"
        ));
    }
}

pub(super) async fn watch_deploy_operation(
    api: &OperationApiClient,
    operation_id: OperationId,
    config: &PloyzctlRuntimeConfig,
) -> Result<(PloyzctlExecutionOutput, Vec<ReplayedOperationEvent>), PloyzctlExecutionError> {
    let mut tree = DeployTree::new();
    let stdout = io::stdout();
    let mode = if stdout.is_terminal() {
        DeployOutputMode::Terminal
    } else {
        DeployOutputMode::Plain
    };
    let mut stdout = stdout.lock();
    let mut output = DeployProgressOutput::new(&mut stdout, mode);

    let (events, outcome) = watch_operation_until_terminal_with(
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

    Ok((
        PloyzctlExecutionOutput::stdout(String::new()).with_operation_outcome(outcome),
        events,
    ))
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

    #[test]
    fn deploy_history_failure_is_a_warning_on_success_output() {
        let mut output = PloyzctlExecutionOutput::stdout("deployed\n".to_owned());

        append_history_warning(
            &mut output,
            Err(deploy_history::DeployHistoryRuntimeError::MissingRoot),
        );

        assert_eq!(output.stdout, "deployed\n");
        assert_eq!(
            output.stderr,
            "warning: deploy succeeded but local history was not updated: cannot determine deploy history directory (set XDG_STATE_HOME or HOME)\n"
        );
    }
}
