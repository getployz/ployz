use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::cloud_bootstrap::{
    CloudBootstrapAttemptState, CloudBootstrapCallbackTarget, CloudBootstrapLocalState,
    cloud_joiner_connect_config, cloud_joiner_success_callback,
    inspect_cloud_bootstrap_local_state, load_existing_cloud_attempt, load_or_create_cloud_attempt,
    persist_cloud_terminal_callback, reset_cloud_attempt, write_cloud_joiner_trusted_ca,
};
use crate::cloud_client::CloudClient;
use crate::command::{HostRunnerCommandRunner, SystemHostRunnerCommandRunner};
use crate::executor::{HostRunnerPlanFailure, HostRunnerPlanTerminal};
use crate::join_executor::execute_host_runner_join_with_redeemed;
use crate::local::{HostRunnerLocalConfig, HostRunnerLocalEffects};
use crate::release_manifest::{
    ReleaseManifest, default_release_manifest_url, persisted_release_manifest_url,
};
use crate::report::HostRunnerTextRecorder;
use crate::steps::{HostRunnerStepFailureReason, JoinToken};
use ployz_core::install::{
    AbsoluteInstallPath, DEFAULT_MACHINE_BOOTSTRAP_URL, FirstMachineInstallSpec,
    MachineBootstrapUrl, MachineJoinClusterName, MachineJoinRuntimeNatsUrl,
};
use ployz_core::nats_config::{CredentialGrant, CredentialName, CredentialRole, NatsUserSeed};
use ployz_core::roles::GatewayRole;
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsConnectConfig, NatsTlsTrust, connect_authenticated,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    CloudBootstrapCallbackAccepted, CloudBootstrapCallbackRequest, CloudBootstrapClientInfo,
    CloudBootstrapDecision, CloudBootstrapEnvelope, CloudBootstrapFailure, CloudBootstrapIntent,
    CloudBootstrapMachineFacts, CloudBootstrapOutcome, CloudBootstrapSessionCreateRequest,
    CloudBootstrapSessionPollRequest, CloudBootstrapToken, CloudBootstrapTokenRedeemRequest,
    CloudFounderBootstrap, CloudJoinerBootstrap, InitFirstMachineActivateRequest, MachineId,
};

use crate::first_machine::{read_cloud_founder_bootstrap_result, run_first_machine_install};
use crate::join_client::{CloudJoinTokenConsumer, JoinRedeemer, JoinReporter};
use crate::runtime::{
    CLOUD_BOOTSTRAP_MAX_POLLS, DEFAULT_NATS_CONNECT_TIMEOUT, HOST_RUNNER_STATE_DIR,
    failure_message, failure_summary,
};

const CLOUD_FOUNDER_ACTIVATION_ATTEMPTS: u32 = 60;
const CLOUD_FOUNDER_ACTIVATION_RETRY_DELAY: Duration = Duration::from_millis(500);

pub(super) fn run_interactive_cloud_bootstrap(cloud_host: &str) -> ExitCode {
    let Some(attempt) = prepare_cloud_bootstrap_attempt() else {
        return ExitCode::FAILURE;
    };
    let client = CloudClient::new(cloud_host);
    if let Some(exit_code) = resume_terminal_callback(&attempt, &client) {
        return exit_code;
    }

    let machine = cloud_machine_facts();
    let created = match client.post_json::<_, ployz_sdk_types::CloudBootstrapSessionCreated>(
        "/api/bootstrap/sessions",
        None,
        &CloudBootstrapSessionCreateRequest {
            attempt_id: attempt.attempt_id.clone(),
            client: CloudBootstrapClientInfo::current(env!("CARGO_PKG_VERSION")),
            machine: machine.clone(),
        },
    ) {
        Ok(created) => created,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };

    println!("Open this link to connect this machine:");
    println!("{}", created.browser_url);
    println!("Waiting for approval...");

    let poll_request = CloudBootstrapSessionPollRequest {
        attempt_id: attempt.attempt_id.clone(),
        session_secret: created.session_secret,
        machine,
    };
    run_cloud_bootstrap_decisions(
        &attempt,
        &client,
        CloudBootstrapFlow::InteractiveSession,
        Duration::from_secs(created.poll_after_seconds.into()),
        || {
            client.post_json::<_, CloudBootstrapDecision>(
                "/api/bootstrap/sessions/poll",
                None,
                &poll_request,
            )
        },
    )
}

pub(super) fn run_token_cloud_bootstrap(
    cloud_host: &str,
    cloud_token: CloudBootstrapToken,
) -> ExitCode {
    let Some(attempt) = prepare_cloud_bootstrap_attempt() else {
        return ExitCode::FAILURE;
    };
    let client = CloudClient::new(cloud_host);
    if let Some(exit_code) = resume_terminal_callback(&attempt, &client) {
        return exit_code;
    }

    let request = CloudBootstrapTokenRedeemRequest {
        attempt_id: attempt.attempt_id.clone(),
        client: CloudBootstrapClientInfo::current(env!("CARGO_PKG_VERSION")),
        machine: cloud_machine_facts(),
    };
    run_cloud_bootstrap_decisions(
        &attempt,
        &client,
        CloudBootstrapFlow::Token,
        Duration::ZERO,
        || {
            client.post_json::<_, CloudBootstrapDecision>(
                "/api/bootstrap/tokens/redeem",
                Some(cloud_token.secret()),
                &request,
            )
        },
    )
}

#[derive(Clone, Copy)]
enum CloudBootstrapFlow {
    InteractiveSession,
    Token,
}

fn run_cloud_bootstrap_decisions<E>(
    attempt: &CloudBootstrapAttemptState,
    client: &CloudClient,
    flow: CloudBootstrapFlow,
    mut retry_after: Duration,
    mut next_decision: impl FnMut() -> Result<CloudBootstrapDecision, E>,
) -> ExitCode
where
    E: std::fmt::Display,
{
    for _ in 0..CLOUD_BOOTSTRAP_MAX_POLLS {
        std::thread::sleep(retry_after);
        let decision = match next_decision() {
            Ok(decision) => decision,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };

        match decision {
            CloudBootstrapDecision::Pending {
                retry_after_seconds,
            } => retry_after = Duration::from_secs(retry_after_seconds.into()),
            CloudBootstrapDecision::Expired => return flow.expired(),
            CloudBootstrapDecision::Failed { failure } => {
                eprintln!("Cloud bootstrap failed: {failure:?}");
                return ExitCode::FAILURE;
            }
            CloudBootstrapDecision::Ready { envelope } => match &envelope.intent {
                CloudBootstrapIntent::WaitForFounder {
                    retry_after_seconds,
                } => {
                    println!("Waiting for the first machine to finish...");
                    retry_after = Duration::from_secs((*retry_after_seconds).into());
                }
                CloudBootstrapIntent::Founder { .. } | CloudBootstrapIntent::Joiner { .. } => {
                    return run_cloud_bootstrap_envelope(*envelope, attempt, client);
                }
            },
        }
    }

    flow.timed_out()
}

impl CloudBootstrapFlow {
    fn expired(self) -> ExitCode {
        match self {
            Self::InteractiveSession => {
                if let Err(error) = reset_cloud_attempt(std::path::Path::new(HOST_RUNNER_STATE_DIR))
                {
                    eprintln!(
                        "Cloud bootstrap session expired and local retry reset failed: {error}"
                    );
                    return ExitCode::FAILURE;
                }
                eprintln!("Cloud bootstrap session expired; rerun sudo ployz host bootstrap");
            }
            Self::Token => eprintln!("Cloud Bootstrap Token expired"),
        }
        ExitCode::FAILURE
    }

    fn timed_out(self) -> ExitCode {
        match self {
            Self::InteractiveSession => {
                eprintln!("Cloud bootstrap approval timed out; rerun sudo ployz host bootstrap");
            }
            Self::Token => {
                eprintln!("Cloud bootstrap timed out; rerun with a valid Cloud Bootstrap Token");
            }
        }
        ExitCode::FAILURE
    }
}

fn prepare_cloud_bootstrap_attempt() -> Option<CloudBootstrapAttemptState> {
    let state_dir = std::path::Path::new(HOST_RUNNER_STATE_DIR);
    let mut runner = SystemHostRunnerCommandRunner::default();
    let supervisor = match runner.read_os_release().and_then(|os_release| {
        crate::host_platform::detect_host_platform(&os_release)
            .map(|profile| crate::supervisor::SupervisorBackend::from(profile.supervisor()))
            .map_err(|error| {
                ployz_core::ops::FailureMessage::try_new(error.to_string())
                    .expect("host platform failure message is non-empty")
            })
    }) {
        Ok(supervisor) => supervisor,
        Err(message) => {
            eprintln!("failed to detect host platform: {}", message.as_str());
            return None;
        }
    };
    let supervisor_dirs = crate::supervisor::SupervisorDirectories::host_defaults();
    let local_state =
        match inspect_cloud_bootstrap_local_state(state_dir, supervisor_dirs.directory(supervisor))
        {
            Ok(state) => state,
            Err(error) => {
                eprintln!("{error}");
                return None;
            }
        };
    let attempt = match local_state {
        CloudBootstrapLocalState::AlreadyBootstrapped { evidence } => {
            match load_existing_cloud_attempt(state_dir) {
                Ok(Some(attempt)) if attempt.terminal.is_some() => {
                    println!("Resuming Cloud bootstrap callback for this machine...");
                    attempt
                }
                Ok(_) => {
                    eprintln!(
                        "this machine already has Ployz bootstrap evidence at {}; refusing before Cloud contact",
                        evidence.display()
                    );
                    return None;
                }
                Err(error) => {
                    eprintln!("{error}");
                    return None;
                }
            }
        }
        CloudBootstrapLocalState::Fresh | CloudBootstrapLocalState::PartialSameAttempt => {
            match load_or_create_cloud_attempt(state_dir) {
                Ok(attempt) => attempt,
                Err(error) => {
                    eprintln!("{error}");
                    return None;
                }
            }
        }
    };
    Some(attempt)
}

fn resume_terminal_callback(
    attempt: &CloudBootstrapAttemptState,
    client: &CloudClient,
) -> Option<ExitCode> {
    if let Some(terminal) = &attempt.terminal {
        return Some(
            match post_cloud_callback(client, &terminal.target, &terminal.callback) {
                Ok(()) => {
                    println!("Cloud bootstrap callback accepted.");
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::FAILURE
                }
            },
        );
    }
    None
}

fn run_cloud_bootstrap_envelope(
    envelope: CloudBootstrapEnvelope,
    attempt: &CloudBootstrapAttemptState,
    client: &CloudClient,
) -> ExitCode {
    if envelope.attempt_id != attempt.attempt_id {
        eprintln!(
            "Cloud bootstrap envelope attempt {} does not match local attempt {}",
            envelope.attempt_id.as_str(),
            attempt.attempt_id.as_str()
        );
        return ExitCode::FAILURE;
    }
    if let Err(message) = validate_cloud_envelope_callback_target(client, &envelope) {
        eprintln!("{message}");
        return ExitCode::FAILURE;
    }

    if let Some(terminal) = &attempt.terminal {
        return match post_cloud_callback(client, &terminal.target, &terminal.callback) {
            Ok(()) => {
                println!("Cloud bootstrap callback accepted.");
                ExitCode::SUCCESS
            }
            Err(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        };
    }

    match envelope.intent.clone() {
        CloudBootstrapIntent::Founder { founder } => {
            run_cloud_founder_bootstrap(*founder, &envelope, client)
        }
        CloudBootstrapIntent::Joiner { joiner } => {
            run_cloud_joiner_bootstrap(*joiner, &envelope, client)
        }
        CloudBootstrapIntent::WaitForFounder { .. } => {
            eprintln!("Cloud returned wait-for-founder where a bootstrap envelope was expected");
            ExitCode::FAILURE
        }
    }
}

fn validate_cloud_envelope_callback_target(
    client: &CloudClient,
    envelope: &CloudBootstrapEnvelope,
) -> Result<(), String> {
    if envelope.callback_token.secret().is_empty() {
        return Err("cloud callback token is empty".to_owned());
    }
    client
        .validate_same_origin_url(&envelope.callback_url)
        .map_err(|error| format!("cloud callback URL is invalid: {error}"))
}

fn run_cloud_founder_bootstrap(
    founder: CloudFounderBootstrap,
    envelope: &CloudBootstrapEnvelope,
    client: &CloudClient,
) -> ExitCode {
    let install = match build_cloud_founder_install_spec(&founder, envelope) {
        Ok(install) => install,
        Err(message) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::EnvelopeInvalid {
                    message: failure_message(&message),
                },
            );
        }
    };
    let machine_id = install.machine_id.clone();
    let runtime_nats_url = install.machine_join_runtime_nats_url.clone();
    let mut target = match crate::cli::first_machine_install_target_from_spec(install) {
        Ok(target) => target,
        Err(error) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::EnvelopeInvalid {
                    message: failure_message(&error.to_string()),
                },
            );
        }
    };
    target = target.with_additional_credential(CredentialGrant {
        public_key: founder.cloud_nats_user_public_key,
        name: CredentialName::try_new("Ployz Cloud").expect("Cloud credential name is non-empty"),
        role: CredentialRole::Operator,
    });
    let nats_material = target.nats_material.clone();

    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let execution = run_first_machine_install(target, &mut recorder);
    drop(recorder);

    let terminal = execution.terminal;
    let callback = match &terminal {
        HostRunnerPlanTerminal::Completed => {
            match activate_cloud_founder_machine(&machine_id, &runtime_nats_url, &nats_material) {
                Ok(()) => match read_cloud_founder_bootstrap_result(
                    &machine_id,
                    &runtime_nats_url,
                    &nats_material,
                ) {
                    Ok(result) => CloudBootstrapCallbackRequest {
                        attempt_id: envelope.attempt_id.clone(),
                        redemption_id: envelope.redemption_id.clone(),
                        outcome: CloudBootstrapOutcome::FounderSucceeded { result },
                    },
                    Err(message) => failed_callback(
                        envelope,
                        CloudBootstrapFailure::BootstrapFailed {
                            message: failure_message(&message),
                        },
                    ),
                },
                Err(message) => failed_callback(
                    envelope,
                    CloudBootstrapFailure::BootstrapFailed {
                        message: failure_message(&message),
                    },
                ),
            }
        }
        HostRunnerPlanTerminal::Failed(failure) => failed_callback(
            envelope,
            CloudBootstrapFailure::BootstrapFailed {
                message: failure_message(failure_summary(failure)),
            },
        ),
    };

    let post_result = persist_and_post_cloud_callback(envelope, client, callback);
    match (terminal, post_result) {
        (HostRunnerPlanTerminal::Completed, Ok(())) => ExitCode::SUCCESS,
        (HostRunnerPlanTerminal::Completed, Err(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        (HostRunnerPlanTerminal::Failed(failure), Ok(())) => {
            eprintln!(
                "ployz host cloud founder bootstrap failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
        (HostRunnerPlanTerminal::Failed(failure), Err(message)) => {
            eprintln!(
                "ployz host cloud founder bootstrap failed: {}; {message}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

fn activate_cloud_founder_machine(
    machine_id: &MachineId,
    runtime_nats_url: &MachineJoinRuntimeNatsUrl,
    material: &ployz_core::install::NatsMachineMaterialPaths,
) -> Result<(), String> {
    let controller_seed = std::fs::read_to_string(material.controller_seed_file())
        .map_err(|error| format!("failed to read controller seed: {error}"))?;
    let controller_seed = NatsUserSeed::try_new(controller_seed.trim())
        .map_err(|_| "controller seed is invalid".to_owned())?;
    let url = NatsClientUrl::try_new(runtime_nats_url.as_str().to_owned())
        .map_err(|error| format!("runtime NATS URL is invalid: {error}"))?;
    let connect = NatsConnectConfig {
        url,
        auth: NatsClientAuth::NkeySeed(controller_seed),
        trust: NatsTlsTrust::ClusterCa(material.ca_file()),
        principal: NatsPrincipal::Controller,
    };
    let request = InitFirstMachineActivateRequest {
        machine_id: machine_id.clone(),
        roles: ployz_core::roles::InstallRolePolicy::install_all(),
        automatic_hostname_configuration:
            ployz_core::ingress::AutomaticHostnameConfiguration::Ployz,
        ployz_dns_target: ployz_core::ingress::PloyzDnsTargetIntent::Enabled,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("failed to start async runtime: {error}"))?;

    runtime.block_on(async move {
        let mut last_error = String::new();
        for _ in 0..CLOUD_FOUNDER_ACTIVATION_ATTEMPTS {
            match connect_authenticated(&connect, DEFAULT_NATS_CONNECT_TIMEOUT).await {
                Ok(client) => {
                    match OperationApiClient::new(client)
                        .init_first_machine_activate(&request)
                        .await
                    {
                        Ok(_) => return Ok(()),
                        Err(error) => last_error = error.to_string(),
                    }
                }
                Err(error) => last_error = error.to_string(),
            }
            tokio::time::sleep(CLOUD_FOUNDER_ACTIVATION_RETRY_DELAY).await;
        }

        Err(format!("failed to activate first machine: {last_error}"))
    })
}

fn build_cloud_founder_install_spec(
    founder: &CloudFounderBootstrap,
    envelope: &CloudBootstrapEnvelope,
) -> Result<FirstMachineInstallSpec, String> {
    let manifest = load_release_manifest()?;
    let suffix = envelope
        .redemption_id
        .as_str()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    Ok(FirstMachineInstallSpec {
        machine_id: MachineId::try_new(format!("cloud_founder_{suffix}"))
            .map_err(|error| error.to_string())?,
        dataplane_endpoint_supernet: ployz_core::dataplane::MachineEndpointSupernet::default_v1(),
        gateway: GatewayRole::Install,
        host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
        machine_public_ip: Some(public_ip_from_runtime_nats_url(&founder.runtime_nats_url)?),
        machine_bootstrap_url: Some(
            MachineBootstrapUrl::try_new(DEFAULT_MACHINE_BOOTSTRAP_URL)
                .map_err(|error| error.to_string())?,
        ),
        machine_join_template_file: Some(
            AbsoluteInstallPath::try_new("/etc/ployz/machine-join-template.json")
                .map_err(|error| error.to_string())?,
        ),
        machine_join_cluster_name: MachineJoinClusterName::try_new("ployz")
            .map_err(|error| error.to_string())?,
        machine_join_runtime_nats_url: founder.runtime_nats_url.clone(),
        artifacts: manifest.install_artifacts()?,
    })
}

fn public_ip_from_runtime_nats_url(
    runtime_nats_url: &MachineJoinRuntimeNatsUrl,
) -> Result<IpAddr, String> {
    let authority = runtime_nats_url
        .as_str()
        .strip_prefix("tls://")
        .or_else(|| runtime_nats_url.as_str().strip_prefix("nats://"))
        .ok_or_else(|| "runtime NATS URL must start with tls:// or nats://".to_owned())?;
    let (host, _) = authority
        .rsplit_once(':')
        .ok_or_else(|| "runtime NATS URL must include a host and port".to_owned())?;
    host.trim_matches(['[', ']'])
        .parse()
        .map_err(|_| "Cloud founder runtime NATS URL must use a public IP host for v1".to_owned())
}

fn load_release_manifest() -> Result<ReleaseManifest, String> {
    let url = release_manifest_url();
    let contents = crate::release_manifest::read_release_manifest_text(&url)?;
    ReleaseManifest::parse(&contents)
}

fn release_manifest_url() -> String {
    std::env::var("PLOYZ_RELEASE_MANIFEST_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            persisted_release_manifest_url(std::path::Path::new("/etc/ployz/release.env")).ok()
        })
        .unwrap_or_else(default_release_manifest_url)
}

fn run_cloud_joiner_bootstrap(
    joiner: CloudJoinerBootstrap,
    envelope: &CloudBootstrapEnvelope,
    client: &CloudClient,
) -> ExitCode {
    let ca_file = PathBuf::from(HOST_RUNNER_STATE_DIR).join("cloud-bootstrap/trusted-nats-ca.pem");
    if let Err(error) = write_cloud_joiner_trusted_ca(&joiner, &ca_file) {
        return persist_post_failed_callback(
            envelope,
            client,
            CloudBootstrapFailure::EnvelopeInvalid {
                message: failure_message(&error.to_string()),
            },
        );
    }
    let connect = match cloud_joiner_connect_config(&joiner, ca_file) {
        Ok(connect) => connect,
        Err(error) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::EnvelopeInvalid {
                    message: failure_message(&error.to_string()),
                },
            );
        }
    };
    let join_token = match JoinToken::try_new(joiner.join_token.as_str()) {
        Ok(token) => token,
        Err(error) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::EnvelopeInvalid {
                    message: failure_message(&format!("invalid join token: {error:?}")),
                },
            );
        }
    };
    let mut redeemer = JoinRedeemer::new(Ok(connect.clone()));
    let mut reporter = JoinReporter::new(Ok(connect), join_token.clone());
    let mut token_consumer = CloudJoinTokenConsumer;
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            supervisor_dirs: crate::supervisor::SupervisorDirectories::host_defaults(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
            docker_daemon_config: "/etc/docker/daemon.json".into(),
            docker_repository_dir: "/etc/yum.repos.d".into(),
        },
        SystemHostRunnerCommandRunner::default(),
    );
    let stdout = std::io::stdout();
    let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
    let join_execution = execute_host_runner_join_with_redeemed(
        &join_token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        &mut recorder,
    );
    drop(recorder);

    let terminal = join_execution.execution.terminal;
    let callback = match (&terminal, &join_execution.redeemed) {
        (HostRunnerPlanTerminal::Completed, Some(redeemed)) => {
            let Some(callback_result) = &redeemed.callback_result else {
                return persist_post_failed_callback(
                    envelope,
                    client,
                    CloudBootstrapFailure::BootstrapFailed {
                        message: failure_message(
                            "join completed without redeemed material evidence",
                        ),
                    },
                );
            };
            cloud_joiner_success_callback(
                envelope.attempt_id.clone(),
                envelope.redemption_id.clone(),
                callback_result,
            )
        }
        (HostRunnerPlanTerminal::Completed, None) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::BootstrapFailed {
                    message: failure_message("join completed without redeemed material evidence"),
                },
            );
        }
        (HostRunnerPlanTerminal::Failed(failure), Some(redeemed)) => {
            let installed_success_callback =
                redeemed.callback_result.as_ref().map(|callback_result| {
                    cloud_joiner_success_callback(
                        envelope.attempt_id.clone(),
                        envelope.redemption_id.clone(),
                        callback_result,
                    )
                });
            cloud_joiner_failed_terminal_callback(envelope, failure, installed_success_callback)
        }
        (HostRunnerPlanTerminal::Failed(failure), None) => {
            cloud_joiner_failed_terminal_callback(envelope, failure, None)
        }
    };

    let post_result = persist_and_post_cloud_callback(envelope, client, callback);
    match (terminal, post_result) {
        (HostRunnerPlanTerminal::Completed, Ok(())) => ExitCode::SUCCESS,
        (HostRunnerPlanTerminal::Completed, Err(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        (HostRunnerPlanTerminal::Failed(failure), Ok(())) => {
            eprintln!(
                "ployz host cloud joiner bootstrap failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
        (HostRunnerPlanTerminal::Failed(failure), Err(message)) => {
            eprintln!(
                "ployz host cloud joiner bootstrap failed: {}; {message}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

fn is_join_report_failure(failure: &HostRunnerPlanFailure) -> bool {
    matches!(
        failure,
        HostRunnerPlanFailure::Step(step)
            if step.reason == HostRunnerStepFailureReason::JoinReportFailed
    )
}

fn cloud_joiner_failed_terminal_callback(
    envelope: &CloudBootstrapEnvelope,
    failure: &HostRunnerPlanFailure,
    installed_success_callback: Option<CloudBootstrapCallbackRequest>,
) -> CloudBootstrapCallbackRequest {
    if is_join_report_failure(failure)
        && let Some(callback) = installed_success_callback
    {
        return callback;
    }
    failed_callback(
        envelope,
        CloudBootstrapFailure::BootstrapFailed {
            message: failure_message(failure_summary(failure)),
        },
    )
}

fn persist_post_failed_callback(
    envelope: &CloudBootstrapEnvelope,
    client: &CloudClient,
    failure: CloudBootstrapFailure,
) -> ExitCode {
    match persist_and_post_cloud_callback(envelope, client, failed_callback(envelope, failure)) {
        Ok(()) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn persist_and_post_cloud_callback(
    envelope: &CloudBootstrapEnvelope,
    client: &CloudClient,
    callback: CloudBootstrapCallbackRequest,
) -> Result<(), String> {
    persist_cloud_terminal_callback(
        std::path::Path::new(HOST_RUNNER_STATE_DIR),
        envelope.clone(),
        callback.clone(),
    )
    .map_err(|error| error.to_string())?;
    let target = CloudBootstrapCallbackTarget::from_envelope(envelope);
    post_cloud_callback(client, &target, &callback)
}

fn post_cloud_callback(
    client: &CloudClient,
    target: &CloudBootstrapCallbackTarget,
    callback: &CloudBootstrapCallbackRequest,
) -> Result<(), String> {
    if callback.attempt_id != target.attempt_id {
        return Err(format!(
            "cloud callback attempt {} does not match envelope attempt {}",
            callback.attempt_id.as_str(),
            target.attempt_id.as_str()
        ));
    }
    if callback.redemption_id != target.redemption_id {
        return Err(format!(
            "cloud callback redemption {} does not match envelope redemption {}",
            callback.redemption_id.as_str(),
            target.redemption_id.as_str()
        ));
    }
    let _accepted = client
        .post_json_to_url::<_, CloudBootstrapCallbackAccepted>(
            &target.callback_url,
            Some(target.callback_token.secret()),
            callback,
        )
        .map_err(|error| format!("failed to post Cloud bootstrap callback: {error}"))?;
    Ok(())
}

fn failed_callback(
    envelope: &CloudBootstrapEnvelope,
    failure: CloudBootstrapFailure,
) -> CloudBootstrapCallbackRequest {
    CloudBootstrapCallbackRequest {
        attempt_id: envelope.attempt_id.clone(),
        redemption_id: envelope.redemption_id.clone(),
        outcome: CloudBootstrapOutcome::Failed { failure },
    }
}

fn cloud_machine_facts() -> CloudBootstrapMachineFacts {
    CloudBootstrapMachineFacts {
        hostname: gethostname::gethostname().into_string().ok(),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        candidate_runtime_nats_url: std::env::var("PLOYZ_CANDIDATE_RUNTIME_NATS_URL")
            .ok()
            .and_then(|value| MachineJoinRuntimeNatsUrl::try_new(value).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cloud_joiner_failed_terminal_callback, persisted_release_manifest_url,
        public_ip_from_runtime_nats_url,
    };
    use crate::executor::HostRunnerPlanFailure;
    use crate::steps::{HostRunnerStepFailure, HostRunnerStepFailureReason, HostRunnerStepLabel};
    use ployz_core::install::MachineJoinRuntimeNatsUrl;
    use ployz_core::ops::FailureMessage;
    use ployz_sdk_types::{
        CloudBootstrapAttemptId, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
        CloudBootstrapEnvelope, CloudBootstrapIntent, CloudBootstrapOutcome,
        CloudBootstrapRedemptionId,
    };

    #[test]
    fn persisted_release_env_supplies_manifest_url() {
        let path = std::env::temp_dir().join(format!(
            "ployz-release-env-{}-{}.env",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after unix epoch")
                .as_nanos()
        ));
        std::fs::write(
            &path,
            "PLOYZ_RELEASE_MANIFEST_URL=https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.7/ployz-release-linux-amd64.env\n",
        )
        .expect("release env can be written");

        assert_eq!(
            persisted_release_manifest_url(&path).expect("manifest URL loads"),
            "https://github.com/getployz/ployz/releases/download/v0.0.2-alpha.7/ployz-release-linux-amd64.env"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn founder_public_ip_comes_from_runtime_nats_url() {
        let url =
            MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222").expect("valid nats URL");

        assert_eq!(
            public_ip_from_runtime_nats_url(&url)
                .expect("public IP parses")
                .to_string(),
            "203.0.113.10"
        );
    }

    #[test]
    fn founder_public_ipv6_comes_from_runtime_nats_url() {
        let url =
            MachineJoinRuntimeNatsUrl::try_new("tls://[2001:db8::1]:4222").expect("valid nats URL");

        assert_eq!(
            public_ip_from_runtime_nats_url(&url)
                .expect("public IP parses")
                .to_string(),
            "2001:db8::1"
        );
    }

    #[test]
    fn founder_runtime_nats_url_rejects_hostname_for_v1() {
        let url =
            MachineJoinRuntimeNatsUrl::try_new("tls://core.example.com:4222").expect("valid URL");

        assert!(public_ip_from_runtime_nats_url(&url).is_err());
    }

    #[test]
    fn join_report_failure_preserves_success_callback_when_join_was_installed() {
        let envelope = CloudBootstrapEnvelope {
            attempt_id: CloudBootstrapAttemptId::try_new("pcba_123").expect("valid attempt id"),
            redemption_id: CloudBootstrapRedemptionId::try_new("pcbr_123")
                .expect("valid redemption id"),
            callback_url: "https://cloud.example.com/api/bootstrap/redemptions/pcbr_123/callback"
                .to_owned(),
            callback_token: CloudBootstrapCallbackToken::try_new("pcbc_123")
                .expect("valid callback token"),
            intent: CloudBootstrapIntent::WaitForFounder {
                retry_after_seconds: 1,
            },
        };
        let failure = HostRunnerPlanFailure::Step(HostRunnerStepFailure {
            step: HostRunnerStepLabel::ReportJoinResult,
            reason: HostRunnerStepFailureReason::JoinReportFailed,
            message: FailureMessage::try_new("report failed").expect("valid message"),
        });

        let success_callback = CloudBootstrapCallbackRequest {
            attempt_id: envelope.attempt_id.clone(),
            redemption_id: envelope.redemption_id.clone(),
            outcome: CloudBootstrapOutcome::JoinerSucceeded {
                result: ployz_sdk_types::CloudJoinerBootstrapResult {
                    operation_id: ployz_core::ids::OperationId::try_new("op_machine")
                        .expect("valid operation id"),
                    machine_id: ployz_core::ids::MachineId::try_new("machine_2")
                        .expect("valid machine id"),
                    name: ployz_core::machine::MachineName::try_new("edge_2")
                        .expect("valid machine name"),
                    last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                        .expect("valid sequence"),
                    result: ployz_sdk_types::MachineJoinRedeemResult::Joined,
                },
            },
        };

        let callback =
            cloud_joiner_failed_terminal_callback(&envelope, &failure, Some(success_callback));

        assert!(matches!(
            callback.outcome,
            CloudBootstrapOutcome::JoinerSucceeded { .. }
        ));
    }
}
