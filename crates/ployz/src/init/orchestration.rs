//! Strict check-then-do founding workflow.

use std::future::Future;

use ployz_core::founding::{
    FoundingDriverEnrollment, FoundingRefusal, FoundingResult, ValidatedFoundingRequest,
    classify_founding_arrival,
};
use ployz_core::operation::FailureMessage;
use ployz_host_runner::lifecycle::founding::{
    FoundingHostEffects, FoundingMilestone, FoundingStateDirectory, FoundingStateError,
};

/// The checks performed through the founding API before and after credential removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundingReadiness {
    /// Proves a trivial Corrosion query, a ready barrier, and `/version`.
    Bootstrap,
    /// Proves `/version` and that the bootstrap-only founding route stays disabled.
    Ordinary,
}

/// The only control-plane boundary crossed by machine-local founding.
///
/// Implementations own concrete HTTP, authentication, bounded retries, and
/// response decoding. This workflow owns when each call is legal.
pub trait FoundingControlPlane {
    fn submit_founding(
        &mut self,
        request: &ValidatedFoundingRequest,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;

    fn await_endpoint_network_ready(
        &mut self,
        request: &ValidatedFoundingRequest,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;

    fn verify_readiness(
        &mut self,
        readiness: FoundingReadiness,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;
}

/// Driver-owned progress boundary used for Cloud enrollment callbacks.
pub trait FoundingProgressObserver {
    fn driver_enrolled(
        &mut self,
        driver: &FoundingDriverEnrollment,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;

    fn ready(
        &mut self,
        driver: &FoundingDriverEnrollment,
    ) -> impl Future<Output = Result<(), FailureMessage>> + Send;
}

struct IgnoreFoundingProgress;

impl FoundingProgressObserver for IgnoreFoundingProgress {
    async fn driver_enrolled(
        &mut self,
        _driver: &FoundingDriverEnrollment,
    ) -> Result<(), FailureMessage> {
        Ok(())
    }

    async fn ready(&mut self, _driver: &FoundingDriverEnrollment) -> Result<(), FailureMessage> {
        Ok(())
    }
}

/// One externally meaningful boundary in the founding sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundingStep {
    ObserveArrival,
    PersistClusterId,
    StageArtifacts,
    EnsureDocker,
    MintMachineMaterial,
    MintDoorMaterial,
    PrepareStorage,
    WriteConfiguration,
    PersistFoundingRequest,
    RestartDockerConfiguration,
    InstallUnitsAndEnableReadyRoles,
    StartKeeper,
    StartCorrosion,
    StartBootstrapApi,
    SubmitFoundingRows,
    AwaitDriverPeerConvergence,
    ReportDriverEnrolled,
    AwaitEndpointNetwork,
    StartDns,
    VerifyDnsReadiness,
    VerifyBootstrapReadiness,
    RemoveBootstrapCredential,
    RestartOrdinaryApi,
    VerifyOrdinaryReadiness,
    ReportDriverReady,
    MarkComplete,
}

#[derive(Debug, thiserror::Error)]
pub enum FoundingFailure {
    #[error("founding refused: {0:?}")]
    Refused(FoundingRefusal),
    #[error("founding state failed during {step:?}: {source}")]
    State {
        step: FoundingStep,
        source: FoundingStateError,
    },
    #[error("founding host effect failed during {step:?}: {message}")]
    Host {
        step: FoundingStep,
        message: FailureMessage,
    },
    #[error("founding control-plane call failed during {step:?}: {message}")]
    ControlPlane {
        step: FoundingStep,
        message: FailureMessage,
    },
    #[error("founding progress callback failed during {step:?}: {message}")]
    Progress {
        step: FoundingStep,
        message: FailureMessage,
    },
}

impl FoundingFailure {
    #[must_use]
    pub const fn step(&self) -> Option<FoundingStep> {
        match self {
            Self::Refused(_) => None,
            Self::State { step, .. }
            | Self::Host { step, .. }
            | Self::ControlPlane { step, .. }
            | Self::Progress { step, .. } => Some(*step),
        }
    }
}

/// Founds machine one, resumes an interrupted attempt, or returns a no-op.
pub async fn found_machine_one(
    state: &FoundingStateDirectory,
    request: &ValidatedFoundingRequest,
    host: &mut impl FoundingHostEffects,
    control_plane: &mut impl FoundingControlPlane,
) -> Result<FoundingResult, FoundingFailure> {
    let mut progress = IgnoreFoundingProgress;
    found_machine_one_with_progress(state, request, host, control_plane, &mut progress).await
}

/// Founds machine one while reporting driver enrollment before terminal readiness.
pub async fn found_machine_one_with_progress(
    state: &FoundingStateDirectory,
    request: &ValidatedFoundingRequest,
    host: &mut impl FoundingHostEffects,
    control_plane: &mut impl FoundingControlPlane,
    progress: &mut impl FoundingProgressObserver,
) -> Result<FoundingResult, FoundingFailure> {
    let _lock = state.try_lock().map_err(|source| FoundingFailure::State {
        step: FoundingStep::ObserveArrival,
        source,
    })?;
    let arrival = state
        .observe_arrival()
        .map_err(|source| FoundingFailure::State {
            step: FoundingStep::ObserveArrival,
            source,
        })?;
    let result = classify_founding_arrival(&request.request().cluster_id, arrival)
        .map_err(FoundingFailure::Refused)?;
    if result == FoundingResult::NoOp {
        return Ok(result);
    }
    if result == FoundingResult::Found {
        state
            .persist_cluster_id_exclusive(&request.request().cluster_id)
            .map_err(|source| FoundingFailure::State {
                step: FoundingStep::PersistClusterId,
                source,
            })?;
    }

    run_host_step(
        state,
        FoundingMilestone::Artifacts,
        FoundingStep::StageArtifacts,
        || host.stage_exact_ployz_and_corrosion(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::Docker,
        FoundingStep::EnsureDocker,
        || host.ensure_docker(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::MachineMaterial,
        FoundingStep::MintMachineMaterial,
        || host.ensure_machine_identity_and_wireguard(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::FoundingRequest,
        FoundingStep::PersistFoundingRequest,
        || host.persist_validated_founding_request(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::DoorMaterial,
        FoundingStep::MintDoorMaterial,
        || host.ensure_cluster_door_material(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::Storage,
        FoundingStep::PrepareStorage,
        || host.prepare_selected_storage(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::Configuration,
        FoundingStep::WriteConfiguration,
        || host.write_configuration_with_bootstrap(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::DockerConfigured,
        FoundingStep::RestartDockerConfiguration,
        || host.restart_and_verify_docker_configuration(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::UnitsInstalledReadyEnabled,
        FoundingStep::InstallUnitsAndEnableReadyRoles,
        || host.install_units_and_enable_ready_roles(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::KeeperStarted,
        FoundingStep::StartKeeper,
        || host.start_keeper(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::CorrosionStarted,
        FoundingStep::StartCorrosion,
        || host.start_corrosion(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::BootstrapApiStarted,
        FoundingStep::StartBootstrapApi,
        || host.start_api_with_bootstrap(),
    )?;
    run_control_step(
        state,
        FoundingMilestone::RowsSubmitted,
        FoundingStep::SubmitFoundingRows,
        control_plane.submit_founding(request),
    )
    .await?;
    if request.request().driver.enrolled_peer().is_some() {
        run_host_step(
            state,
            FoundingMilestone::DriverPeerConverged,
            FoundingStep::AwaitDriverPeerConvergence,
            || host.await_driver_peer_convergence(&request.request().driver),
        )?;
        run_progress_step(
            state,
            FoundingMilestone::DriverEnrolledReported,
            FoundingStep::ReportDriverEnrolled,
            progress.driver_enrolled(&request.request().driver),
        )
        .await?;
    }
    run_control_step(
        state,
        FoundingMilestone::EndpointNetworkReady,
        FoundingStep::AwaitEndpointNetwork,
        control_plane.await_endpoint_network_ready(request),
    )
    .await?;
    run_host_step(
        state,
        FoundingMilestone::DnsStarted,
        FoundingStep::StartDns,
        || host.enable_and_start_dns(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::DnsReady,
        FoundingStep::VerifyDnsReadiness,
        || host.await_dns_readiness(),
    )?;
    run_control_step(
        state,
        FoundingMilestone::BootstrapReady,
        FoundingStep::VerifyBootstrapReadiness,
        control_plane.verify_readiness(FoundingReadiness::Bootstrap),
    )
    .await?;
    run_host_step(
        state,
        FoundingMilestone::BootstrapCredentialRemoved,
        FoundingStep::RemoveBootstrapCredential,
        || host.remove_bootstrap_credential(),
    )?;
    run_host_step(
        state,
        FoundingMilestone::OrdinaryApiStarted,
        FoundingStep::RestartOrdinaryApi,
        || host.restart_api_without_bootstrap(),
    )?;
    run_control_step(
        state,
        FoundingMilestone::OrdinaryReady,
        FoundingStep::VerifyOrdinaryReadiness,
        control_plane.verify_readiness(FoundingReadiness::Ordinary),
    )
    .await?;
    if request.request().driver.enrolled_peer().is_some() {
        run_progress_step(
            state,
            FoundingMilestone::DriverReadyReported,
            FoundingStep::ReportDriverReady,
            progress.ready(&request.request().driver),
        )
        .await?;
    }
    state
        .record_milestone(FoundingMilestone::Complete)
        .map_err(|source| FoundingFailure::State {
            step: FoundingStep::MarkComplete,
            source,
        })?;
    Ok(result)
}

fn run_host_step(
    state: &FoundingStateDirectory,
    milestone: FoundingMilestone,
    step: FoundingStep,
    effect: impl FnOnce() -> Result<(), FailureMessage>,
) -> Result<(), FoundingFailure> {
    if state
        .milestone_complete(milestone)
        .map_err(|source| FoundingFailure::State { step, source })?
    {
        return Ok(());
    }
    effect().map_err(|message| FoundingFailure::Host { step, message })?;
    state
        .record_milestone(milestone)
        .map_err(|source| FoundingFailure::State { step, source })
}

async fn run_control_step(
    state: &FoundingStateDirectory,
    milestone: FoundingMilestone,
    step: FoundingStep,
    effect: impl Future<Output = Result<(), FailureMessage>>,
) -> Result<(), FoundingFailure> {
    if state
        .milestone_complete(milestone)
        .map_err(|source| FoundingFailure::State { step, source })?
    {
        return Ok(());
    }
    effect
        .await
        .map_err(|message| FoundingFailure::ControlPlane { step, message })?;
    state
        .record_milestone(milestone)
        .map_err(|source| FoundingFailure::State { step, source })
}

async fn run_progress_step(
    state: &FoundingStateDirectory,
    milestone: FoundingMilestone,
    step: FoundingStep,
    effect: impl Future<Output = Result<(), FailureMessage>>,
) -> Result<(), FoundingFailure> {
    if state
        .milestone_complete(milestone)
        .map_err(|source| FoundingFailure::State { step, source })?
    {
        return Ok(());
    }
    effect
        .await
        .map_err(|message| FoundingFailure::Progress { step, message })?;
    state
        .record_milestone(milestone)
        .map_err(|source| FoundingFailure::State { step, source })
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use ployz_core::corrosion::{
        AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
        MachineDocument, MachineStorageSelection, MachineStorageSelectionReason, MachineTransport,
        MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerDocument, PeerTransport,
        StorageMode, derive_builtin_wireguard_member,
    };
    use ployz_core::founding::{FoundingDriverEnrollment, FoundingRepairCommand, FoundingRequest};
    use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
    use ployz_core::machine::{MachineLifecycle, MachineName};
    use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};

    use super::*;

    const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const OTHER_CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    const MACHINE_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    const PEER_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

    struct RecordingHost {
        calls: Vec<&'static str>,
        fail_at: Option<&'static str>,
        door_lock_state: Option<FoundingStateDirectory>,
    }

    impl RecordingHost {
        fn record(&mut self, name: &'static str) -> Result<(), FailureMessage> {
            self.calls.push(name);
            if self.fail_at == Some(name) {
                return Err(failure("injected host failure"));
            }
            Ok(())
        }
    }

    impl FoundingHostEffects for RecordingHost {
        fn stage_exact_ployz_and_corrosion(&mut self) -> Result<(), FailureMessage> {
            self.record("artifacts")
        }
        fn ensure_docker(&mut self) -> Result<(), FailureMessage> {
            self.record("docker")
        }
        fn ensure_machine_identity_and_wireguard(&mut self) -> Result<(), FailureMessage> {
            self.record("machine_material")
        }
        fn ensure_cluster_door_material(&mut self) -> Result<(), FailureMessage> {
            if let Some(state) = &self.door_lock_state {
                assert!(matches!(
                    state.try_lock(),
                    Err(FoundingStateError::InitInProgress)
                ));
            }
            self.record("door_material")
        }
        fn prepare_selected_storage(&mut self) -> Result<(), FailureMessage> {
            self.record("storage")
        }
        fn write_configuration_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
            self.record("configuration")
        }
        fn persist_validated_founding_request(&mut self) -> Result<(), FailureMessage> {
            self.record("founding_request")
        }
        fn restart_and_verify_docker_configuration(&mut self) -> Result<(), FailureMessage> {
            self.record("docker_configured")
        }
        fn install_units_and_enable_ready_roles(&mut self) -> Result<(), FailureMessage> {
            self.record("units_installed_ready_enabled")
        }
        fn start_keeper(&mut self) -> Result<(), FailureMessage> {
            self.record("keeper")
        }
        fn start_corrosion(&mut self) -> Result<(), FailureMessage> {
            self.record("corrosion")
        }
        fn start_api_with_bootstrap(&mut self) -> Result<(), FailureMessage> {
            self.record("bootstrap_api")
        }
        fn enable_and_start_dns(&mut self) -> Result<(), FailureMessage> {
            self.record("dns_started")
        }
        fn await_dns_readiness(&mut self) -> Result<(), FailureMessage> {
            self.record("dns_ready")
        }
        fn await_driver_peer_convergence(
            &mut self,
            _driver: &FoundingDriverEnrollment,
        ) -> Result<(), FailureMessage> {
            self.record("driver_peer")
        }
        fn remove_bootstrap_credential(&mut self) -> Result<(), FailureMessage> {
            self.record("remove_bootstrap")
        }
        fn restart_api_without_bootstrap(&mut self) -> Result<(), FailureMessage> {
            self.record("ordinary_api")
        }
    }

    struct RecordingControl {
        calls: Vec<&'static str>,
        fail_at: Option<&'static str>,
    }

    impl RecordingControl {
        fn record(&mut self, name: &'static str) -> Result<(), FailureMessage> {
            self.calls.push(name);
            if self.fail_at == Some(name) {
                return Err(failure("injected control failure"));
            }
            Ok(())
        }
    }

    impl FoundingControlPlane for RecordingControl {
        async fn submit_founding(
            &mut self,
            _request: &ValidatedFoundingRequest,
        ) -> Result<(), FailureMessage> {
            self.record("submit")
        }

        async fn await_endpoint_network_ready(
            &mut self,
            _request: &ValidatedFoundingRequest,
        ) -> Result<(), FailureMessage> {
            self.record("network_ready")
        }

        async fn verify_readiness(
            &mut self,
            readiness: FoundingReadiness,
        ) -> Result<(), FailureMessage> {
            match readiness {
                FoundingReadiness::Bootstrap => self.record("bootstrap_ready"),
                FoundingReadiness::Ordinary => self.record("ordinary_ready"),
            }
        }
    }

    struct RecordingProgress {
        calls: Vec<&'static str>,
        fail_ready: bool,
    }

    impl FoundingProgressObserver for RecordingProgress {
        async fn driver_enrolled(
            &mut self,
            _driver: &FoundingDriverEnrollment,
        ) -> Result<(), FailureMessage> {
            self.calls.push("enrolled");
            Ok(())
        }

        async fn ready(
            &mut self,
            _driver: &FoundingDriverEnrollment,
        ) -> Result<(), FailureMessage> {
            self.calls.push("ready");
            if self.fail_ready {
                return Err(failure("injected ready callback failure"));
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn founding_starts_dns_only_after_the_endpoint_network_is_ready() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let request = request(CLUSTER);
        let mut host = recording_host(None);
        let mut control = recording_control(None);

        let result = found_machine_one(&state, &request, &mut host, &mut control)
            .await
            .expect("founding succeeds");

        assert_eq!(result, FoundingResult::Found);
        assert_eq!(
            host.calls,
            [
                "artifacts",
                "docker",
                "machine_material",
                "founding_request",
                "door_material",
                "storage",
                "configuration",
                "docker_configured",
                "units_installed_ready_enabled",
                "keeper",
                "corrosion",
                "bootstrap_api",
                "dns_started",
                "dns_ready",
                "remove_bootstrap",
                "ordinary_api",
            ]
        );
        assert_eq!(
            control.calls,
            [
                "submit",
                "network_ready",
                "bootstrap_ready",
                "ordinary_ready"
            ]
        );
        assert_eq!(
            state.observe_arrival().expect("arrival reads"),
            ployz_core::founding::FoundingArrival::Complete {
                persisted_cluster_id: cluster_id(CLUSTER),
            }
        );
    }

    #[tokio::test]
    async fn door_material_effect_runs_while_the_founding_lock_is_held() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let mut host = recording_host(None);
        host.door_lock_state = Some(state.clone());
        let mut control = recording_control(None);

        found_machine_one(&state, &request(CLUSTER), &mut host, &mut control)
            .await
            .expect("founding succeeds under one lock");

        assert!(host.calls.contains(&"door_material"));
    }

    #[tokio::test]
    async fn resume_skips_durable_steps_and_continues_at_the_failure() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let request = request(CLUSTER);
        let mut first_host = recording_host(Some("storage"));
        let mut first_control = recording_control(None);

        let failure = found_machine_one(&state, &request, &mut first_host, &mut first_control)
            .await
            .expect_err("storage fails");
        assert_eq!(failure.step(), Some(FoundingStep::PrepareStorage));
        assert_eq!(
            first_host.calls,
            [
                "artifacts",
                "docker",
                "machine_material",
                "founding_request",
                "door_material",
                "storage",
            ]
        );
        assert!(first_control.calls.is_empty());

        let mut resumed_host = recording_host(None);
        let mut resumed_control = recording_control(None);
        let result = found_machine_one(&state, &request, &mut resumed_host, &mut resumed_control)
            .await
            .expect("resume succeeds");

        assert_eq!(result, FoundingResult::Resumed);
        assert_eq!(
            resumed_host.calls,
            [
                "storage",
                "configuration",
                "docker_configured",
                "units_installed_ready_enabled",
                "keeper",
                "corrosion",
                "bootstrap_api",
                "dns_started",
                "dns_ready",
                "remove_bootstrap",
                "ordinary_api",
            ]
        );
    }

    #[tokio::test]
    async fn dns_readiness_failure_resumes_at_the_udp_probe() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let request = request(CLUSTER);
        let mut first_host = recording_host(Some("dns_ready"));
        let mut first_control = recording_control(None);

        let failure = found_machine_one(&state, &request, &mut first_host, &mut first_control)
            .await
            .expect_err("DNS readiness fails");

        assert_eq!(failure.step(), Some(FoundingStep::VerifyDnsReadiness));
        assert!(first_host.calls.ends_with(&["dns_started", "dns_ready"]));

        let mut resumed_host = recording_host(None);
        let mut resumed_control = recording_control(None);
        found_machine_one(&state, &request, &mut resumed_host, &mut resumed_control)
            .await
            .expect("DNS readiness resumes");

        assert_eq!(
            resumed_host.calls,
            ["dns_ready", "remove_bootstrap", "ordinary_api"]
        );
        assert_eq!(resumed_control.calls, ["bootstrap_ready", "ordinary_ready"]);
    }

    #[tokio::test]
    async fn foreign_and_joined_state_refuse_before_any_effect() {
        for joined in [false, true] {
            let directory = tempfile::tempdir().expect("tempdir");
            let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
            state
                .persist_cluster_id_exclusive(&cluster_id(OTHER_CLUSTER))
                .expect("foreign cluster persists");
            if joined {
                std::fs::write(directory.path().join("join-complete"), b"complete\n")
                    .expect("join marker writes");
            }
            let mut host = recording_host(None);
            let mut control = recording_control(None);

            let error = found_machine_one(&state, &request(CLUSTER), &mut host, &mut control)
                .await
                .expect_err("foreign state refuses");

            assert!(matches!(
                error,
                FoundingFailure::Refused(FoundingRefusal::ForeignState {
                    repair_command: FoundingRepairCommand::ResetMachine,
                    ..
                })
            ));
            assert!(host.calls.is_empty());
            assert!(control.calls.is_empty());
        }
    }

    #[tokio::test]
    async fn complete_state_is_a_no_op_without_readiness_or_host_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        state
            .persist_cluster_id_exclusive(&cluster_id(CLUSTER))
            .expect("cluster persists");
        state
            .record_milestone(FoundingMilestone::Complete)
            .expect("complete marker writes");
        let mut host = recording_host(None);
        let mut control = recording_control(None);

        assert_eq!(
            found_machine_one(&state, &request(CLUSTER), &mut host, &mut control)
                .await
                .expect("no-op succeeds"),
            FoundingResult::NoOp
        );
        assert!(host.calls.is_empty());
        assert!(control.calls.is_empty());
    }

    #[tokio::test]
    async fn ready_callback_failure_resumes_before_complete_without_repeating_prior_work() {
        let directory = tempfile::tempdir().expect("tempdir");
        let state = FoundingStateDirectory::open(directory.path()).expect("state directory");
        let request = assisted_request(CLUSTER);
        let mut first_host = recording_host(None);
        let mut first_control = recording_control(None);
        let mut first_progress = RecordingProgress {
            calls: Vec::new(),
            fail_ready: true,
        };

        let error = found_machine_one_with_progress(
            &state,
            &request,
            &mut first_host,
            &mut first_control,
            &mut first_progress,
        )
        .await
        .expect_err("Ready callback fails");

        assert_eq!(error.step(), Some(FoundingStep::ReportDriverReady));
        assert_eq!(first_progress.calls, ["enrolled", "ready"]);
        assert_eq!(
            state.observe_arrival().expect("arrival reads"),
            ployz_core::founding::FoundingArrival::Partial {
                persisted_cluster_id: cluster_id(CLUSTER),
            }
        );

        let mut resumed_host = recording_host(None);
        let mut resumed_control = recording_control(None);
        let mut resumed_progress = RecordingProgress {
            calls: Vec::new(),
            fail_ready: false,
        };
        let result = found_machine_one_with_progress(
            &state,
            &request,
            &mut resumed_host,
            &mut resumed_control,
            &mut resumed_progress,
        )
        .await
        .expect("Ready callback resumes");

        assert_eq!(result, FoundingResult::Resumed);
        assert!(resumed_host.calls.is_empty());
        assert!(resumed_control.calls.is_empty());
        assert_eq!(resumed_progress.calls, ["ready"]);
        assert_eq!(
            state.observe_arrival().expect("arrival reads"),
            ployz_core::founding::FoundingArrival::Complete {
                persisted_cluster_id: cluster_id(CLUSTER),
            }
        );
    }

    fn recording_host(fail_at: Option<&'static str>) -> RecordingHost {
        RecordingHost {
            calls: Vec::new(),
            fail_at,
            door_lock_state: None,
        }
    }

    fn recording_control(fail_at: Option<&'static str>) -> RecordingControl {
        RecordingControl {
            calls: Vec::new(),
            fail_at,
        }
    }

    fn cluster_id(value: &str) -> ClusterId {
        ClusterId::try_new(value).expect("fixture cluster id")
    }

    fn request(cluster: &str) -> ValidatedFoundingRequest {
        let cluster_id = cluster_id(cluster);
        let machine_id = MachineRowId::try_new(MACHINE).expect("fixture machine id");
        let pubkey = WireGuardPublicKey::try_new(MACHINE_KEY).expect("fixture machine key");
        let provenance = OperatorWriteProvenance {
            written_by: OperationInitiator::Machine {
                machine_id: machine_id.clone(),
            },
            written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z")
                .expect("fixture timestamp"),
        };
        FoundingRequest {
            cluster_id: cluster_id.clone(),
            cluster: ClusterDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance: provenance.clone(),
                name: "ares".to_owned(),
                storage_default: StorageMode::Plain,
                hostname_mode: AutomaticHostnameMode::Disabled,
                prefix: MachineEndpointSupernet::default_v1(),
                provider: MeshProvider::BuiltinWireguard,
                acme_directory_url: "https://acme.example/directory".to_owned(),
                acme_contact: None,
            },
            machine_id,
            machine: MachineDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: cluster_id.clone(),
                provenance,
                name: MachineName::try_new("ares").expect("machine name"),
                lifecycle: MachineLifecycle::Active,
                transport: MachineTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&cluster_id, &pubkey)
                        .bind_address()
                        .get(),
                    pubkey,
                    endpoint: Some((Ipv4Addr::new(203, 0, 113, 7), 51820).into()),
                    subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24")
                        .expect("first subnet"),
                },
                storage: MachineStorageSelection {
                    mode: StorageMode::Plain,
                    reason: MachineStorageSelectionReason::Default,
                },
            },
            driver: FoundingDriverEnrollment::OnHost,
        }
        .try_validate()
        .expect("fixture validates")
    }

    fn assisted_request(cluster: &str) -> ValidatedFoundingRequest {
        let mut request = request(cluster).into_request();
        let peer_id = PeerId::try_new(PEER).expect("peer id");
        let public_key = WireGuardPublicKey::try_new(PEER_KEY).expect("peer key");
        request.driver = FoundingDriverEnrollment::Ssh {
            peer_id,
            document: PeerDocument {
                v: CorrosionDocumentVersion::V1,
                cluster_id: request.cluster_id.clone(),
                provenance: request.machine.provenance.clone(),
                name: "operator-laptop".to_owned(),
                transport: PeerTransport::Wireguard {
                    addr_v6: derive_builtin_wireguard_member(&request.cluster_id, &public_key)
                        .bind_address()
                        .get(),
                    pubkey: public_key,
                    endpoint: None,
                },
            },
        };
        request.try_validate().expect("assisted fixture validates")
    }

    fn failure(message: &str) -> FailureMessage {
        FailureMessage::try_new(message).expect("non-empty failure")
    }
}
