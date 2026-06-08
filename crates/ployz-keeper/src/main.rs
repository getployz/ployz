use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use ployz_core::ops::FailureMessage;
use ployz_core::roles::joined_node_process_set;
use ployz_keeper::artifacts::{
    ArtifactSource, ArtifactVersion, PloyzdArtifactTarget, Sha256Digest,
};
use ployz_keeper::cli::{KeeperCliError, KeeperCommand, load_command};
use ployz_keeper::executor::{KeeperPlanFailure, KeeperPlanTerminal, execute_keeper_plan};
use ployz_keeper::join_executor::{
    KeeperJoinRedeemer, KeeperJoinReporter, KeeperJoinTokenConsumer, RedeemedKeeperJoin,
    execute_keeper_join,
};
use ployz_keeper::local::{KeeperLocalConfig, KeeperLocalEffects, SystemKeeperCommandRunner};
use ployz_keeper::report::KeeperTextRecorder;
use ployz_keeper::steps::{
    FirstNodeInstallTarget, JoinToken, KeeperJoinTarget, NonEmptyRoleSet,
    PloyzdRoleEnvironmentTarget, RedactedJoinMaterial, first_node_install_plan,
};
use ployz_nats::connect::{NatsClientUrl, NatsClientUrlError, connect_with_timeout};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    MachineJoinRedeemRequest, MachineJoinRedeemed, MachineJoinReportOutcome,
    MachineJoinReportRequest, MachineJoinToken,
};

const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> ExitCode {
    match load_command(std::env::args_os().skip(1)) {
        Ok(KeeperCommand::Start(startup)) => {
            if let Some(join) = &startup.join {
                let stdout = std::io::stdout();
                let mut recorder = KeeperTextRecorder::new(stdout.lock());
                let execution = run_join(&join.token, join.file.clone(), &mut recorder);
                match execution.terminal {
                    KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
                    KeeperPlanTerminal::Failed(failure) => {
                        eprintln!("ployz-keeper join failed: {}", failure_summary(&failure));
                        ExitCode::FAILURE
                    }
                }
            } else {
                println!("ployz-keeper started");
                ExitCode::SUCCESS
            }
        }
        Ok(KeeperCommand::FirstNodeInstall(target)) => {
            let stdout = std::io::stdout();
            let mut recorder = KeeperTextRecorder::new(stdout.lock());
            let execution = run_first_node_install(*target, &mut recorder);
            match execution.terminal {
                KeeperPlanTerminal::Completed => ExitCode::SUCCESS,
                KeeperPlanTerminal::Failed(failure) => {
                    eprintln!(
                        "ployz-keeper first-node-install failed: {}",
                        failure_summary(&failure)
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Err(KeeperCliError::HelpRequested) => {
            println!("{}", KeeperCliError::HelpRequested);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run_join(
    token: &JoinToken,
    join_token_file: std::path::PathBuf,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let mut redeemer = SystemJoinRedeemer::from_env();
    let mut reporter = SystemJoinReporter::from_env(token.clone());
    let mut token_consumer = StartupJoinTokenConsumer { join_token_file };
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: "/var/lib/ployz".into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    execute_keeper_join(
        token,
        &mut redeemer,
        &mut reporter,
        &mut token_consumer,
        &mut effects,
        recorder,
    )
}

fn run_first_node_install(
    target: FirstNodeInstallTarget,
    recorder: &mut impl ployz_keeper::executor::KeeperStepRecorder,
) -> ployz_keeper::executor::KeeperPlanExecution {
    let plan = first_node_install_plan(target);
    let mut effects = KeeperLocalEffects::new(
        KeeperLocalConfig {
            systemd_dir: "/etc/systemd/system".into(),
            state_dir: "/var/lib/ployz".into(),
        },
        SystemKeeperCommandRunner::default(),
    );
    execute_keeper_plan(&plan, &mut effects, recorder)
}

fn failure_summary(failure: &KeeperPlanFailure) -> &str {
    match failure {
        KeeperPlanFailure::Step(step) => step.message.as_str(),
        KeeperPlanFailure::Record(record) => record.message.as_str(),
    }
}

struct SystemJoinRedeemer {
    nats_url: Result<NatsClientUrl, KeeperNatsUrlError>,
}

impl SystemJoinRedeemer {
    fn from_env() -> Self {
        Self {
            nats_url: load_nats_url_from_env(),
        }
    }
}

impl KeeperJoinRedeemer for SystemJoinRedeemer {
    fn redeem_join_token(
        &mut self,
        token: &JoinToken,
    ) -> Result<RedeemedKeeperJoin, FailureMessage> {
        let nats_url = self
            .nats_url
            .clone()
            .map_err(|error| failure_message(&format!("{error}")))?;
        let join_token = MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        let redeem_nats_url = nats_url.clone();
        let redeemed = runtime.block_on(async move {
            let client = connect_with_timeout(&redeem_nats_url, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            OperationApiClient::new(client)
                .machine_join_redeem(&MachineJoinRedeemRequest { join_token })
                .await
                .map_err(|error| failure_message(&format!("failed to redeem join token: {error}")))
        })?;

        keeper_join_target(redeemed)
    }
}

struct SystemJoinReporter {
    nats_url: Result<NatsClientUrl, KeeperNatsUrlError>,
    join_token: JoinToken,
}

impl SystemJoinReporter {
    fn from_env(join_token: JoinToken) -> Self {
        Self {
            nats_url: load_nats_url_from_env(),
            join_token,
        }
    }
}

impl KeeperJoinReporter for SystemJoinReporter {
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

impl SystemJoinReporter {
    fn report_join_result(&self, request: MachineJoinReportRequest) -> Result<(), FailureMessage> {
        let nats_url = self
            .nats_url
            .clone()
            .map_err(|error| failure_message(&format!("{error}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        runtime.block_on(async move {
            let client = connect_with_timeout(&nats_url, DEFAULT_NATS_CONNECT_TIMEOUT)
                .await
                .map_err(|error| failure_message(&error.to_string()))?;
            OperationApiClient::new(client)
                .machine_join_report(&request)
                .await
                .map(|_| ())
                .map_err(|error| failure_message(&format!("failed to report join result: {error}")))
        })
    }

    fn machine_join_token(&self) -> Result<MachineJoinToken, FailureMessage> {
        MachineJoinToken::try_new(self.join_token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))
    }
}

fn load_nats_url_from_env() -> Result<NatsClientUrl, KeeperNatsUrlError> {
    let value = std::env::var(PLOYZ_NATS_URL_ENV).map_err(|_| KeeperNatsUrlError::Missing)?;
    NatsClientUrl::try_new(value.clone())
        .map_err(|source| KeeperNatsUrlError::Invalid { value, source })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeeperNatsUrlError {
    Missing,
    Invalid {
        value: String,
        source: NatsClientUrlError,
    },
}

impl std::fmt::Display for KeeperNatsUrlError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(formatter, "{PLOYZ_NATS_URL_ENV} is required"),
            Self::Invalid { value, .. } => {
                write!(formatter, "{PLOYZ_NATS_URL_ENV}={value:?} is invalid")
            }
        }
    }
}

fn keeper_join_target(redeemed: MachineJoinRedeemed) -> Result<RedeemedKeeperJoin, FailureMessage> {
    let material = RedactedJoinMaterial::new(
        redeemed.node_id.clone(),
        redeemed.join_bundle.cluster_name.as_str().to_owned(),
    )
    .map_err(|error| failure_message(&format!("invalid join material: {error:?}")))?;
    let artifact = PloyzdArtifactTarget::new(
        ArtifactVersion::try_new(redeemed.join_bundle.ployzd.version.as_str())
            .map_err(|error| failure_message(&format!("invalid ployzd version: {error}")))?,
        ArtifactSource::try_new(redeemed.join_bundle.ployzd.source.as_str())
            .map_err(|error| failure_message(&format!("invalid ployzd source: {error}")))?,
        Sha256Digest::try_new(redeemed.join_bundle.ployzd.sha256.as_str())
            .map_err(|error| failure_message(&format!("invalid ployzd digest: {error}")))?,
        PathBuf::from(redeemed.join_bundle.ployzd.install_path.as_str()),
    )
    .map_err(|error| failure_message(&format!("invalid ployzd install target: {error}")))?;
    let roles = NonEmptyRoleSet::try_new(
        joined_node_process_set(&redeemed.node_id, redeemed.gateway)
            .roles()
            .to_vec(),
    )
    .map_err(|error| failure_message(&format!("invalid joined node role set: {error:?}")))?;
    let runtime_nats_url =
        NatsClientUrl::try_new(redeemed.join_bundle.runtime_nats_url.as_str())
            .map_err(|error| failure_message(&format!("invalid runtime nats url: {error:?}")))?;
    Ok(RedeemedKeeperJoin::new(
        redeemed.operation_id,
        redeemed.node_id,
        KeeperJoinTarget::new(
            material,
            artifact,
            roles,
            PloyzdRoleEnvironmentTarget::default_path(runtime_nats_url),
        ),
    ))
}

fn failure_message(message: &str) -> FailureMessage {
    FailureMessage::try_new(message).expect("generated keeper failure message is non-empty")
}

struct StartupJoinTokenConsumer {
    join_token_file: std::path::PathBuf,
}

impl KeeperJoinTokenConsumer for StartupJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        ployz_keeper::join::remove_join_token_file(&self.join_token_file).map_err(|error| {
            FailureMessage::try_new(error.to_string()).expect("join token file error is non-empty")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::keeper_join_target;
    use ployz_core::ids::{NodeId, OperationId};
    use ployz_core::install::{
        AbsoluteInstallPath, InstallArtifactSource, InstallArtifactVersion, InstallSha256Digest,
        MachineJoinBundle, MachineJoinClusterName, MachineJoinPloyzdArtifact,
        MachineJoinRuntimeNatsUrl,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::roles::FirstNodeGateway;
    use ployz_sdk_types::{MachineJoinRedeemResult, MachineJoinRedeemed};

    #[test]
    fn keeper_join_target_uses_runtime_nats_url_from_redeemed_bundle() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            node_id: NodeId::try_new("node_2").expect("valid node id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: FirstNodeGateway::Skip,
            join_bundle: machine_join_bundle(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::ops::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = keeper_join_target(redeemed)
            .expect("redeemed bundle converts")
            .target;

        assert_eq!(
            target.role_environment.render(),
            "PLOYZ_NATS_URL=nats://127.0.0.1:7422\n"
        );
    }

    fn machine_join_bundle() -> MachineJoinBundle {
        MachineJoinBundle {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                .expect("valid runtime nats url"),
            ployzd: MachineJoinPloyzdArtifact {
                version: InstallArtifactVersion::try_new("0.1.0").expect("valid version"),
                source: InstallArtifactSource::try_new("/tmp/ployzd").expect("valid source"),
                sha256: InstallSha256Digest::try_new(
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("valid digest"),
                install_path: AbsoluteInstallPath::try_new("/usr/local/bin/ployzd")
                    .expect("valid install path"),
            },
        }
    }
}
