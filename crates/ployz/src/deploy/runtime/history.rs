use std::path::PathBuf;

use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::operation::{DeployCompletionOutcome, OperationEvent, ReplayedOperationEvent};

use crate::deploy::command::DeployHistoryCommand;
use crate::deploy::history_store::{
    ClusterFingerprint, DeployHistory, DeployHistoryEntry, DeployHistoryTimestamp,
    default_deploy_history_root, render_history,
};

use crate::dispatcher::PloyzctlRuntimeConfig;
use crate::execution_error::PloyzctlExecutionError;
use crate::execution_support::{PloyzctlExecutionOutput, with_cluster_context_from_disk};

use super::DeployExecutionError;

pub(crate) fn inspect(
    command: DeployHistoryCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = with_cluster_context_from_disk(config.clone())?;
    let history = stream(&config, command.namespace_id).map_err(execution_error)?;
    let entries = history
        .load()
        .map_err(DeployHistoryRuntimeError::from)
        .map_err(execution_error)?;
    Ok(PloyzctlExecutionOutput::stdout(render_history(&entries)))
}

pub(super) fn stream(
    config: &PloyzctlRuntimeConfig,
    namespace_id: NamespaceId,
) -> Result<DeployHistory, DeployHistoryRuntimeError> {
    let Some(nats_url) = config.nats_url.as_deref() else {
        return Err(DeployHistoryRuntimeError::MissingNatsUrl);
    };
    let Some(ca_file) = config.nats_ca_file.as_deref() else {
        return Err(DeployHistoryRuntimeError::MissingNatsCaFile);
    };
    let Some(root) = config.deploy_history_root() else {
        return Err(DeployHistoryRuntimeError::MissingRoot);
    };
    let cluster = ClusterFingerprint::from_connection(nats_url, ca_file)?;
    Ok(DeployHistory::new(root, cluster, namespace_id))
}

pub(super) fn record_terminal_success(
    history: Result<DeployHistory, DeployHistoryRuntimeError>,
    operation_id: OperationId,
    events: &[ReplayedOperationEvent],
) -> Result<(), DeployHistoryRuntimeError> {
    let mut request = None;
    let mut completion = None;
    let mut evidence_error = None;

    for replayed in events {
        if let OperationEvent::DeploySubmitted {
            operation_id: submitted_id,
            target,
            ..
        } = &replayed.event
            && submitted_id == &operation_id
        {
            request = Some(target.clone());
            continue;
        }
        if let OperationEvent::DeployImageResolved {
            operation_id: resolved_id,
            service_id,
            resolved,
            ..
        } = &replayed.event
            && resolved_id == &operation_id
        {
            let Some(target) = request.as_mut() else {
                evidence_error = Some("image resolution preceded submitted request".to_owned());
                continue;
            };
            let Some(service) = target
                .services
                .iter_mut()
                .find(|service| service.service_id == *service_id)
            else {
                evidence_error = Some(format!(
                    "image resolution names unknown service {}",
                    service_id.as_str()
                ));
                continue;
            };
            if resolved.pinned_digest().is_none() {
                evidence_error = Some(format!(
                    "resolved image {} is not digest-pinned",
                    resolved.as_str()
                ));
                continue;
            }
            if service.image.pinned_digest().is_some() && service.image != *resolved {
                evidence_error = Some(format!(
                    "service {} resolved to multiple digests",
                    service_id.as_str()
                ));
                continue;
            }
            service.image = resolved.clone();
            continue;
        }
        if let OperationEvent::DeployCompleted {
            operation_id: completed_id,
            outcome,
        } = &replayed.event
            && completed_id == &operation_id
        {
            completion = Some(*outcome);
        }
    }

    match completion {
        None
        | Some(DeployCompletionOutcome::PartiallyCompleted)
        | Some(DeployCompletionOutcome::PartiallyCompletedWithWarnings) => Ok(()),
        Some(
            DeployCompletionOutcome::Completed | DeployCompletionOutcome::CompletedWithWarnings,
        ) => {
            if let Some(message) = evidence_error {
                return Err(evidence_error_for(operation_id, message));
            }
            let Some(request) = request else {
                return Err(evidence_error_for(
                    operation_id,
                    "submitted request evidence is missing",
                ));
            };
            if let Some(service) = request
                .services
                .iter()
                .find(|service| service.image.pinned_digest().is_none())
            {
                return Err(evidence_error_for(
                    operation_id,
                    format!(
                        "digest resolution evidence is missing for service {}",
                        service.service_id.as_str()
                    ),
                ));
            }
            history?.append_success(DeployHistoryEntry {
                recorded_at: DeployHistoryTimestamp::now()?,
                operation_id,
                request,
            })?;
            Ok(())
        }
    }
}

impl PloyzctlRuntimeConfig {
    fn deploy_history_root(&self) -> Option<PathBuf> {
        if self.deploy_history_root.is_some() {
            return self.deploy_history_root.clone();
        }
        default_deploy_history_root()
    }
}

fn evidence_error_for(
    operation_id: OperationId,
    message: impl Into<String>,
) -> DeployHistoryRuntimeError {
    DeployHistoryRuntimeError::Evidence {
        operation_id,
        message: message.into(),
    }
}

fn execution_error(source: DeployHistoryRuntimeError) -> PloyzctlExecutionError {
    DeployExecutionError::History {
        message: source.to_string(),
    }
    .into()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum DeployHistoryRuntimeError {
    #[error("no cluster context supplies a NATS URL for deploy history")]
    MissingNatsUrl,
    #[error("no cluster CA file is configured for deploy history")]
    MissingNatsCaFile,
    #[error("cannot determine deploy history directory (set XDG_STATE_HOME or HOME)")]
    MissingRoot,
    #[error("{message}")]
    Store { message: String },
    #[error("completed deploy {} has unusable history evidence: {message}", operation_id.as_str())]
    Evidence {
        operation_id: OperationId,
        message: String,
    },
}

impl From<crate::deploy::history_store::DeployHistoryError> for DeployHistoryRuntimeError {
    fn from(source: crate::deploy::history_store::DeployHistoryError) -> Self {
        Self::Store {
            message: source.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployRequest, DeployServiceSpec, ImageReference, ImageSource,
        ReplicaCount,
    };
    use ployz_core::operation::{DeployOperationFailure, OperationKind};
    use ployz_test_support::ids::{
        cancellation_reason, event_sequence, machine_id, namespace_id, operation_event_recorded_at,
        operation_id, service_id,
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
            recorded_at_unix_ms: operation_event_recorded_at(1_784_116_800_000 + sequence),
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

    fn history(temporary: &tempfile::TempDir) -> DeployHistory {
        let ca_file = temporary.path().join("ca.pem");
        std::fs::write(&ca_file, "test ca").expect("CA writes");
        DeployHistory::new(
            temporary.path().join("history"),
            ClusterFingerprint::from_connection("tls://cluster.example:4222", &ca_file)
                .expect("cluster fingerprints"),
            namespace_id("default"),
        )
    }

    #[test]
    fn record_terminal_success_persists_resolved_digest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = history(&temporary);

        record_terminal_success(
            Ok(history.clone()),
            operation_id("op_deploy"),
            &completed_events(DeployCompletionOutcome::Completed),
        )
        .expect("foreground success records");

        let entries = history.load().expect("history loads");
        let [entry] = entries.as_slice() else {
            panic!("one entry persists");
        };
        let [service] = entry.request.services.as_slice() else {
            panic!("one service persists");
        };
        assert_eq!(
            service.image.as_str(),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn record_terminal_success_includes_completed_with_warnings() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = history(&temporary);

        record_terminal_success(
            Ok(history.clone()),
            operation_id("op_deploy"),
            &completed_events(DeployCompletionOutcome::CompletedWithWarnings),
        )
        .expect("warning completion records");

        assert_eq!(history.load().expect("history loads").len(), 1);
    }

    #[test]
    fn record_terminal_success_excludes_partial_outcomes() {
        for outcome in [
            DeployCompletionOutcome::PartiallyCompleted,
            DeployCompletionOutcome::PartiallyCompletedWithWarnings,
        ] {
            let temporary = tempfile::tempdir().expect("temporary directory");
            let history = history(&temporary);
            record_terminal_success(
                Ok(history.clone()),
                operation_id("op_deploy"),
                &completed_events(outcome),
            )
            .expect("partial completion is ignored");
            assert!(history.load().expect("history loads").is_empty());
        }
    }

    #[test]
    fn non_success_outcome_ignores_unavailable_history_cache() {
        record_terminal_success(
            Err(DeployHistoryRuntimeError::MissingRoot),
            operation_id("op_deploy"),
            &completed_events(DeployCompletionOutcome::PartiallyCompleted),
        )
        .expect("cache is irrelevant to a partial deploy");
    }

    #[test]
    fn record_terminal_success_excludes_failed_deploys() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = history(&temporary);
        let operation_id = operation_id("op_failed");
        let events = [
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
                OperationEvent::DeployFailed {
                    operation_id: operation_id.clone(),
                    failure: DeployOperationFailure::NoUsableMachines {
                        reasons: Vec::new(),
                    },
                },
            ),
        ];

        record_terminal_success(Ok(history.clone()), operation_id, &events)
            .expect("failure is ignored");
        assert!(history.load().expect("history loads").is_empty());
    }

    #[test]
    fn record_terminal_success_excludes_cancelled_deploys() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let history = history(&temporary);
        let operation_id = operation_id("op_cancelled");
        let events = [
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

        record_terminal_success(Ok(history.clone()), operation_id, &events)
            .expect("cancellation is ignored");
        assert!(history.load().expect("history loads").is_empty());
    }

    #[test]
    fn inspect_renders_the_selected_local_stream() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let ca_file = temporary.path().join("ca.pem");
        std::fs::write(&ca_file, "test ca").expect("CA writes");
        let config = PloyzctlRuntimeConfig {
            nats_url: Some("tls://cluster.example:4222".to_owned()),
            nats_ca_file: Some(ca_file),
            nats_seed_file: Some(temporary.path().join("operator.seed")),
            join_seed_file: Some(temporary.path().join("join.seed")),
            deploy_history_root: Some(temporary.path().join("history")),
            ..PloyzctlRuntimeConfig::default()
        };
        let namespace_id = namespace_id("default");
        stream(&config, namespace_id.clone())
            .expect("history stream resolves")
            .append_success(DeployHistoryEntry {
                recorded_at: DeployHistoryTimestamp::from_unix_seconds(1_750_000_000),
                operation_id: operation_id("op_history"),
                request: request(
                    "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ),
            })
            .expect("history entry persists");

        let output = inspect(DeployHistoryCommand { namespace_id }, &config)
            .expect("history command executes");

        assert_eq!(
            output.stdout,
            "1750000000  op_history  default  1 service  web=ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n"
        );
    }
}
