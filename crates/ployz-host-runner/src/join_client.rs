use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use crate::artifacts::{ArtifactKind, DataplaneArtifactTargets, artifact_target};
use crate::cli::HostRunnerStartup;
use crate::command::SystemHostRunnerCommandRunner;
use crate::executor::HostRunnerPlanTerminal;
use crate::join::JOIN_MATERIAL_DIR;
use crate::join_executor::{
    HostRunnerJoinRedeemer, HostRunnerJoinReporter, HostRunnerJoinTokenConsumer,
    RedeemedHostRunnerJoin, execute_host_runner_join,
};
use crate::local::{HostRunnerLocalConfig, HostRunnerLocalEffects};
use crate::report::HostRunnerTextRecorder;
use crate::steps::{
    HostRunnerJoinMaterial, HostRunnerJoinTarget, JoinToken, NonEmptyRoleSet,
    PloyzdRoleEnvironmentTarget, RoleNatsCredentials,
};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::ops::FailureMessage;
use ployz_core::roles::plan_joined_machine_process_set;
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsClientUrlError, NatsConnectConfig, NatsTlsTrust,
    connect_authenticated,
};
use ployz_nats::operation_api_client::{OperationApiClient, OperationApiClientError};
use ployz_sdk_types::{
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemed,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReportedOutcome,
    MachineJoinToken,
};

use crate::runtime::{
    DEFAULT_NATS_CONNECT_TIMEOUT, HOST_RUNNER_STATE_DIR, PLOYZ_JOIN_NKEY_SEED_ENV,
    PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_URL_ENV, REDEEM_MATERIAL_ATTEMPTS,
    REDEEM_MATERIAL_RETRY_DELAY, failure_message, failure_summary,
};

const JOIN_REPORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub(crate) fn run_start_command(startup: HostRunnerStartup) -> ExitCode {
    if let Some(join) = &startup.join {
        let stdout = std::io::stdout();
        let mut recorder = HostRunnerTextRecorder::new(stdout.lock());
        let execution = run_startup_join(&join.token, join.file.clone(), &mut recorder);
        match execution.terminal {
            HostRunnerPlanTerminal::Completed => ExitCode::SUCCESS,
            HostRunnerPlanTerminal::Failed(failure) => {
                eprintln!("ployz host join failed: {}", failure_summary(&failure));
                ExitCode::FAILURE
            }
        }
    } else {
        println!("ployz host started");
        ExitCode::SUCCESS
    }
}

pub(crate) fn run_startup_join(
    token: &JoinToken,
    join_token_file: std::path::PathBuf,
    recorder: &mut impl crate::executor::HostRunnerStepRecorder,
) -> crate::executor::HostRunnerPlanExecution {
    run_join_with_consumer(
        token,
        StartupJoinTokenConsumer { join_token_file },
        recorder,
    )
}

pub(crate) fn run_join_with_consumer(
    token: &JoinToken,
    mut token_consumer: impl HostRunnerJoinTokenConsumer,
    recorder: &mut impl crate::executor::HostRunnerStepRecorder,
) -> crate::executor::HostRunnerPlanExecution {
    let mut redeemer = JoinRedeemer::from_env();
    let mut reporter = JoinReporter::from_env(token.clone());
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
            docker_daemon_config: "/etc/docker/daemon.json".into(),
        },
        SystemHostRunnerCommandRunner::default(),
    );
    execute_host_runner_join(
        token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        recorder,
    )
}

pub(crate) struct JoinRedeemer {
    connect: Result<NatsConnectConfig, FailureMessage>,
}

impl JoinRedeemer {
    pub(crate) fn new(connect: Result<NatsConnectConfig, FailureMessage>) -> Self {
        Self { connect }
    }

    pub(crate) fn from_env() -> Self {
        Self::new(
            load_join_connect_from_env().map_err(|error| failure_message(&format!("{error}"))),
        )
    }
}

impl HostRunnerJoinRedeemer for JoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedHostRunnerJoin, FailureMessage> {
        let connect = self.connect.clone()?;
        let join_token = MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        let redeemed = runtime.block_on(async move {
            let client = connect_authenticated(&connect, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            redeem_until_material_ready(&OperationApiClient::new(client), join_token).await
        })?;

        host_runner_join_target(redeemed)
    }
}

/// Redeems the join token, retrying boundedly while the core's mint worker
/// has not reached `material-ready` yet. Any other failure is terminal.
async fn redeem_until_material_ready(
    api: &OperationApiClient,
    join_token: MachineJoinToken,
) -> Result<MachineJoinRedeemed, FailureMessage> {
    let mut last_not_ready = String::new();
    for _ in 0..REDEEM_MATERIAL_ATTEMPTS {
        match api
            .machine_join_redeem(&MachineJoinRedeemRequest {
                join_token: join_token.clone(),
            })
            .await
        {
            Ok(redeemed) => return Ok(redeemed),
            Err(OperationApiClientError::Domain {
                error: MachineJoinRedeemError::MaterialNotReady { operation_id },
                ..
            }) => {
                last_not_ready = format!(
                    "operation {} has not reached material-ready",
                    operation_id.as_str()
                );
                tokio::time::sleep(REDEEM_MATERIAL_RETRY_DELAY).await;
            }
            Err(error) => {
                return Err(failure_message(&format!(
                    "failed to redeem join token: {error}"
                )));
            }
        }
    }

    Err(failure_message(&format!(
        "join material did not become ready within {REDEEM_MATERIAL_ATTEMPTS} attempts: {last_not_ready}"
    )))
}

pub(crate) struct JoinReporter {
    connect: Result<NatsConnectConfig, FailureMessage>,
    join_token: JoinToken,
}

impl JoinReporter {
    pub(crate) fn new(
        connect: Result<NatsConnectConfig, FailureMessage>,
        join_token: JoinToken,
    ) -> Self {
        Self {
            connect,
            join_token,
        }
    }

    fn from_env(join_token: JoinToken) -> Self {
        Self::new(
            load_join_connect_from_env().map_err(|error| failure_message(&format!("{error}"))),
            join_token,
        )
    }
}

impl HostRunnerJoinReporter for JoinReporter {
    fn report_join_completed(&mut self) -> Result<(), FailureMessage> {
        self.report_join_result(MachineJoinReportRequest {
            join_token: self.machine_join_token()?,
            outcome: MachineJoinReportOutcome::Completed,
        })
    }

    fn report_join_failed(
        &mut self,
        failure: ployz_sdk_types::MachineJoinReportFailure,
    ) -> Result<(), FailureMessage> {
        self.report_join_result(MachineJoinReportRequest {
            join_token: self.machine_join_token()?,
            outcome: MachineJoinReportOutcome::Failed { failure },
        })
    }
}

impl JoinReporter {
    fn report_join_result(&self, request: MachineJoinReportRequest) -> Result<(), FailureMessage> {
        let connect = self.connect.clone()?;
        let reported_completion = matches!(request.outcome, MachineJoinReportOutcome::Completed);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        runtime.block_on(async move {
            let client = connect_authenticated(&connect, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            let reported = OperationApiClient::new(client)
                .with_request_timeout(JOIN_REPORT_REQUEST_TIMEOUT)
                .machine_join_report(&request)
                .await
                .map_err(|error| {
                    failure_message(&format!("failed to report join result: {error}"))
                })?;
            match reported.outcome {
                MachineJoinReportedOutcome::Failed { failure } if reported_completion => Err(
                    failure_message(&format!("machine join rejected by core: {failure:?}")),
                ),
                MachineJoinReportedOutcome::Completed
                | MachineJoinReportedOutcome::Failed { .. } => Ok(()),
            }
        })
    }

    fn machine_join_token(&self) -> Result<MachineJoinToken, FailureMessage> {
        let token = self.join_token.clone();
        MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))
    }
}

pub(crate) struct CloudJoinTokenConsumer;

impl HostRunnerJoinTokenConsumer for CloudJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        Ok(())
    }
}

/// Builds the Host Runner's Join-credential connection: TLS against the cluster
/// CA file plus the deliberately low-privilege Join seed, both delivered by
/// the install command env.
fn load_join_connect_from_env() -> Result<NatsConnectConfig, HostRunnerNatsConnectError> {
    let url =
        std::env::var(PLOYZ_NATS_URL_ENV).map_err(|_| HostRunnerNatsConnectError::MissingUrl)?;
    let url = NatsClientUrl::try_new(url.clone())
        .map_err(|source| HostRunnerNatsConnectError::InvalidUrl { value: url, source })?;
    let ca_file = std::env::var(PLOYZ_NATS_CA_FILE_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(HostRunnerNatsConnectError::MissingCaFile)?;
    let seed = std::env::var(PLOYZ_JOIN_NKEY_SEED_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or(HostRunnerNatsConnectError::MissingJoinSeed)?;
    let seed = NatsUserSeed::try_new(seed.trim())
        .map_err(|_| HostRunnerNatsConnectError::InvalidJoinSeed)?;

    Ok(NatsConnectConfig {
        url,
        auth: NatsClientAuth::NkeySeed(seed),
        trust: NatsTlsTrust::ClusterCa(ca_file),
        principal: NatsPrincipal::Join,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum HostRunnerNatsConnectError {
    #[error("{PLOYZ_NATS_URL_ENV} is required")]
    MissingUrl,
    #[error("{PLOYZ_NATS_URL_ENV}={value:?} is invalid")]
    InvalidUrl {
        value: String,
        #[source]
        source: NatsClientUrlError,
    },
    #[error("{PLOYZ_NATS_CA_FILE_ENV} is required")]
    MissingCaFile,
    #[error("{PLOYZ_JOIN_NKEY_SEED_ENV} is required")]
    MissingJoinSeed,
    #[error("{PLOYZ_JOIN_NKEY_SEED_ENV} must be an SU-prefixed user seed")]
    InvalidJoinSeed,
}

fn host_runner_join_target(
    redeemed: MachineJoinRedeemed,
) -> Result<RedeemedHostRunnerJoin, FailureMessage> {
    let callback_result = redeemed.clone();
    let machine_id = redeemed.machine_id.clone();
    let material = HostRunnerJoinMaterial::from_join_payload(
        machine_id.clone(),
        &redeemed.join_bundle,
        &redeemed.secret_delivery,
    )
    .map_err(|error| failure_message(&format!("invalid join material: {error:?}")))?;
    let ployzd_artifact =
        artifact_target(ArtifactKind::Ployzd, &redeemed.join_bundle.material.ployzd)
            .map_err(|error| failure_message(&format!("invalid ployzd install target: {error}")))?;
    let ebpf_bytecode_artifact = artifact_target(
        ArtifactKind::EbpfBytecode,
        &redeemed.join_bundle.material.ebpf_bytecode,
    )
    .map_err(|error| failure_message(&format!("invalid eBPF bytecode install target: {error}")))?;
    let ebpf_ctl_artifact = artifact_target(
        ArtifactKind::EbpfCtl,
        &redeemed.join_bundle.material.ebpf_ctl,
    )
    .map_err(|error| failure_message(&format!("invalid eBPF ctl install target: {error}")))?;
    let roles = NonEmptyRoleSet::try_new(
        plan_joined_machine_process_set(&machine_id, redeemed.roles)
            .roles()
            .to_vec(),
    )
    .map_err(|error| failure_message(&format!("invalid joined machine role set: {error:?}")))?;
    let runtime_nats_client_url =
        NatsClientUrl::try_new(redeemed.join_bundle.material.runtime_nats_url.as_str())
            .map_err(|error| failure_message(&format!("invalid runtime nats url: {error:?}")))?;
    let join_material_dir = PathBuf::from(HOST_RUNNER_STATE_DIR).join(JOIN_MATERIAL_DIR);
    let role_environment = PloyzdRoleEnvironmentTarget::default_path(
        machine_id.clone(),
        runtime_nats_client_url,
        RoleNatsCredentials::joined(&join_material_dir),
    )
    .with_dataplane_endpoint_subnet(redeemed.endpoint_subnet.clone());

    Ok(RedeemedHostRunnerJoin::new(
        redeemed.operation_id,
        machine_id.clone(),
        HostRunnerJoinTarget::new(
            material,
            ployzd_artifact,
            DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
            roles,
            role_environment,
        ),
    )
    .with_callback_result(callback_result))
}

struct StartupJoinTokenConsumer {
    join_token_file: std::path::PathBuf,
}

impl HostRunnerJoinTokenConsumer for StartupJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        crate::join::remove_join_token_file(&self.join_token_file).map_err(|error| {
            FailureMessage::try_new(error.to_string()).expect("join token file error is non-empty")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::host_runner_join_target;
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::install::{
        AbsoluteInstallPath, InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion,
        InstallSha256Digest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
        MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTrustedNats,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_sdk_types::{MachineJoinRedeemResult, MachineJoinRedeemed};

    #[test]
    fn host_runner_join_target_uses_runtime_nats_url_from_redeemed_bundle() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new("10.198.2.0/24")
                .expect("valid endpoint subnet"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = host_runner_join_target(redeemed)
            .expect("redeemed bundle converts")
            .target;

        assert_eq!(
            target.role_environment.render_for_role(
                &ployz_core::roles::DaemonProcessRole::Machine(
                    MachineId::try_new("machine_2").expect("valid machine id")
                )
            ),
            "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NATS_CA_FILE=/var/lib/ployz/join-material.d/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/join-material.d/nats.creds\nPLOYZ_MACHINE_ID=machine_2\nPLOYZ_EBPF_BYTECODE=/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\nPLOYZ_EBPF_CTL=/usr/local/bin/ployz-ebpf-ctl\n"
        );
    }

    #[test]
    fn host_runner_join_target_does_not_render_machine_public_ip_env() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            endpoint_subnet: endpoint_subnet("10.198.1.0/24"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = host_runner_join_target(redeemed)
            .expect("redeemed bundle converts")
            .target;

        let rendered = target
            .role_environment
            .render_for_role(&ployz_core::roles::DaemonProcessRole::Gateway);
        assert!(!rendered.contains("PLOYZ_MACHINE_PUBLIC_IP="));
    }

    #[test]
    fn fallback_endpoint_subnet_reaches_joined_machine_daemon() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine_255").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_255").expect("valid machine id"),
            name: MachineName::try_new("edge_255").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            endpoint_subnet: endpoint_subnet("10.198.0.0/24"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = host_runner_join_target(redeemed)
            .expect("redeemed bundle converts")
            .target;
        let rendered = target.role_environment.render_for_role(
            &ployz_core::roles::DaemonProcessRole::Machine(
                MachineId::try_new("machine_255").expect("valid machine id"),
            ),
        );

        assert!(rendered.contains("PLOYZ_DATAPLANE_ENDPOINT_SUBNET=10.198.0.0/24\n"));
    }

    fn machine_join_bundle() -> MachineJoinBundle {
        MachineJoinBundle {
            material: MachineJoinMaterial {
                cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
                dataplane_endpoint_supernet:
                    ployz_core::dataplane::MachineEndpointSupernet::default_v1(),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                    .expect("valid runtime nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: NatsCaCertificatePem::try_new(
                        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                    )
                    .expect("valid ca pem"),
                },
                recovery_key_wrapped: ployz_core::install::WrappedCaKey::new(vec![1, 2, 3]),
                core_seeds_wrapped: ployz_core::install::WrappedCoreSeeds::new(vec![4, 5, 6]),
                ployzd: join_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
                ebpf_bytecode: join_artifact(
                    "/tmp/ployz-ebpf-tc",
                    "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
                ),
                ebpf_ctl: join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
            },
        }
    }

    fn endpoint_subnet(value: &str) -> ployz_core::dataplane::MachineEndpointSubnet {
        ployz_core::dataplane::MachineEndpointSubnet::try_new(value).expect("valid endpoint subnet")
    }

    fn join_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
        InstallArtifactSpec {
            version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
            source: InstallArtifactSource::try_new(source).expect("valid source"),
            sha256: InstallSha256Digest::try_new(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("valid digest"),
            install_path: AbsoluteInstallPath::try_new(install_path).expect("valid install path"),
        }
    }

    fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
        MachineJoinSecretDelivery {
            nats_credentials: NatsUserSeed::try_new(
                "SUACH75SWCM5D2JMJM6EKLR2WDARVGZT4QC6LX3AGHSWOMVAKERABBBRWM",
            )
            .expect("valid nats credentials"),
        }
    }
}
