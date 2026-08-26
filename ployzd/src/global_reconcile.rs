//! Machine-local convergence for Global Service slots.

use std::{fmt::Display, time::Duration};

use chrono::{SecondsFormat, Utc};
use ployz_core::{
    ContainerId, ContainerObservation, GlobalReconcileFailureObservation, Machine,
    MachineStorageObservation, ObservedGlobalSlotSpec, ServicePlacementEligibility,
    derive_services, service_placement_eligibility,
};
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::ReplicatedStore,
    machine::{LocalMachine, LocalMachineError},
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Publishes the latest Machine-local Global reconcile failures.
pub(crate) type GlobalReconcilePublisher = watch::Sender<Vec<GlobalReconcileFailureObservation>>;

/// Receives the latest Machine-local Global reconcile failures.
pub(crate) type GlobalReconcileObservations =
    watch::Receiver<Vec<GlobalReconcileFailureObservation>>;

/// Create the daemon's single latest-value Global reconcile observation channel.
#[must_use]
pub(crate) fn global_reconcile_observation_channel()
-> (GlobalReconcilePublisher, GlobalReconcileObservations) {
    watch::channel(Vec::new())
}

trait GlobalSlotReconciler {
    type Error: Display;

    async fn ensure_global_slot(&self, slot: &ObservedGlobalSlotSpec) -> Result<(), Self::Error>;
    async fn retire_global_slot(&self, container_id: &ContainerId) -> Result<(), Self::Error>;
}

impl GlobalSlotReconciler for LocalMachine {
    type Error = LocalMachineError;

    async fn ensure_global_slot(&self, slot: &ObservedGlobalSlotSpec) -> Result<(), Self::Error> {
        LocalMachine::ensure_global_slot(self, &slot.identity().project, slot.resolved_spec())
            .await
            .map(|_| ())
    }

    async fn retire_global_slot(&self, container_id: &ContainerId) -> Result<(), Self::Error> {
        LocalMachine::retire_global_slot(self, container_id).await
    }
}

/// Failure while driving the Machine-local Global reconcile loop.
#[derive(Debug, Error)]
pub(crate) enum RunError {
    /// The daemon's participation signal ended before shutdown.
    #[error("participation signal closed")]
    ParticipationSignalClosed(#[source] watch::error::RecvError),
}

/// Converge this participating Machine's eligible Global slots.
///
/// Runs after participation changes and on a slow periodic tick until shutdown.
///
/// # Errors
///
/// Returns [`RunError::ParticipationSignalClosed`] if the participation signal
/// closes before shutdown.
pub(crate) async fn run(
    store: ReplicatedStore,
    reconciler: LocalMachine,
    observations: GlobalReconcilePublisher,
    mut participating: watch::Receiver<bool>,
    shutdown: CancellationToken,
) -> Result<(), RunError> {
    let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let reconcile = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            changed = participating.changed() => match changed {
                Ok(()) => *participating.borrow(),
                Err(_) if shutdown.is_cancelled() => return Ok(()),
                Err(error) => return Err(RunError::ParticipationSignalClosed(error)),
            },
            _ = interval.tick() => *participating.borrow(),
        };
        if reconcile {
            reconcile_store(&store, &reconciler, &observations).await;
        }
    }
}

async fn reconcile_store(
    store: &ReplicatedStore,
    reconciler: &LocalMachine,
    observations: &GlobalReconcilePublisher,
) {
    let record = match reconciler.record() {
        Ok(record) => record,
        Err(error) => {
            eprintln!("failed to read local Machine for Global reconciliation: {error}");
            return;
        }
    };
    let Some(machine) = record.machine().cloned() else {
        eprintln!(
            "failed to read local Machine for Global reconciliation: Machine is not participating"
        );
        return;
    };
    let storage = reconciler.observe_storage().await;
    match store.containers().await {
        Ok(snapshot) => {
            observations.send_replace(
                reconcile_global_slots(
                    &snapshot.observations,
                    &machine,
                    storage.as_ref(),
                    reconciler,
                    &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                )
                .await,
            );
        }
        Err(error) => eprintln!("failed to read Globals for local reconciliation: {error}"),
    }
}

async fn reconcile_global_slots<R: GlobalSlotReconciler>(
    containers: &[ContainerObservation],
    machine: &Machine,
    storage: Option<&MachineStorageObservation>,
    reconciler: &R,
    observed_at: &str,
) -> Vec<GlobalReconcileFailureObservation> {
    let mut failures = Vec::new();
    let services = derive_services(containers.iter().cloned());
    for service in &services {
        let Some(slot) = service.observed_global_slot() else {
            continue;
        };
        let eligibility = service_placement_eligibility(
            &slot.resolved_spec().placement,
            &slot.resolved_spec().volume_graph,
            machine,
            storage,
        );
        match eligibility {
            ServicePlacementEligibility::Eligible => {
                if let Err(error) = reconciler.ensure_global_slot(&slot).await {
                    failures.push(reconcile_failure(&slot, error, observed_at));
                }
            }
            ServicePlacementEligibility::Ineligible => {
                for container in service
                    .containers
                    .iter()
                    .filter(|container| container.as_observation().machine_id == machine.id)
                {
                    if let Err(error) = reconciler
                        .retire_global_slot(&container.as_observation().container_id)
                        .await
                    {
                        failures.push(reconcile_failure(&slot, error, observed_at));
                    }
                }
            }
            ServicePlacementEligibility::Unknown => {}
        }
    }
    failures
}

fn reconcile_failure(
    slot: &ObservedGlobalSlotSpec,
    error: impl Display,
    observed_at: &str,
) -> GlobalReconcileFailureObservation {
    GlobalReconcileFailureObservation {
        service: slot.identity().clone(),
        last_error: error.to_string(),
        observed_at: observed_at.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::Ipv6Addr,
        sync::{Arc, Mutex},
    };

    use ployz_core::{
        ContainerId, ContainerKind, ContainerRuntimeObservation, HealthObservation, MachineId,
        MachineName, ManagementAddress, Placement, ProjectName, QualifiedService,
        ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceId, ServiceMode, ServiceName,
        WireGuardPublicKey,
    };
    use serde_json::json;
    use tokio::sync::watch;

    use super::*;

    #[tokio::test]
    async fn run_reports_a_closed_participation_signal() {
        let (store, server) = crate::corrosion::fake_cluster::store().await;
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-global-reconcile-{}", MachineId::random()));
        let local = Arc::new(Mutex::new(
            crate::machine::LocalMachineStore::open(&data_dir).unwrap(),
        ));
        let (restart, _) = watch::channel(false);
        let reconciler = LocalMachine::new(local, restart);
        let (participating, participating_rx) = watch::channel(false);
        let (observations, _) = global_reconcile_observation_channel();
        drop(participating);

        let error = run(
            store,
            reconciler,
            observations,
            participating_rx,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, RunError::ParticipationSignalClosed(_)));
        server.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn effect_ensures_eligible_retires_ineligible_and_retries_unknown() {
        let local = machine('1', "local");
        let mut slot = observation(
            &local,
            'a',
            "app",
            "api",
            ServiceMode::Global,
            Placement::default(),
        );
        add_provisioned_mount(&mut slot.resolved_spec);
        let reconciler = FakeReconciler {
            calls: Mutex::new(Vec::new()),
            retired: Mutex::new(Vec::new()),
            failing: Mutex::new(None),
        };

        let unknown = reconcile_global_slots(
            std::slice::from_ref(&slot),
            &local,
            None,
            &reconciler,
            "2026-08-24T20:00:00Z",
        )
        .await;
        assert!(unknown.is_empty());
        assert!(reconciler.calls.lock().unwrap().is_empty());
        assert!(reconciler.retired.lock().unwrap().is_empty());

        let ready = reconcile_global_slots(
            std::slice::from_ref(&slot),
            &local,
            Some(&ployz_core::MachineStorageObservation::Ready),
            &reconciler,
            "2026-08-24T20:05:00Z",
        )
        .await;
        assert!(ready.is_empty());
        assert_eq!(reconciler.calls.lock().unwrap().as_slice(), ["app/api"]);

        reconciler.calls.lock().unwrap().clear();
        let stateless = reconcile_global_slots(
            std::slice::from_ref(&slot),
            &local,
            Some(&ployz_core::MachineStorageObservation::Stateless),
            &reconciler,
            "2026-08-24T20:10:00Z",
        )
        .await;
        assert!(stateless.is_empty());
        assert!(reconciler.calls.lock().unwrap().is_empty());
        assert_eq!(
            reconciler.retired.lock().unwrap().as_slice(),
            [slot.container_id]
        );
    }

    #[tokio::test]
    async fn effect_reports_ensure_failures() {
        let local = machine('1', "local");
        let peer = machine('2', "peer");
        let containers = [observation(
            &peer,
            'c',
            "ployz-system",
            "ingress",
            ServiceMode::Global,
            Placement::default(),
        )];
        let reconciler = FakeReconciler {
            calls: Mutex::new(Vec::new()),
            retired: Mutex::new(Vec::new()),
            failing: Mutex::new(Some("ployz-system/ingress")),
        };

        let failures = reconcile_global_slots(
            &containers,
            &local,
            None,
            &reconciler,
            "2026-08-24T20:00:00Z",
        )
        .await;

        assert_eq!(
            reconciler.calls.lock().unwrap().as_slice(),
            ["ployz-system/ingress"]
        );
        assert_eq!(
            failures,
            [ployz_core::GlobalReconcileFailureObservation {
                service: QualifiedService::system_ingress(),
                last_error: "pull failed".into(),
                observed_at: "2026-08-24T20:00:00Z".into(),
            }]
        );
    }

    struct FakeReconciler {
        calls: Mutex<Vec<String>>,
        retired: Mutex<Vec<ContainerId>>,
        failing: Mutex<Option<&'static str>>,
    }

    impl GlobalSlotReconciler for FakeReconciler {
        type Error = RpcError;

        async fn ensure_global_slot(
            &self,
            slot: &ObservedGlobalSlotSpec,
        ) -> Result<(), Self::Error> {
            let identity = slot.identity().to_string();
            self.calls.lock().unwrap().push(identity.clone());
            if self.failing.lock().unwrap().as_ref() == Some(&identity.as_str()) {
                Err(RpcError {
                    code: RpcErrorCode::Internal,
                    message: "pull failed".into(),
                    details: serde_json::Value::Null,
                })
            } else {
                Ok(())
            }
        }

        async fn retire_global_slot(&self, container_id: &ContainerId) -> Result<(), Self::Error> {
            self.retired.lock().unwrap().push(*container_id);
            Ok(())
        }
    }

    fn add_provisioned_mount(spec: &mut ResolvedServiceSpec) {
        use std::num::NonZeroU64;

        use ployz_core::{
            ContainerPath, DockerVolumeName, ProvisionedVolumeMaximumBytes, ServiceMount,
            ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, VolumeSource,
        };

        let reference = ServiceVolumeReference::parse("data").unwrap();
        spec.volume_graph = ServiceVolumeGraph::parse(
            vec![ServiceVolume {
                reference: reference.clone(),
                source: VolumeSource::Provisioned {
                    name: DockerVolumeName::parse("app_data").unwrap(),
                    maximum_bytes: ProvisionedVolumeMaximumBytes::new(
                        NonZeroU64::new(100).unwrap(),
                    ),
                    labels: Default::default(),
                },
            }],
            vec![ServiceMount {
                volume: reference,
                target: ContainerPath::parse("/data").unwrap(),
                read_only: false,
                no_copy: false,
                subpath: None,
            }],
        )
        .unwrap();
    }

    fn machine(id: char, name: &str) -> Machine {
        Machine {
            id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", id.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([id as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        }
    }

    fn observation(
        machine: &Machine,
        id: char,
        project: &str,
        name: &str,
        mode: ServiceMode,
        placement: Placement,
    ) -> ContainerObservation {
        let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
        let service_name = ServiceName::parse(name).unwrap();
        let mode = serde_json::to_value(mode).unwrap();
        let mut spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": mode,
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap();
        spec.placement = placement;
        ContainerObservation {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: format!("{name}-{id}"),
            created_at_unix_nanos: 1,
            machine_id: machine.id,
            project_name: ProjectName::parse(project).unwrap(),
            service_id,
            service_name,
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: spec,
            address: None,
            labels: Default::default(),
        }
    }
}
