use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ployz_core::install::{
    AbsoluteInstallPath, DEFAULT_MACHINE_BOOTSTRAP_URL, FirstMachineInstallArtifacts,
    FirstMachineInstallSpec, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
    InstallSha256Digest, MachineBootstrapUrl, MachineJoinClusterName, MachineJoinRuntimeNatsUrl,
    NatsServerInstallSpec,
};
use ployz_core::roles::{DnsRole, GatewayRole};
use ployz_keeper::cloud_bootstrap::{
    CloudBootstrapAttemptState, CloudBootstrapCallbackTarget, CloudBootstrapLocalState,
    cloud_joiner_connect_config, cloud_joiner_success_callback,
    inspect_cloud_bootstrap_local_state, load_existing_cloud_attempt, load_or_create_cloud_attempt,
    persist_cloud_terminal_callback, reset_cloud_attempt, write_cloud_joiner_trusted_ca,
};
use ployz_keeper::cloud_client::{CloudClient, get_text_url};
use ployz_keeper::command::SystemKeeperCommandRunner;
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal};
use ployz_keeper::join_executor::execute_keeper_join_with_redeemed;
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{JoinToken, KeeperStepFailureReason};
use ployz_sdk_types::{
    CloudBootstrapCallbackAccepted, CloudBootstrapCallbackRequest, CloudBootstrapClientInfo,
    CloudBootstrapDecision, CloudBootstrapEnvelope, CloudBootstrapFailure, CloudBootstrapIntent,
    CloudBootstrapMachineFacts, CloudBootstrapOutcome, CloudBootstrapSessionCreateRequest,
    CloudBootstrapSessionPollRequest, CloudFounderBootstrap, CloudJoinerBootstrap, MachineId,
};

use super::{
    CLOUD_BOOTSTRAP_MAX_POLLS, CloudJoinTokenConsumer, JoinRedeemer, JoinReporter,
    KEEPER_STATE_DIR, failure_message, failure_summary, load_machine_public_ip_from_env,
    read_cloud_founder_bootstrap_result, run_first_machine_install,
};

pub(super) fn run_interactive_cloud_bootstrap(cloud_host: &str) -> ExitCode {
    let state_dir = std::path::Path::new(KEEPER_STATE_DIR);
    let systemd_dir = std::path::Path::new("/etc/systemd/system");
    let local_state = match inspect_cloud_bootstrap_local_state(state_dir, systemd_dir) {
        Ok(state) => state,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
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
                    return ExitCode::FAILURE;
                }
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            }
        }
        CloudBootstrapLocalState::Fresh | CloudBootstrapLocalState::PartialSameAttempt => {
            match load_or_create_cloud_attempt(state_dir) {
                Ok(attempt) => attempt,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };
    let client = CloudClient::new(cloud_host);
    if let Some(terminal) = &attempt.terminal {
        return match post_cloud_callback(&client, &terminal.target, &terminal.callback) {
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

    let mut poll_after_seconds = created.poll_after_seconds;
    for _ in 0..CLOUD_BOOTSTRAP_MAX_POLLS {
        std::thread::sleep(Duration::from_secs(poll_after_seconds.into()));
        let decision = match client.post_json::<_, CloudBootstrapDecision>(
            "/api/bootstrap/sessions/poll",
            None,
            &CloudBootstrapSessionPollRequest {
                attempt_id: attempt.attempt_id.clone(),
                session_secret: created.session_secret.clone(),
                machine: machine.clone(),
            },
        ) {
            Ok(decision) => decision,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::FAILURE;
            }
        };

        if update_poll_delay_from_pending(&decision, &mut poll_after_seconds) {
            continue;
        }
        match decision {
            CloudBootstrapDecision::Pending { .. } => continue,
            CloudBootstrapDecision::Expired => {
                if let Err(error) = reset_cloud_attempt(std::path::Path::new(KEEPER_STATE_DIR)) {
                    eprintln!(
                        "Cloud bootstrap session expired and local retry reset failed: {error}"
                    );
                    return ExitCode::FAILURE;
                }
                eprintln!("Cloud bootstrap session expired; rerun sudo ployz-keeper bootstrap");
                return ExitCode::FAILURE;
            }
            CloudBootstrapDecision::Failed { failure } => {
                eprintln!("Cloud bootstrap failed: {failure:?}");
                return ExitCode::FAILURE;
            }
            CloudBootstrapDecision::Ready { envelope } => match &envelope.intent {
                CloudBootstrapIntent::WaitForFounder {
                    retry_after_seconds,
                } => {
                    println!("Waiting for the first machine to finish...");
                    std::thread::sleep(Duration::from_secs((*retry_after_seconds).into()));
                }
                CloudBootstrapIntent::Founder { .. } | CloudBootstrapIntent::Joiner { .. } => {
                    return run_cloud_bootstrap_envelope(*envelope, &attempt, &client);
                }
            },
        }
    }

    eprintln!("Cloud bootstrap approval timed out; rerun sudo ployz-keeper bootstrap");
    ExitCode::FAILURE
}

fn update_poll_delay_from_pending(
    decision: &CloudBootstrapDecision,
    poll_after_seconds: &mut u16,
) -> bool {
    match decision {
        CloudBootstrapDecision::Pending {
            retry_after_seconds,
        } => {
            *poll_after_seconds = *retry_after_seconds;
            true
        }
        CloudBootstrapDecision::Ready { .. }
        | CloudBootstrapDecision::Expired
        | CloudBootstrapDecision::Failed { .. } => false,
    }
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
    let mut target = match ployz_keeper::cli::first_machine_install_target_from_spec(install) {
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
    target = target.with_additional_user_public_key(founder.cloud_nats_user_public_key);
    let nats_material = target.nats_material.clone();

    let stdout = std::io::stdout();
    let mut recorder = KeeperTextRecorder::new(stdout.lock());
    let execution = run_first_machine_install(target, &mut recorder);
    drop(recorder);

    let terminal = execution.terminal;
    let callback = match &terminal {
        KeeperPlanTerminal::Completed => match read_cloud_founder_bootstrap_result(
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
        KeeperPlanTerminal::Failed(failure) => failed_callback(
            envelope,
            CloudBootstrapFailure::BootstrapFailed {
                message: failure_message(failure_summary(failure)),
            },
        ),
    };

    let post_result = persist_and_post_cloud_callback(envelope, client, callback);
    match (terminal, post_result) {
        (KeeperPlanTerminal::Completed, Ok(())) => ExitCode::SUCCESS,
        (KeeperPlanTerminal::Completed, Err(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        (KeeperPlanTerminal::Failed(failure), Ok(())) => {
            eprintln!(
                "ployz-keeper cloud founder bootstrap failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
        (KeeperPlanTerminal::Failed(failure), Err(message)) => {
            eprintln!(
                "ployz-keeper cloud founder bootstrap failed: {}; {message}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
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
        gateway: GatewayRole::Install,
        dns: DnsRole::Install,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReleaseManifest {
    version: String,
    ployzd_url: String,
    ployzd_sha256: String,
    ebpf_tc_url: String,
    ebpf_tc_sha256: String,
    ebpf_ctl_url: String,
    ebpf_ctl_sha256: String,
    nats_server_version: String,
    nats_server_url: String,
    nats_server_sha256: String,
}

impl ReleaseManifest {
    fn parse(contents: &str) -> Result<Self, String> {
        Ok(Self {
            version: manifest_value(contents, "PLOYZ_VERSION")?,
            ployzd_url: manifest_value(contents, "PLOYZD_URL")?,
            ployzd_sha256: manifest_value(contents, "PLOYZD_SHA256")?,
            ebpf_tc_url: manifest_value(contents, "PLOYZ_EBPF_TC_URL")?,
            ebpf_tc_sha256: manifest_value(contents, "PLOYZ_EBPF_TC_SHA256")?,
            ebpf_ctl_url: manifest_value(contents, "PLOYZ_EBPF_CTL_URL")?,
            ebpf_ctl_sha256: manifest_value(contents, "PLOYZ_EBPF_CTL_SHA256")?,
            nats_server_version: manifest_value(contents, "PLOYZ_NATS_SERVER_VERSION")?,
            nats_server_url: manifest_value(contents, "PLOYZ_NATS_SERVER_URL")?,
            nats_server_sha256: manifest_value(contents, "PLOYZ_NATS_SERVER_SHA256")?,
        })
    }

    fn install_artifacts(&self) -> Result<FirstMachineInstallArtifacts, String> {
        Ok(FirstMachineInstallArtifacts {
            ployzd: artifact_spec(
                &self.version,
                &self.ployzd_url,
                &self.ployzd_sha256,
                "/usr/local/bin/ployzd",
            )?,
            ebpf_bytecode: artifact_spec(
                &self.version,
                &self.ebpf_tc_url,
                &self.ebpf_tc_sha256,
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            )?,
            ebpf_ctl: artifact_spec(
                &self.version,
                &self.ebpf_ctl_url,
                &self.ebpf_ctl_sha256,
                "/usr/local/bin/ployz-ebpf-ctl",
            )?,
            nats_server: NatsServerInstallSpec {
                version: InstallArtifactVersion::try_new(&self.nats_server_version)
                    .map_err(|error| error.to_string())?,
                source: InstallArtifactSource::try_new(&self.nats_server_url)
                    .map_err(|error| error.to_string())?,
                sha256: InstallSha256Digest::try_new(&self.nats_server_sha256)
                    .map_err(|error| error.to_string())?,
                binary: AbsoluteInstallPath::try_new("/usr/local/bin/nats-server")
                    .map_err(|error| error.to_string())?,
                config: AbsoluteInstallPath::try_new("/etc/nats/nats-server.conf")
                    .map_err(|error| error.to_string())?,
            },
        })
    }
}

fn artifact_spec(
    version: &str,
    source: &str,
    sha256: &str,
    install_path: &str,
) -> Result<InstallArtifactSpec, String> {
    Ok(InstallArtifactSpec {
        version: InstallArtifactVersion::try_new(version).map_err(|error| error.to_string())?,
        source: InstallArtifactSource::try_new(source).map_err(|error| error.to_string())?,
        sha256: InstallSha256Digest::try_new(sha256).map_err(|error| error.to_string())?,
        install_path: AbsoluteInstallPath::try_new(install_path)
            .map_err(|error| error.to_string())?,
    })
}

fn load_release_manifest() -> Result<ReleaseManifest, String> {
    let url = release_manifest_url();
    let contents = get_text_url(&url)
        .map_err(|error| format!("failed to download release manifest {url}: {error}"))?;
    ReleaseManifest::parse(&contents)
}

fn release_manifest_url() -> String {
    std::env::var("PLOYZ_RELEASE_MANIFEST_URL")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            persisted_release_manifest_url(std::path::Path::new("/etc/ployz/release.env")).ok()
        })
        .unwrap_or_else(|| {
            format!(
                "https://github.com/getployz/ployz/releases/download/v{}/ployz-release-{}.env",
                env!("CARGO_PKG_VERSION"),
                release_platform()
            )
        })
}

fn persisted_release_manifest_url(path: &std::path::Path) -> Result<String, String> {
    let contents = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    manifest_value(&contents, "PLOYZ_RELEASE_MANIFEST_URL")
}

fn release_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-amd64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "arm") => "linux-arm64",
        _ => "unsupported",
    }
}

fn manifest_value(contents: &str, key: &str) -> Result<String, String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("release manifest is missing {key}"))
}

#[cfg(test)]
mod tests {
    use super::{
        ReleaseManifest, cloud_joiner_failed_terminal_callback, persisted_release_manifest_url,
        public_ip_from_runtime_nats_url, update_poll_delay_from_pending,
    };
    use ployz_core::install::MachineJoinRuntimeNatsUrl;
    use ployz_core::ops::FailureMessage;
    use ployz_keeper::executor::KeeperPlanFailure;
    use ployz_keeper::steps::{KeeperStepFailure, KeeperStepFailureReason, KeeperStepLabel};
    use ployz_sdk_types::{
        CloudBootstrapAttemptId, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
        CloudBootstrapEnvelope, CloudBootstrapIntent, CloudBootstrapOutcome,
        CloudBootstrapRedemptionId,
    };

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn release_manifest_builds_founder_artifacts_in_keeper() {
        let manifest = ReleaseManifest::parse(&format!(
            "PLOYZ_VERSION=0.1.0\n\
             PLOYZD_URL=https://example.test/ployzd\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_NATS_SERVER_VERSION=2.14.2\n\
             PLOYZ_NATS_SERVER_URL=https://example.test/nats-server\n\
             PLOYZ_NATS_SERVER_SHA256={SHA}\n"
        ))
        .expect("manifest parses");
        let artifacts = manifest.install_artifacts().expect("artifacts build");

        assert_eq!(
            artifacts.ployzd.install_path.as_str(),
            "/usr/local/bin/ployzd"
        );
        assert_eq!(
            artifacts.nats_server.binary.as_str(),
            "/usr/local/bin/nats-server"
        );
    }

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
    fn pending_decision_updates_next_poll_delay() {
        let mut poll_after_seconds = 2;
        let decision = ployz_sdk_types::CloudBootstrapDecision::Pending {
            retry_after_seconds: 9,
        };

        assert!(update_poll_delay_from_pending(
            &decision,
            &mut poll_after_seconds
        ));
        assert_eq!(poll_after_seconds, 9);
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
        let failure = KeeperPlanFailure::Step(KeeperStepFailure {
            step: KeeperStepLabel::ReportJoinResult,
            reason: KeeperStepFailureReason::JoinReportFailed,
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

fn run_cloud_joiner_bootstrap(
    joiner: CloudJoinerBootstrap,
    envelope: &CloudBootstrapEnvelope,
    client: &CloudClient,
) -> ExitCode {
    let ca_file = PathBuf::from(KEEPER_STATE_DIR).join("cloud-bootstrap/trusted-nats-ca.pem");
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
    let machine_public_ip = match load_machine_public_ip_from_env() {
        Ok(public_ip) => public_ip,
        Err(error) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::BootstrapFailed {
                    message: failure_message(&error.to_string()),
                },
            );
        }
    };

    let mut redeemer = JoinRedeemer::new(Ok(connect.clone()), Ok(machine_public_ip));
    let mut reporter = JoinReporter::new(Ok(connect), join_token.clone());
    let mut token_consumer = CloudJoinTokenConsumer;
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: KEEPER_STATE_DIR.into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    let stdout = std::io::stdout();
    let mut recorder = KeeperTextRecorder::new(stdout.lock());
    let join_execution = execute_keeper_join_with_redeemed(
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
        (KeeperPlanTerminal::Completed, Some(redeemed)) => {
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
        (KeeperPlanTerminal::Completed, None) => {
            return persist_post_failed_callback(
                envelope,
                client,
                CloudBootstrapFailure::BootstrapFailed {
                    message: failure_message("join completed without redeemed material evidence"),
                },
            );
        }
        (KeeperPlanTerminal::Failed(failure), Some(redeemed)) => {
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
        (KeeperPlanTerminal::Failed(failure), None) => {
            cloud_joiner_failed_terminal_callback(envelope, failure, None)
        }
    };

    let post_result = persist_and_post_cloud_callback(envelope, client, callback);
    match (terminal, post_result) {
        (KeeperPlanTerminal::Completed, Ok(())) => ExitCode::SUCCESS,
        (KeeperPlanTerminal::Completed, Err(message)) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
        (KeeperPlanTerminal::Failed(failure), Ok(())) => {
            eprintln!(
                "ployz-keeper cloud joiner bootstrap failed: {}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
        (KeeperPlanTerminal::Failed(failure), Err(message)) => {
            eprintln!(
                "ployz-keeper cloud joiner bootstrap failed: {}; {message}",
                failure_summary(&failure)
            );
            ExitCode::FAILURE
        }
    }
}

fn is_join_report_failure(failure: &KeeperPlanFailure) -> bool {
    matches!(
        failure,
        KeeperPlanFailure::Step(step)
            if step.reason == KeeperStepFailureReason::JoinReportFailed
    )
}

fn cloud_joiner_failed_terminal_callback(
    envelope: &CloudBootstrapEnvelope,
    failure: &KeeperPlanFailure,
    installed_success_callback: Option<CloudBootstrapCallbackRequest>,
) -> CloudBootstrapCallbackRequest {
    if is_join_report_failure(failure) {
        if let Some(callback) = installed_success_callback {
            return callback;
        }
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
        std::path::Path::new(KEEPER_STATE_DIR),
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
