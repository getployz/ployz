use std::io::{self, BufRead, IsTerminal, Write};
use std::path::PathBuf;

use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{NamespaceId, OperationId};
use ployz_core::ops::{DeployCompletionOutcome, OperationEvent, ReplayedOperationEvent};

use crate::commands::deploy::{
    DeployHistoryCommand, DeployRollbackCommand, DeployRollbackSelection,
};
use crate::deploy_history::{
    ClusterFingerprint, DeployHistory, DeployHistoryEntry, DeployHistoryTimestamp,
    default_deploy_history_root, render_history,
};

use super::{PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig};

pub(super) fn select_rollback_request(
    command: DeployRollbackCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<DeployRequest, PloyzctlExecutionError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    select_rollback_request_with_io(
        command,
        config,
        stdin.is_terminal(),
        &mut stdin.lock(),
        &mut stdout.lock(),
    )
}

fn select_rollback_request_with_io<R: BufRead, W: Write>(
    command: DeployRollbackCommand,
    config: &PloyzctlRuntimeConfig,
    stdin_is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<DeployRequest, PloyzctlExecutionError> {
    let config = config.clone().with_cluster_context_from_disk()?;
    let entries = stream(&config, command.namespace_id)
        .map_err(execution_error)?
        .load()
        .map_err(DeployHistoryRuntimeError::from)
        .map_err(execution_error)?;

    match command.selection {
        DeployRollbackSelection::Operation(operation_id) => entries
            .iter()
            .find(|entry| entry.operation_id == operation_id)
            .map(|entry| entry.request.clone())
            .ok_or_else(|| {
                selection_error(format!(
                    "deploy history has no successful operation {}",
                    operation_id.as_str()
                ))
            }),
        DeployRollbackSelection::LastGood => {
            let [.., selected, _newest] = entries.as_slice() else {
                return Err(selection_error(
                    "rollback --last-good requires at least two successful deploys",
                ));
            };
            Ok(selected.request.clone())
        }
        DeployRollbackSelection::Interactive => {
            if !stdin_is_terminal {
                return Err(selection_error(
                    "interactive rollback requires a terminal; use --to or --last-good",
                ));
            }
            let prior = entries
                .split_last()
                .map(|(_newest, prior)| prior)
                .filter(|prior| !prior.is_empty())
                .ok_or_else(|| {
                    selection_error("interactive rollback requires at least two successful deploys")
                })?;

            writeln!(output, "Select a successful deploy to roll back to:")
                .and_then(|()| {
                    for (index, entry) in prior.iter().rev().enumerate() {
                        write!(output, "  {}) {}", index + 1, entry.operation_id.as_str())?;
                        for service in &entry.request.services {
                            write!(
                                output,
                                "  {}={}",
                                service.service_id.as_str(),
                                service.image.as_str()
                            )?;
                        }
                        writeln!(output)?;
                    }
                    write!(output, "Rollback [1]: ")?;
                    output.flush()
                })
                .map_err(|error| {
                    selection_error(format!("could not write rollback picker: {error}"))
                })?;

            let mut response = String::new();
            let bytes_read = input.read_line(&mut response).map_err(|error| {
                selection_error(format!("could not read rollback selection: {error}"))
            })?;
            if bytes_read == 0 {
                return Err(selection_error("rollback selection was not provided"));
            }
            let selected_index = if response.trim().is_empty() {
                0
            } else {
                response
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|number| number.checked_sub(1))
                    .ok_or_else(|| selection_error("rollback selection must be a listed number"))?
            };
            prior
                .iter()
                .rev()
                .nth(selected_index)
                .map(|entry| entry.request.clone())
                .ok_or_else(|| selection_error("rollback selection must be a listed number"))
        }
    }
}

pub(super) fn inspect(
    command: DeployHistoryCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = config.clone().with_cluster_context_from_disk()?;
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
    PloyzctlExecutionError::DeployHistory {
        message: source.to_string(),
    }
}

fn selection_error(message: impl Into<String>) -> PloyzctlExecutionError {
    execution_error(DeployHistoryRuntimeError::Selection {
        message: message.into(),
    })
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
    #[error("{message}")]
    Selection { message: String },
    #[error("completed deploy {} has unusable history evidence: {message}", operation_id.as_str())]
    Evidence {
        operation_id: OperationId,
        message: String,
    },
}

impl From<crate::deploy_history::DeployHistoryError> for DeployHistoryRuntimeError {
    fn from(source: crate::deploy_history::DeployHistoryError) -> Self {
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
    use ployz_core::ops::{DeployOperationFailure, OperationKind};
    use ployz_test_support::ids::{
        cancellation_reason, event_sequence, machine_id, namespace_id, operation_id, service_id,
    };
    use std::io::Cursor;

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

    fn runtime_config(temporary: &tempfile::TempDir) -> PloyzctlRuntimeConfig {
        let ca_file = temporary.path().join("ca.pem");
        std::fs::write(&ca_file, "test ca").expect("CA writes");
        PloyzctlRuntimeConfig {
            nats_url: Some("tls://cluster.example:4222".to_owned()),
            nats_ca_file: Some(ca_file),
            nats_seed_file: Some(temporary.path().join("operator.seed")),
            join_seed_file: Some(temporary.path().join("join.seed")),
            deploy_history_root: Some(temporary.path().join("history")),
            ..PloyzctlRuntimeConfig::default()
        }
    }

    fn append_history_entry(history: &DeployHistory, operation: &str, image: &str) {
        history
            .append_success(DeployHistoryEntry {
                recorded_at: DeployHistoryTimestamp::from_unix_seconds(1_750_000_000),
                operation_id: operation_id(operation),
                request: request(image),
            })
            .expect("history entry persists");
    }

    fn populated_rollback_history(
        temporary: &tempfile::TempDir,
    ) -> (PloyzctlRuntimeConfig, NamespaceId) {
        let config = runtime_config(temporary);
        let namespace_id = namespace_id("default");
        let history = stream(&config, namespace_id.clone()).expect("history stream resolves");
        append_history_entry(
            &history,
            "op_first",
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        append_history_entry(
            &history,
            "op_last_good",
            "ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        append_history_entry(
            &history,
            "op_newest",
            "ghcr.io/acme/web@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
        );
        (config, namespace_id)
    }

    #[test]
    fn rollback_to_operation_selects_its_exact_request() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = runtime_config(&temporary);
        let namespace_id = namespace_id("default");
        let history = stream(&config, namespace_id.clone()).expect("history stream resolves");
        append_history_entry(
            &history,
            "op_old",
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        );
        append_history_entry(
            &history,
            "op_new",
            "ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let selected = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Operation(operation_id("op_old")),
            },
            &config,
            false,
            &mut input,
            &mut output,
        )
        .expect("operation exists");

        assert_eq!(
            selected.services[0].image.as_str(),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn rollback_to_operation_errors_when_history_has_no_matching_operation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = runtime_config(&temporary);
        let namespace_id = namespace_id("default");
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Operation(operation_id("op_missing")),
            },
            &config,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("missing operation cannot be replayed");

        assert!(error.to_string().contains("op_missing"));
    }

    #[test]
    fn last_good_selects_the_entry_immediately_before_the_newest() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_rollback_history(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let selected = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::LastGood,
            },
            &config,
            false,
            &mut input,
            &mut output,
        )
        .expect("last good exists");

        assert_eq!(
            selected.services[0].image.as_str(),
            "ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn last_good_errors_when_history_has_fewer_than_two_deploys() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = runtime_config(&temporary);
        let namespace_id = namespace_id("default");
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::LastGood,
            },
            &config,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("last good needs current and prior deploys");

        assert!(error.to_string().contains("at least two"));
    }

    #[test]
    fn interactive_rollback_requires_a_terminal() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = runtime_config(&temporary);
        let namespace_id = namespace_id("default");
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Interactive,
            },
            &config,
            false,
            &mut input,
            &mut output,
        )
        .expect_err("non-terminal input is ambiguous");

        assert!(error.to_string().contains("--to or --last-good"));
    }

    #[test]
    fn interactive_rollback_defaults_to_last_good_and_renders_pinned_candidates() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_rollback_history(&temporary);
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();

        let selected = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Interactive,
            },
            &config,
            true,
            &mut input,
            &mut output,
        )
        .expect("default selection exists");

        assert_eq!(
            (
                selected.services[0].image.as_str(),
                String::from_utf8(output).expect("picker output is UTF-8")
            ),
            (
                "ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                concat!(
                    "Select a successful deploy to roll back to:\n",
                    "  1) op_last_good  web=ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
                    "  2) op_first  web=ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
                    "Rollback [1]: "
                )
                .to_owned()
            )
        );
    }

    #[test]
    fn interactive_rollback_accepts_a_numbered_older_candidate() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_rollback_history(&temporary);
        let mut input = Cursor::new(b"2\n".to_vec());
        let mut output = Vec::new();

        let selected = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Interactive,
            },
            &config,
            true,
            &mut input,
            &mut output,
        )
        .expect("second candidate exists");

        assert_eq!(
            selected.services[0].image.as_str(),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn interactive_rollback_rejects_an_unlisted_number() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_rollback_history(&temporary);
        let mut input = Cursor::new(b"3\n".to_vec());
        let mut output = Vec::new();

        let error = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Interactive,
            },
            &config,
            true,
            &mut input,
            &mut output,
        )
        .expect_err("third prior deploy is not listed");

        assert!(error.to_string().contains("must be a listed number"));
    }

    #[test]
    fn interactive_rollback_rejects_end_of_input() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_rollback_history(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_rollback_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Interactive,
            },
            &config,
            true,
            &mut input,
            &mut output,
        )
        .expect_err("end of input is not an empty line");

        assert!(error.to_string().contains("was not provided"));
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
