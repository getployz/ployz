//! Machine-local, add-only convergence for Global Service slots.

use std::{fmt::Display, time::Duration};

use chrono::{SecondsFormat, Utc};
use ployz_core::{
    ContainerObservation, GlobalReconcileFailureObservation, Machine, ObservedGlobalSlotSpec,
    derive_services, missing_global_slots,
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

trait GlobalSlotEnsurer {
    type Error: Display;

    async fn ensure_global_slot(&self, slot: &ObservedGlobalSlotSpec) -> Result<(), Self::Error>;
}

impl GlobalSlotEnsurer for LocalMachine {
    type Error = LocalMachineError;

    async fn ensure_global_slot(&self, slot: &ObservedGlobalSlotSpec) -> Result<(), Self::Error> {
        LocalMachine::ensure_global_slot(self, &slot.identity().project, slot.resolved_spec())
            .await
            .map(|_| ())
    }
}

/// Failure while driving the Machine-local Global reconcile loop.
#[derive(Debug, Error)]
pub(crate) enum RunError {
    /// The daemon's participation signal ended before shutdown.
    #[error("participation signal closed")]
    ParticipationSignalClosed(#[source] watch::error::RecvError),
}

/// Converge this participating Machine's missing eligible Global slots.
///
/// Runs after participation changes and on a slow periodic tick until shutdown.
///
/// # Errors
///
/// Returns [`RunError::ParticipationSignalClosed`] if the participation signal
/// closes before shutdown.
pub(crate) async fn run(
    store: ReplicatedStore,
    ensurer: LocalMachine,
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
            reconcile_store(&store, &ensurer, &observations).await;
        }
    }
}

async fn reconcile_store(
    store: &ReplicatedStore,
    ensurer: &LocalMachine,
    observations: &GlobalReconcilePublisher,
) {
    let record = match ensurer.record() {
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
    match store.containers().await {
        Ok(snapshot) => {
            observations.send_replace(
                ensure_missing_global_slots(
                    &snapshot.observations,
                    &machine,
                    ensurer,
                    &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                )
                .await,
            );
        }
        Err(error) => eprintln!("failed to read Globals for local reconciliation: {error}"),
    }
}

async fn ensure_missing_global_slots<E: GlobalSlotEnsurer>(
    containers: &[ContainerObservation],
    machine: &Machine,
    ensurer: &E,
    observed_at: &str,
) -> Vec<GlobalReconcileFailureObservation> {
    let mut failures = Vec::new();
    let services = derive_services(containers.iter().cloned());
    for slot in missing_global_slots(&services, machine) {
        if let Err(last_error) = ensurer.ensure_global_slot(&slot).await {
            failures.push(GlobalReconcileFailureObservation {
                service: slot.identity().clone(),
                last_error: last_error.to_string(),
                observed_at: observed_at.into(),
            });
        }
    }
    failures
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
        let ensurer = LocalMachine::new(local, restart);
        let (participating, participating_rx) = watch::channel(false);
        let (observations, _) = global_reconcile_observation_channel();
        drop(participating);

        let error = run(
            store,
            ensurer,
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
    async fn effect_ensures_each_selected_slot_and_reports_only_failures() {
        let local = machine('1', "local");
        let peer = machine('2', "peer");
        let containers = [
            observation(
                &peer,
                'a',
                "app",
                "api",
                ServiceMode::Global,
                Placement::default(),
            ),
            observation(
                &peer,
                'c',
                "ployz-system",
                "caddy",
                ServiceMode::Global,
                Placement::default(),
            ),
        ];
        let ensurer = FakeEnsurer {
            calls: Mutex::new(Vec::new()),
            failing: Mutex::new(Some("ployz-system/caddy")),
        };

        let failures =
            ensure_missing_global_slots(&containers, &local, &ensurer, "2026-08-24T20:00:00Z")
                .await;

        let mut calls = ensurer.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(calls, ["app/api", "ployz-system/caddy"]);
        assert_eq!(
            failures,
            [ployz_core::GlobalReconcileFailureObservation {
                service: QualifiedService::system_caddy(),
                last_error: "pull failed".into(),
                observed_at: "2026-08-24T20:00:00Z".into(),
            }]
        );
    }

    struct FakeEnsurer {
        calls: Mutex<Vec<String>>,
        failing: Mutex<Option<&'static str>>,
    }

    impl GlobalSlotEnsurer for FakeEnsurer {
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
