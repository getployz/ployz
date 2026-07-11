use std::collections::BTreeMap;
use std::io::{self, BufRead, IsTerminal, Write};

use ployz_core::deploy::{DeployOrigin, DeployRequest};
use ployz_sdk_types::DeploySubmitRequest;

use crate::client_ids::generate_client_deploy_rollback_id;
use crate::commands::deploy::{DeployRollbackCommand, DeployRollbackSelection};

use super::{
    PloyzctlExecutionError, PloyzctlExecutionOutput, PloyzctlRuntimeConfig, deploy_follow,
    deploy_history, nats_connect_config, operation_api_client_with_connect,
};

pub(super) async fn execute(
    command: DeployRollbackCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let config = config.clone().with_cluster_context_from_disk()?;
    let namespace_id = command.namespace_id.clone();
    let mut target = select_request(command, &config)?;
    target.origin = Some(DeployOrigin::try_new("rollback").expect("rollback origin is valid"));
    let connect = nats_connect_config(&config)?;
    let api = operation_api_client_with_connect(&config, connect).await?;
    let reservation_id = deploy_follow::reserve_deploy(&api, namespace_id.clone()).await?;
    let generated = generate_client_deploy_rollback_id().map_err(|error| {
        PloyzctlExecutionError::GenerateClientOperationIds {
            message: error.to_string(),
        }
    })?;
    let accepted = deploy_follow::submit_deploy(
        &api,
        DeploySubmitRequest {
            idempotency_key: generated.idempotency_key,
            reservation_id,
            target,
            registry_credentials: BTreeMap::new(),
        },
    )
    .await?;
    deploy_follow::follow_accepted_deploy(
        &api,
        accepted.operation_id,
        &config,
        namespace_id,
        String::new(),
    )
    .await
}

fn select_request(
    command: DeployRollbackCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<DeployRequest, PloyzctlExecutionError> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    select_request_with_io(
        command,
        config,
        stdin.is_terminal(),
        &mut stdin.lock(),
        &mut stdout.lock(),
    )
}

fn select_request_with_io<R: BufRead, W: Write>(
    command: DeployRollbackCommand,
    config: &PloyzctlRuntimeConfig,
    stdin_is_terminal: bool,
    input: &mut R,
    output: &mut W,
) -> Result<DeployRequest, PloyzctlExecutionError> {
    let entries = deploy_history::stream(config, command.namespace_id)
        .map_err(|error| deploy_history_error(error.to_string()))?
        .load()
        .map_err(|error| deploy_history_error(error.to_string()))?;

    match command.selection {
        DeployRollbackSelection::Operation(operation_id) => entries
            .iter()
            .find(|entry| entry.operation_id == operation_id)
            .map(|entry| entry.request.clone())
            .ok_or_else(|| {
                deploy_history_error(format!(
                    "deploy history has no successful operation {}",
                    operation_id.as_str()
                ))
            }),
        DeployRollbackSelection::LastGood => {
            let [.., selected, _newest] = entries.as_slice() else {
                return Err(deploy_history_error(
                    "rollback --last-good requires at least two successful deploys",
                ));
            };
            Ok(selected.request.clone())
        }
        DeployRollbackSelection::Interactive => {
            if !stdin_is_terminal {
                return Err(deploy_history_error(
                    "interactive rollback requires a terminal; use --to or --last-good",
                ));
            }
            let prior = entries
                .split_last()
                .map(|(_newest, prior)| prior)
                .filter(|prior| !prior.is_empty())
                .ok_or_else(|| {
                    deploy_history_error(
                        "interactive rollback requires at least two successful deploys",
                    )
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
                    deploy_history_error(format!("could not write rollback picker: {error}"))
                })?;

            let mut response = String::new();
            let bytes_read = input.read_line(&mut response).map_err(|error| {
                deploy_history_error(format!("could not read rollback selection: {error}"))
            })?;
            if bytes_read == 0 {
                return Err(deploy_history_error("rollback selection was not provided"));
            }
            let selected_index = if response.trim().is_empty() {
                0
            } else {
                response
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .and_then(|number| number.checked_sub(1))
                    .ok_or_else(|| {
                        deploy_history_error("rollback selection must be a listed number")
                    })?
            };
            prior
                .iter()
                .rev()
                .nth(selected_index)
                .map(|entry| entry.request.clone())
                .ok_or_else(|| deploy_history_error("rollback selection must be a listed number"))
        }
    }
}

fn deploy_history_error(message: impl Into<String>) -> PloyzctlExecutionError {
    PloyzctlExecutionError::DeployHistory {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::deploy::{
        ContainerRuntimeSpec, DeployServiceSpec, ImageReference, ImageSource, ReplicaCount,
    };
    use ployz_test_support::ids::{namespace_id, operation_id, service_id};
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

    fn append_history_entry(
        history: &crate::deploy_history::DeployHistory,
        operation: &str,
        image: &str,
    ) {
        history
            .append_success(crate::deploy_history::DeployHistoryEntry {
                recorded_at: crate::deploy_history::DeployHistoryTimestamp::from_unix_seconds(
                    1_750_000_000,
                ),
                operation_id: operation_id(operation),
                request: request(image),
            })
            .expect("history entry persists");
    }

    fn populated_history(
        temporary: &tempfile::TempDir,
    ) -> (PloyzctlRuntimeConfig, ployz_core::ids::NamespaceId) {
        let config = runtime_config(temporary);
        let namespace_id = namespace_id("default");
        let history =
            deploy_history::stream(&config, namespace_id.clone()).expect("history stream resolves");
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

    fn selected_image(request: &DeployRequest) -> &str {
        let [service] = request.services.as_slice() else {
            panic!("one selected service");
        };
        service.image.as_str()
    }

    #[test]
    fn rollback_to_operation_selects_its_exact_request() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_history(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let selected = select_request_with_io(
            DeployRollbackCommand {
                namespace_id,
                selection: DeployRollbackSelection::Operation(operation_id("op_first")),
            },
            &config,
            false,
            &mut input,
            &mut output,
        )
        .expect("operation exists");

        assert_eq!(
            selected_image(&selected),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn rollback_to_operation_errors_when_history_has_no_matching_operation() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = runtime_config(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_request_with_io(
            DeployRollbackCommand {
                namespace_id: namespace_id("default"),
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
        let (config, namespace_id) = populated_history(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let selected = select_request_with_io(
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
            selected_image(&selected),
            "ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
    }

    #[test]
    fn last_good_errors_when_history_has_fewer_than_two_deploys() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let config = runtime_config(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_request_with_io(
            DeployRollbackCommand {
                namespace_id: namespace_id("default"),
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
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_request_with_io(
            DeployRollbackCommand {
                namespace_id: namespace_id("default"),
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
        let (config, namespace_id) = populated_history(&temporary);
        let mut input = Cursor::new(b"\n".to_vec());
        let mut output = Vec::new();

        let selected = select_request_with_io(
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
            selected_image(&selected),
            "ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(
            String::from_utf8(output).expect("picker output is UTF-8"),
            concat!(
                "Select a successful deploy to roll back to:\n",
                "  1) op_last_good  web=ghcr.io/acme/web@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
                "  2) op_first  web=ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
                "Rollback [1]: "
            )
        );
    }

    #[test]
    fn interactive_rollback_accepts_a_numbered_older_candidate() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_history(&temporary);
        let mut input = Cursor::new(b"2\n".to_vec());
        let mut output = Vec::new();

        let selected = select_request_with_io(
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
            selected_image(&selected),
            "ghcr.io/acme/web@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }

    #[test]
    fn interactive_rollback_rejects_an_unlisted_number() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let (config, namespace_id) = populated_history(&temporary);
        let mut input = Cursor::new(b"3\n".to_vec());
        let mut output = Vec::new();

        let error = select_request_with_io(
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
        let (config, namespace_id) = populated_history(&temporary);
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();

        let error = select_request_with_io(
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
}
