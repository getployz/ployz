//! Machine Join transport clients and keeper startup resume.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use super::execution::{
    HostRunnerJoinRedeemer, HostRunnerJoinReporter, HostRunnerJoinResolver,
    HostRunnerJoinTokenConsumer, JoinTargetResolutionFailure, execute_host_runner_join,
};
use super::{JOIN_MATERIAL_DIR, remove_join_token_file};
use crate::cli::HostRunnerStartup;
use crate::execution::{ArtifactKind, DataplaneArtifactTargets, artifact_target};
use crate::execution::{
    HostRunnerLocalConfig, HostRunnerLocalEffects, SupervisorDirectories,
    SystemHostRunnerCommandRunner,
};
use crate::lifecycle::installed_substrate::InstalledSubstrateRelease;
use crate::plan::HostRunnerPlanTerminal;
use crate::plan::{
    HostRunnerJoinMaterial, HostRunnerJoinTarget, HostRunnerTextRecorder, JoinToken,
    NonEmptyRoleSet, PloyzReleaseArtifact, PloyzdRoleEnvironmentTarget, RoleNatsCredentials,
};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::operation::FailureMessage;
use ployz_core::roles::plan_joined_machine_process_set;
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{
    NatsClientAuth, NatsClientUrl, NatsClientUrlError, NatsConnectConfig, NatsTlsTrust,
    connect_authenticated,
};
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{
    MachineJoinRedeemed, MachineJoinReportOutcome, MachineJoinReportRequest,
    MachineJoinReportedOutcome, MachineJoinToken,
};

use crate::release_manifest::{
    ReleaseManifest, ReleaseManifestError, ReleasePlatform, persisted_release_manifest_url,
    read_release_manifest_text, release_manifest_url_for_platform,
};
use crate::runtime::{
    DEFAULT_NATS_CONNECT_TIMEOUT, HOST_RUNNER_STATE_DIR, PLOYZ_JOIN_NKEY_SEED_ENV,
    PLOYZ_NATS_CA_FILE_ENV, PLOYZ_NATS_URL_ENV, failure_message, failure_summary,
};
use ployz_core::install::{
    FirstMachineInstallArtifacts, MachineJoinSubstrateRelease, ReleasePlatformFailure,
};

const JOIN_REPORT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

mod authorization;

use authorization::redeem_across_authorization_reload;

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
    recorder: &mut impl crate::plan::HostRunnerStepRecorder,
) -> crate::plan::HostRunnerPlanExecution {
    run_join_with_consumer(
        token,
        StartupJoinTokenConsumer { join_token_file },
        recorder,
    )
}

pub(crate) fn run_join_with_consumer(
    token: &JoinToken,
    mut token_consumer: impl HostRunnerJoinTokenConsumer,
    recorder: &mut impl crate::plan::HostRunnerStepRecorder,
) -> crate::plan::HostRunnerPlanExecution {
    let mut redeemer = JoinRedeemer::from_env();
    let mut resolver = JoinTargetResolver;
    let mut reporter = JoinReporter::from_env(token.clone());
    let mut effects = HostRunnerLocalEffects::new(
        HostRunnerLocalConfig {
            supervisor_dirs: SupervisorDirectories::host_defaults(),
            state_dir: HOST_RUNNER_STATE_DIR.into(),
            docker_daemon_config: "/etc/docker/daemon.json".into(),
            docker_repository_dir: "/etc/yum.repos.d".into(),
        },
        SystemHostRunnerCommandRunner::default(),
    );
    execute_host_runner_join(
        token,
        &mut redeemer,
        &mut resolver,
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
    ) -> Result<MachineJoinRedeemed, FailureMessage> {
        let connect = self.connect.clone()?;
        let join_token = MachineJoinToken::try_new(token.as_str())
            .map_err(|error| failure_message(&format!("invalid join token: {error:?}")))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure_message(&format!("failed to start async runtime: {error}")))?;

        let redeemed = runtime.block_on(async move {
            redeem_across_authorization_reload(&connect, join_token, |retry| {
                eprintln!(
                    "join authorization unavailable; retrying redemption ({}/{})",
                    retry.attempt, retry.attempts
                );
            })
            .await
        })?;

        Ok(redeemed)
    }
}

pub(crate) struct JoinTargetResolver;

impl HostRunnerJoinResolver for JoinTargetResolver {
    fn resolve_join_target(
        &mut self,
        redeemed: &MachineJoinRedeemed,
    ) -> Result<HostRunnerJoinTarget, JoinTargetResolutionFailure> {
        resolve_host_runner_join_target(redeemed)
    }
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

fn resolve_host_runner_join_target(
    redeemed: &MachineJoinRedeemed,
) -> Result<HostRunnerJoinTarget, JoinTargetResolutionFailure> {
    let resolved = load_local_join_artifacts(&redeemed.join_bundle.material.substrate_release)?;
    resolve_host_runner_join_target_with_artifacts(redeemed, resolved.artifacts)
        .map(|target| target.with_installed_substrate_release(resolved.installed_release))
        .map_err(|message| JoinTargetResolutionFailure::Other { message })
}

fn resolve_host_runner_join_target_with_artifacts(
    redeemed: &MachineJoinRedeemed,
    artifacts: FirstMachineInstallArtifacts,
) -> Result<HostRunnerJoinTarget, FailureMessage> {
    let machine_id = redeemed.machine_id.clone();
    let material = HostRunnerJoinMaterial::from_join_payload(
        machine_id.clone(),
        &redeemed.join_bundle,
        &redeemed.secret_delivery,
    )
    .map_err(|error| failure_message(&format!("invalid join material: {error:?}")))?;
    let ployzd_artifact = artifact_target(ArtifactKind::Ployzd, &artifacts.ployzd)
        .map_err(|error| failure_message(&format!("invalid ployzd install target: {error}")))?;
    let ployzd_artifact = PloyzReleaseArtifact::try_new(ployzd_artifact)
        .map_err(|error| failure_message(&format!("invalid Ployz release: {error}")))?;
    let ebpf_bytecode_artifact =
        artifact_target(ArtifactKind::EbpfBytecode, &artifacts.ebpf_bytecode).map_err(|error| {
            failure_message(&format!("invalid eBPF bytecode install target: {error}"))
        })?;
    let ebpf_ctl_artifact = artifact_target(ArtifactKind::EbpfCtl, &artifacts.ebpf_ctl)
        .map_err(|error| failure_message(&format!("invalid eBPF ctl install target: {error}")))?;
    let railpack_artifact = artifact_target(ArtifactKind::Railpack, &artifacts.railpack)
        .map_err(|error| failure_message(&format!("invalid Railpack install target: {error}")))?;
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

    Ok(HostRunnerJoinTarget::new(
        material,
        ployzd_artifact,
        DataplaneArtifactTargets::new(ebpf_bytecode_artifact, ebpf_ctl_artifact),
        railpack_artifact,
        roles,
        role_environment,
        redeemed.host_port_assurance,
    ))
}

fn load_local_join_artifacts(
    expected: &MachineJoinSubstrateRelease,
) -> Result<ResolvedJoinArtifacts, JoinTargetResolutionFailure> {
    let platform = ReleasePlatform::from_target(std::env::consts::OS, std::env::consts::ARCH)
        .map_err(|_| JoinTargetResolutionFailure::ReleasePlatform {
            failure: ReleasePlatformFailure::Unsupported {
                platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            },
        })?;
    load_join_artifacts_from_release_env(
        std::path::Path::new("/etc/ployz/release.env"),
        expected,
        platform,
    )
}

fn load_join_artifacts_from_release_env(
    release_env: &std::path::Path,
    expected: &MachineJoinSubstrateRelease,
    platform: ReleasePlatform,
) -> Result<ResolvedJoinArtifacts, JoinTargetResolutionFailure> {
    let manifest_url = persisted_release_manifest_url(release_env).map_err(|error| {
        other_resolution_failure(format!(
            "failed to read installed release identity: {error}"
        ))
    })?;
    let installed_contents =
        read_release_manifest_text(&manifest_url).map_err(other_resolution_failure)?;
    resolve_join_artifacts_from_manifests(expected, platform, &installed_contents, |url| {
        read_release_manifest_text(url)
    })
}

fn other_resolution_failure(message: impl Into<String>) -> JoinTargetResolutionFailure {
    JoinTargetResolutionFailure::Other {
        message: failure_message(&message.into()),
    }
}

fn manifest_resolution_failure(error: ReleaseManifestError) -> JoinTargetResolutionFailure {
    match error {
        ReleaseManifestError::Platform { failure } => {
            JoinTargetResolutionFailure::ReleasePlatform { failure }
        }
        ReleaseManifestError::Invalid { message } => other_resolution_failure(message),
    }
}

fn resolve_join_artifacts_from_manifests(
    expected: &MachineJoinSubstrateRelease,
    local_platform: ReleasePlatform,
    installed_contents: &str,
    read_exact_manifest: impl FnOnce(&str) -> Result<String, String>,
) -> Result<ResolvedJoinArtifacts, JoinTargetResolutionFailure> {
    match ReleaseManifest::parse(installed_contents) {
        Ok(installed)
            if installed.platform() == local_platform
                && installed.version() == &expected.version =>
        {
            let artifacts = installed
                .install_artifacts()
                .map_err(other_resolution_failure)?;
            let installed_release = InstalledSubstrateRelease::persisted_manifest(
                expected.clone(),
                installed_contents.to_owned(),
            )
            .map_err(other_resolution_failure)?;
            return Ok(ResolvedJoinArtifacts {
                artifacts,
                installed_release,
            });
        }
        Err(error @ ReleaseManifestError::Platform { .. }) => {
            return Err(manifest_resolution_failure(error));
        }
        Ok(_) | Err(ReleaseManifestError::Invalid { .. }) => {}
    }

    if expected.version.as_str().starts_with("dev-") {
        return Err(other_resolution_failure(
            "installed local development release does not match cluster join release",
        ));
    }

    let url = release_manifest_url_for_platform(&expected.version, local_platform.manifest_slug());
    let contents = read_exact_manifest(&url).map_err(other_resolution_failure)?;
    let manifest = ReleaseManifest::parse(&contents).map_err(manifest_resolution_failure)?;
    let artifacts = resolve_join_artifacts(expected, &manifest, local_platform)
        .map_err(other_resolution_failure)?;
    Ok(ResolvedJoinArtifacts {
        artifacts,
        installed_release: InstalledSubstrateRelease::public_exact(expected.clone()),
    })
}

#[derive(Debug)]
struct ResolvedJoinArtifacts {
    artifacts: FirstMachineInstallArtifacts,
    installed_release: InstalledSubstrateRelease,
}

fn resolve_join_artifacts(
    expected: &MachineJoinSubstrateRelease,
    manifest: &ReleaseManifest,
    local_platform: ReleasePlatform,
) -> Result<FirstMachineInstallArtifacts, String> {
    if manifest.version() != &expected.version {
        return Err(format!(
            "installed release {} does not match cluster join release {}",
            manifest.ployz_version(),
            expected.version.as_str()
        ));
    }
    manifest.install_artifacts_for(local_platform)
}

struct StartupJoinTokenConsumer {
    join_token_file: std::path::PathBuf,
}

impl HostRunnerJoinTokenConsumer for StartupJoinTokenConsumer {
    fn consume_join_token(&mut self) -> Result<(), FailureMessage> {
        remove_join_token_file(&self.join_token_file).map_err(|error| {
            FailureMessage::try_new(error.to_string()).expect("join token file error is non-empty")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_join_artifacts_from_release_env, resolve_host_runner_join_target_with_artifacts,
        resolve_join_artifacts, resolve_join_artifacts_from_manifests,
    };
    use crate::lifecycle::installed_substrate::{
        InstalledSubstrateManifestSource, load_installed_substrate_release,
        store_installed_substrate_release,
    };
    use crate::lifecycle::machine_join::execution::JoinTargetResolutionFailure;
    use ployz_core::ids::{MachineId, OperationId};
    use ployz_core::install::{
        ExactPloyzVersion, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
        MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinSubstrateRelease,
        MachineJoinTrustedNats, ReleasePlatformFailure,
    };
    use ployz_core::machine::{JoinTokenRedeemedAt, MachineName};
    use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_sdk_types::{MachineJoinRedeemResult, MachineJoinRedeemed};

    use crate::release_manifest::{ReleaseManifest, ReleasePlatform};

    const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[path = "authorization_tests.rs"]
    mod authorization_tests;

    fn redeemed_machine() -> MachineJoinRedeemed {
        MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("operation id"),
            machine_id: MachineId::try_new("machine_2").expect("machine id"),
            name: MachineName::try_new("edge_2").expect("machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::External,
            endpoint_subnet: endpoint_subnet("10.198.2.0/24"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("redeemed at"),
            last_event_sequence: ployz_core::operation::EventSequence::try_new(8)
                .expect("event sequence"),
            result: MachineJoinRedeemResult::Joined,
        }
    }

    #[test]
    fn joining_host_resolves_arm64_artifacts_from_its_local_release_manifest() {
        let manifest = release_manifest("linux-arm64", "linux-arm64");
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };

        let artifacts = resolve_join_artifacts(&expected, &manifest, ReleasePlatform::LinuxArm64)
            .expect("matching local ARM release resolves");

        assert_eq!(
            artifacts.ployzd.source.as_str(),
            "https://example.test/ployzd-linux-arm64"
        );
        assert_eq!(
            artifacts.ebpf_ctl.source.as_str(),
            "https://example.test/ployz-ebpf-ctl-linux-arm64"
        );
        assert_eq!(
            artifacts.railpack.source.as_str(),
            "https://example.test/railpack-linux-arm64"
        );
    }

    #[test]
    fn joining_host_resolves_amd64_artifacts_from_its_local_release_manifest() {
        let manifest = release_manifest("linux-amd64", "linux-amd64");
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };

        let artifacts = resolve_join_artifacts(&expected, &manifest, ReleasePlatform::LinuxAmd64)
            .expect("matching local AMD release resolves");

        assert_eq!(
            artifacts.ployzd.source.as_str(),
            "https://example.test/ployzd-linux-amd64"
        );
    }

    #[test]
    fn joining_host_rejects_release_for_another_platform() {
        let manifest = release_manifest("linux-amd64", "linux-amd64");
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };

        let error = resolve_join_artifacts(&expected, &manifest, ReleasePlatform::LinuxArm64)
            .expect_err("cross-platform local manifest is rejected");

        assert_eq!(
            error,
            "release manifest platform linux-amd64 does not match host platform linux-arm64"
        );
    }

    #[test]
    fn joining_host_rejects_a_release_other_than_the_cluster_join_release() {
        let manifest = release_manifest("linux-arm64", "linux-arm64");
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.2.0").expect("exact release"),
        };

        let error = resolve_join_artifacts(&expected, &manifest, ReleasePlatform::LinuxArm64)
            .expect_err("stale local release is rejected");

        assert_eq!(
            error,
            "installed release 0.1.0 does not match cluster join release 0.2.0"
        );
    }

    #[test]
    fn joining_host_requires_installer_persisted_release_identity() {
        let root = tempfile::tempdir().expect("tempdir");
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };

        let error = load_join_artifacts_from_release_env(
            &root.path().join("missing-release.env"),
            &expected,
            ReleasePlatform::LinuxArm64,
        )
        .expect_err("missing installed release identity fails closed");

        assert!(matches!(
            error,
            JoinTargetResolutionFailure::Other { message }
                if message.as_str().starts_with("failed to read installed release identity:")
        ));
    }

    #[test]
    fn advanced_installer_channel_resolves_the_founder_selected_exact_release() {
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };
        let installed = release_manifest_contents("linux-arm64", "linux-arm64", "0.2.0");
        let exact = release_manifest_contents("linux-arm64", "linux-arm64", "0.1.0");

        let artifacts = resolve_join_artifacts_from_manifests(
            &expected,
            ReleasePlatform::LinuxArm64,
            &installed,
            |url| {
                assert!(url.contains("/download/v0.1.0/ployz-release-linux-arm64.env"));
                Ok(exact)
            },
        )
        .expect("founder-selected release resolves independently of installed channel");

        assert_eq!(artifacts.artifacts.ployzd.version.as_str(), "0.1.0");
        assert_eq!(
            artifacts.installed_release.manifest_source,
            InstalledSubstrateManifestSource::PublicExactRelease
        );
        assert_eq!(
            artifacts.artifacts.ployzd.source.as_str(),
            "https://example.test/ployzd-linux-arm64"
        );

        let state_dir = tempfile::tempdir().expect("state dir");
        store_installed_substrate_release(state_dir.path(), &artifacts.installed_release)
            .expect("installed exact identity persists");
        assert_eq!(
            load_installed_substrate_release(state_dir.path())
                .expect("installed exact identity loads")
                .release
                .version
                .as_str(),
            "0.1.0"
        );
    }

    #[test]
    fn matching_custom_join_manifest_persists_the_exact_source_bytes() {
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };
        let installed = release_manifest_contents("linux-arm64", "custom-arm64", "0.1.0");

        let resolved = resolve_join_artifacts_from_manifests(
            &expected,
            ReleasePlatform::LinuxArm64,
            &installed,
            |_| panic!("matching local manifests do not fetch a fallback"),
        )
        .expect("matching local manifest resolves");

        assert_eq!(
            resolved.installed_release.manifest_source,
            InstalledSubstrateManifestSource::PersistedManifest {
                contents: installed,
            }
        );
    }

    #[test]
    fn exact_join_manifest_missing_platform_is_a_typed_failure() {
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };
        let installed = release_manifest_contents("linux-arm64", "linux-arm64", "0.2.0");
        let exact = release_manifest_contents("linux-arm64", "linux-arm64", "0.1.0").replacen(
            "PLOYZ_RELEASE_PLATFORM=linux-arm64\n",
            "",
            1,
        );

        let failure = resolve_join_artifacts_from_manifests(
            &expected,
            ReleasePlatform::LinuxArm64,
            &installed,
            |_| Ok(exact),
        )
        .expect_err("missing exact-release platform fails closed");

        assert_eq!(
            failure,
            JoinTargetResolutionFailure::ReleasePlatform {
                failure: ReleasePlatformFailure::Missing,
            }
        );
    }

    #[test]
    fn exact_join_manifest_unsupported_platform_is_a_typed_failure() {
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
        };
        let installed = release_manifest_contents("linux-arm64", "linux-arm64", "0.2.0");
        let exact = release_manifest_contents("linux-riscv64", "linux-riscv64", "0.1.0");

        let failure = resolve_join_artifacts_from_manifests(
            &expected,
            ReleasePlatform::LinuxArm64,
            &installed,
            |_| Ok(exact),
        )
        .expect_err("unsupported exact-release platform fails closed");

        assert_eq!(
            failure,
            JoinTargetResolutionFailure::ReleasePlatform {
                failure: ReleasePlatformFailure::Unsupported {
                    platform: "linux-riscv64".to_owned(),
                },
            }
        );
    }

    #[test]
    fn development_join_manifest_missing_platform_is_a_typed_failure() {
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("dev-local").expect("exact release"),
        };
        let installed = release_manifest_contents("linux-arm64", "linux-arm64", "dev-local")
            .replacen("PLOYZ_RELEASE_PLATFORM=linux-arm64\n", "", 1);

        let failure = resolve_join_artifacts_from_manifests(
            &expected,
            ReleasePlatform::LinuxArm64,
            &installed,
            |_| panic!("development joins do not fetch a remote fallback"),
        )
        .expect_err("missing installed development platform fails closed");

        assert_eq!(
            failure,
            JoinTargetResolutionFailure::ReleasePlatform {
                failure: ReleasePlatformFailure::Missing,
            }
        );
    }

    #[test]
    fn development_join_manifest_unsupported_platform_is_a_typed_failure() {
        let expected = MachineJoinSubstrateRelease {
            version: ExactPloyzVersion::try_new("dev-local").expect("exact release"),
        };
        let installed = release_manifest_contents("linux-riscv64", "linux-riscv64", "dev-local");

        let failure = resolve_join_artifacts_from_manifests(
            &expected,
            ReleasePlatform::LinuxArm64,
            &installed,
            |_| panic!("development joins do not fetch a remote fallback"),
        )
        .expect_err("unsupported installed development platform fails closed");

        assert_eq!(
            failure,
            JoinTargetResolutionFailure::ReleasePlatform {
                failure: ReleasePlatformFailure::Unsupported {
                    platform: "linux-riscv64".to_owned(),
                },
            }
        );
    }

    #[test]
    fn host_runner_join_target_uses_runtime_nats_url_from_redeemed_bundle() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::External,
            endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.2.0/24")
                .expect("valid endpoint subnet"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::operation::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = resolved_target(&redeemed);

        assert_eq!(
            target.host_port_assurance,
            ployz_core::install::HostPortAssurance::External
        );

        assert_eq!(
            target.role_environment.render_for_role(
                &ployz_core::roles::DaemonProcessRole::Machine(
                    MachineId::try_new("machine_2").expect("valid machine id")
                )
            ),
            "PLOYZ_NATS_URL=nats://127.0.0.1:7422\nPLOYZ_NATS_CA_FILE=/var/lib/ployz/join-material.d/ca.pem\nPLOYZ_NATS_NKEY_SEED_FILE=/var/lib/ployz/join-material.d/nats.creds\nPLOYZ_MACHINE_ID=machine_2\nPLOYZ_DATAPLANE_ENDPOINT_SUBNET=10.198.2.0/24\nPLOYZ_EBPF_BYTECODE=/usr/local/lib/ployz/ebpf/ployz-ebpf-tc\nPLOYZ_EBPF_CTL=/usr/local/bin/ployz-ebpf-ctl\n"
        );
    }

    #[test]
    fn host_runner_join_target_does_not_render_machine_public_ip_env() {
        let redeemed = MachineJoinRedeemed {
            operation_id: OperationId::try_new("op_machine").expect("valid operation id"),
            machine_id: MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
            endpoint_subnet: endpoint_subnet("10.198.1.0/24"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::operation::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = resolved_target(&redeemed);

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
            host_port_assurance: ployz_core::install::HostPortAssurance::Keeper,
            endpoint_subnet: endpoint_subnet("10.198.0.0/24"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed at"),
            last_event_sequence: ployz_core::operation::EventSequence::try_new(8)
                .expect("valid sequence"),
            result: MachineJoinRedeemResult::Joined,
        };

        let target = resolved_target(&redeemed);
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
                    ployz_core::network::MachineEndpointSupernet::default_v1(),
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
                substrate_release: MachineJoinSubstrateRelease {
                    version: ExactPloyzVersion::try_new("0.1.0").expect("exact release"),
                },
            },
        }
    }

    fn endpoint_subnet(value: &str) -> ployz_core::network::MachineEndpointSubnet {
        ployz_core::network::MachineEndpointSubnet::try_new(value).expect("valid endpoint subnet")
    }

    fn release_manifest(platform: &str, artifact_suffix: &str) -> ReleaseManifest {
        ReleaseManifest::parse(&release_manifest_contents(
            platform,
            artifact_suffix,
            "0.1.0",
        ))
        .expect("release manifest")
    }

    fn release_manifest_contents(platform: &str, artifact_suffix: &str, version: &str) -> String {
        format!(
            "PLOYZ_RELEASE_PLATFORM={platform}\n\
             PLOYZ_VERSION={version}\n\
             PLOYZD_URL=https://example.test/ployzd-{artifact_suffix}\n\
             PLOYZD_SHA256={SHA}\n\
             PLOYZ_EBPF_TC_URL=https://example.test/ployz-ebpf-tc-{artifact_suffix}\n\
             PLOYZ_EBPF_TC_SHA256={SHA}\n\
             PLOYZ_EBPF_CTL_URL=https://example.test/ployz-ebpf-ctl-{artifact_suffix}\n\
             PLOYZ_EBPF_CTL_SHA256={SHA}\n\
             PLOYZ_RAILPACK_VERSION=v0.31.0\n\
             PLOYZ_RAILPACK_URL=https://example.test/railpack-{artifact_suffix}\n\
             PLOYZ_RAILPACK_SHA256={SHA}\n"
        )
    }

    fn resolved_target(redeemed: &MachineJoinRedeemed) -> crate::plan::HostRunnerJoinTarget {
        let artifacts = release_manifest("linux-amd64", "linux-amd64")
            .install_artifacts()
            .expect("release artifacts");
        resolve_host_runner_join_target_with_artifacts(redeemed, artifacts)
            .expect("redeemed bundle converts")
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
