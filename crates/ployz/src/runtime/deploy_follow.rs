use std::io::{self, IsTerminal, Write};

use ployz_core::deploy::{DeployRequest, ImageReference};
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{DeployCompletionOutcome, OperationEvent, ReplayedOperationEvent};
use ployz_sdk_types::DeployReserveRequest;

use crate::api_client::OperationApiClient;
use crate::commands::deploy::{DeployCommand, DeployOutput};
use crate::commands::deploy_render::{
    DeployTree, render_failure_block, render_frame, render_plain_lines, render_terminal,
};
use crate::deploy_history::{DeployHistory, DeployHistoryEntry, DeployHistoryTimestamp};

use super::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig, api_error,
    deploy_history, deploy_history_error, nats_connect_config, operation_api_client_with_connect,
    operation_replay_request, watch_operation_until_terminal_with,
};

pub(super) async fn execute_deploy(
    mut command: DeployCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = config.clone().with_cluster_context_from_disk()?;
    let detach = command.detach;
    let history = if detach {
        None
    } else {
        Some(deploy_history(&config, command.namespace_id.clone())?)
    };
    let warnings = command.warnings.join("\n");
    if !warnings.is_empty() {
        eprintln!("{warnings}");
    }
    let connect = nats_connect_config(&config)?;
    let api = operation_api_client_with_connect(&config, connect).await?;
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
    let operation_id = accepted.operation_id;
    let (mut output, events) = watch_deploy_operation(&api, operation_id.clone(), &config).await?;
    let Some(history) = history else {
        return Err(completed_history_evidence_error(
            operation_id,
            "history stream was not initialized",
        ));
    };
    record_successful_deploy(&history, operation_id, &events)?;
    output.stdout.insert_str(0, &receipt_output);
    Ok(output)
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

    let events = watch_operation_until_terminal_with(
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

    Ok((PloyzctlExecutionOutput::stdout(String::new()), events))
}

fn successful_history_entry(
    operation_id: OperationId,
    events: &[ReplayedOperationEvent],
) -> Result<Option<DeployHistoryEntry>, PloyzctlExecutionError> {
    let successful = events.iter().rev().find_map(|replayed| {
        let OperationEvent::DeployCompleted {
            operation_id: completed_id,
            outcome,
        } = &replayed.event
        else {
            return None;
        };
        (completed_id == &operation_id).then_some(matches!(
            *outcome,
            DeployCompletionOutcome::Completed | DeployCompletionOutcome::CompletedWithWarnings
        ))
    });
    if successful != Some(true) {
        return Ok(None);
    }

    let Some(mut request) = events.iter().find_map(|replayed| {
        let OperationEvent::DeploySubmitted {
            operation_id: submitted_id,
            target,
            ..
        } = &replayed.event
        else {
            return None;
        };
        (submitted_id == &operation_id).then(|| target.clone())
    }) else {
        return Err(completed_history_evidence_error(
            operation_id,
            "submitted request evidence is missing",
        ));
    };

    let resolved = events.iter().filter_map(|replayed| {
        let OperationEvent::DeployImageResolved {
            operation_id: resolved_id,
            service_id,
            resolved,
            ..
        } = &replayed.event
        else {
            return None;
        };
        (resolved_id == &operation_id).then_some((service_id, resolved))
    });
    let mut images = std::collections::BTreeMap::<ServiceId, ImageReference>::new();
    for (service_id, image) in resolved {
        if image.pinned_digest().is_none() {
            return Err(completed_history_evidence_error(
                operation_id,
                format!("resolved image {} is not digest-pinned", image.as_str()),
            ));
        }
        if let Some(previous) = images.insert(service_id.clone(), image.clone())
            && previous != *image
        {
            return Err(completed_history_evidence_error(
                operation_id,
                format!(
                    "service {} resolved to multiple digests",
                    service_id.as_str()
                ),
            ));
        }
    }
    pin_request_images(&operation_id, &mut request, &images)?;

    Ok(Some(DeployHistoryEntry {
        recorded_at: DeployHistoryTimestamp::now().map_err(deploy_history_error)?,
        operation_id,
        request,
    }))
}

fn record_successful_deploy(
    history: &DeployHistory,
    operation_id: OperationId,
    events: &[ReplayedOperationEvent],
) -> Result<bool, PloyzctlExecutionError> {
    let Some(entry) = successful_history_entry(operation_id, events)? else {
        return Ok(false);
    };
    history
        .append_success(entry)
        .map_err(deploy_history_error)?;
    Ok(true)
}

fn pin_request_images(
    operation_id: &OperationId,
    request: &mut DeployRequest,
    images: &std::collections::BTreeMap<ServiceId, ImageReference>,
) -> Result<(), PloyzctlExecutionError> {
    for service in &mut request.services {
        if let Some(image) = images.get(&service.service_id) {
            service.image = image.clone();
        }
        if service.image.pinned_digest().is_none() {
            return Err(completed_history_evidence_error(
                operation_id.clone(),
                format!(
                    "digest resolution evidence is missing for service {}",
                    service.service_id.as_str()
                ),
            ));
        }
    }
    Ok(())
}

fn completed_history_evidence_error(
    operation_id: OperationId,
    message: impl Into<String>,
) -> PloyzctlExecutionError {
    PloyzctlExecutionError::CompletedDeployHistoryEvidence {
        operation_id,
        message: message.into(),
    }
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
    use ployz_core::deploy::{ContainerRuntimeSpec, DeployServiceSpec, ImageSource, ReplicaCount};
    use ployz_core::ops::{DeployOperationFailure, OperationKind};
    use ployz_test_support::ids::{
        cancellation_reason, event_sequence, machine_id, namespace_id, operation_id, service_id,
    };

    fn request(image: &str) -> DeployRequest {
        DeployRequest {
            namespace_id: namespace_id("default"),
            origin: None,
            services: vec![DeployServiceSpec {
                service_id: service_id("web"),
                image: ImageReference::try_new(image).expect("valid image"),
                image_source: ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        }
    }

    fn replay(sequence: u64, event: OperationEvent) -> ReplayedOperationEvent {
        ReplayedOperationEvent {
            sequence: event_sequence(sequence),
            event,
        }
    }

    fn completed_events(outcome: DeployCompletionOutcome) -> Vec<ReplayedOperationEvent> {
        let operation_id = operation_id("op_deploy");
        vec![
            replay(
                1,
                OperationEvent::DeploySubmitted {
                    operation_id: operation_id.clone(),
                    reservation_id: None,
                    target: request("ghcr.io/acme/web:latest"),
                },
            ),
            replay(
                2,
                OperationEvent::DeployImageResolved {
                    operation_id: operation_id.clone(),
                    service_id: service_id("web"),
                    machine_id: machine_id("edge-1"),
                    requested: ImageReference::try_new("ghcr.io/acme/web:latest")
                        .expect("valid requested image"),
                    resolved: ImageReference::try_new(
                        "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .expect("valid resolved image"),
                    credential_supplied: false,
                },
            ),
            replay(
                3,
                OperationEvent::DeployCompleted {
                    operation_id,
                    outcome,
                },
            ),
        ]
    }

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
    fn successful_history_entry_replaces_tag_with_resolved_digest() {
        let entry = successful_history_entry(
            operation_id("op_deploy"),
            &completed_events(DeployCompletionOutcome::Completed),
        )
        .expect("completed deploy is reconstructable")
        .expect("completed deploy is recorded");

        let [service] = entry.request.services.as_slice() else {
            panic!("history entry has one service");
        };
        assert_eq!(
            service.image.as_str(),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn successful_history_entry_includes_completed_with_warnings() {
        let entry = successful_history_entry(
            operation_id("op_deploy"),
            &completed_events(DeployCompletionOutcome::CompletedWithWarnings),
        )
        .expect("completed deploy is reconstructable");

        assert!(entry.is_some());
    }

    #[test]
    fn successful_history_entry_excludes_partial_deploys() {
        for outcome in [
            DeployCompletionOutcome::PartiallyCompleted,
            DeployCompletionOutcome::PartiallyCompletedWithWarnings,
        ] {
            assert!(
                successful_history_entry(operation_id("op_deploy"), &completed_events(outcome))
                    .expect("partial completion is classified")
                    .is_none()
            );
        }
    }

    #[test]
    fn successful_history_entry_excludes_failed_deploys() {
        let failed_operation_id = operation_id("op_failed");
        let failed = vec![
            replay(
                1,
                OperationEvent::DeploySubmitted {
                    operation_id: failed_operation_id.clone(),
                    reservation_id: None,
                    target: request("ghcr.io/acme/web:latest"),
                },
            ),
            replay(
                2,
                OperationEvent::DeployFailed {
                    operation_id: failed_operation_id.clone(),
                    failure: DeployOperationFailure::NoUsableMachines {
                        reasons: Vec::new(),
                    },
                },
            ),
        ];

        assert!(
            successful_history_entry(failed_operation_id, &failed)
                .expect("failure is classified")
                .is_none()
        );
    }

    #[test]
    fn successful_history_entry_excludes_cancelled_deploys() {
        let operation_id = operation_id("op_cancelled");
        let events = vec![
            replay(
                1,
                OperationEvent::DeploySubmitted {
                    operation_id: operation_id.clone(),
                    reservation_id: None,
                    target: request("ghcr.io/acme/web:latest"),
                },
            ),
            replay(
                2,
                OperationEvent::Cancelled {
                    operation_id: operation_id.clone(),
                    kind: OperationKind::Deploy,
                    reason: cancellation_reason("operator cancelled"),
                },
            ),
        ];

        assert!(
            successful_history_entry(operation_id, &events)
                .expect("cancellation is classified")
                .is_none()
        );
    }

    #[test]
    fn foreground_success_recording_persists_the_resolved_digest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = DeployHistory::new(
            temporary.path().to_owned(),
            crate::deploy_history::ClusterFingerprint::from_connection(
                "tls://cluster.example:4222",
                &write_test_ca(temporary.path()),
            )
            .expect("cluster fingerprints"),
            namespace_id("default"),
        );

        assert!(
            record_successful_deploy(
                &history,
                operation_id("op_deploy"),
                &completed_events(DeployCompletionOutcome::Completed),
            )
            .expect("foreground success records")
        );
        let entries = history.load().expect("history loads");
        let [entry] = entries.as_slice() else {
            panic!("one history entry persisted");
        };
        let [service] = entry.request.services.as_slice() else {
            panic!("history entry has one service");
        };
        assert_eq!(
            service.image.as_str(),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    fn write_test_ca(directory: &std::path::Path) -> std::path::PathBuf {
        let path = directory.join("ca.pem");
        std::fs::write(&path, "test ca").expect("CA writes");
        path
    }

    #[tokio::test]
    async fn deploy_history_command_renders_the_selected_local_stream() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let ca_file = write_test_ca(temporary.path());
        let config = PloyzctlRuntimeConfig {
            nats_url: Some("tls://cluster.example:4222".to_owned()),
            nats_ca_file: Some(ca_file),
            nats_seed_file: Some(temporary.path().join("operator.seed")),
            join_seed_file: Some(temporary.path().join("join.seed")),
            deploy_history_root: Some(temporary.path().join("history")),
            ..PloyzctlRuntimeConfig::default()
        };
        let namespace_id = namespace_id("default");
        deploy_history(&config, namespace_id.clone())
            .expect("history stream resolves")
            .append_success(DeployHistoryEntry {
                recorded_at: DeployHistoryTimestamp::from_unix_seconds(1_750_000_000),
                operation_id: operation_id("op_history"),
                request: request(
                    "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
            })
            .expect("history entry persists");

        let output = crate::runtime::execute_command(
            crate::commands::PloyzctlCommand::DeployHistory(
                crate::commands::deploy::DeployHistoryCommand { namespace_id },
            ),
            &config,
        )
        .await
        .expect("history command executes");

        assert_eq!(
            output.stdout,
            "1750000000  op_history  default  1 service  web=ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        );
    }
}
