//! Machine-local, add-only convergence for Global Service slots.

use std::{io, time::Duration};

use chrono::{SecondsFormat, Utc};
use ployz_core::{
    ContainerObservation, GlobalReconcileFailureObservation, GlobalServiceSlot, Machine, RpcError,
    derive_services, missing_global_slots,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{corrosion::ReplicatedStore, rpc::MachineService};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub(crate) type GlobalReconcileObservations = watch::Sender<Vec<GlobalReconcileFailureObservation>>;

#[must_use]
pub(crate) fn global_reconcile_observations() -> GlobalReconcileObservations {
    watch::channel(Vec::new()).0
}

trait GlobalSlotEnsurer {
    async fn ensure_global_slot(&self, slot: &GlobalServiceSlot) -> Result<(), RpcError>;
}

impl GlobalSlotEnsurer for MachineService {
    async fn ensure_global_slot(&self, slot: &GlobalServiceSlot) -> Result<(), RpcError> {
        self.ensure_local_global_slot(&slot.identity, &slot.spec)
            .await
            .map(|_| ())
    }
}

pub(crate) async fn run(
    store: ReplicatedStore,
    ensurer: MachineService,
    observations: GlobalReconcileObservations,
    mut participating: watch::Receiver<bool>,
    shutdown: CancellationToken,
) -> io::Result<()> {
    let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let reconcile = tokio::select! {
            biased;
            () = shutdown.cancelled() => return Ok(()),
            changed = participating.changed() => match changed {
                Ok(()) => *participating.borrow(),
                Err(_) if shutdown.is_cancelled() => return Ok(()),
                Err(error) => return Err(io::Error::other(error)),
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
    ensurer: &MachineService,
    observations: &GlobalReconcileObservations,
) {
    let machine = match ensurer.local_machine() {
        Ok(machine) => machine,
        Err(error) => {
            eprintln!("failed to read local Machine for Global reconciliation: {error}");
            return;
        }
    };
    match store.containers().await {
        Ok(snapshot) => {
            reconcile_and_publish(
                &snapshot.observations,
                &machine,
                ensurer,
                observations,
                &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            )
            .await;
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
                service: slot.identity,
                last_error: last_error.to_string(),
                observed_at: observed_at.into(),
            });
        }
    }
    failures
}

async fn reconcile_and_publish<E: GlobalSlotEnsurer>(
    containers: &[ContainerObservation],
    machine: &Machine,
    ensurer: &E,
    observations: &GlobalReconcileObservations,
    observed_at: &str,
) {
    observations
        .send_replace(ensure_missing_global_slots(containers, machine, ensurer, observed_at).await);
}

#[cfg(test)]
mod tests {
    use std::{net::Ipv6Addr, sync::Mutex};

    use ployz_core::{
        ContainerId, ContainerKind, ContainerRuntimeObservation, HealthObservation, MachineId,
        MachineName, ManagementAddress, Placement, ProjectName, QualifiedService,
        ResolvedServiceSpec, RpcErrorCode, ServiceId, ServiceMode, ServiceName, WireGuardPublicKey,
    };
    use serde_json::json;

    use super::*;

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

    #[tokio::test]
    async fn successful_pass_clears_the_previous_failure_observation() {
        let local = machine('1', "local");
        let peer = machine('2', "peer");
        let containers = [observation(
            &peer,
            'a',
            "app",
            "api",
            ServiceMode::Global,
            Placement::default(),
        )];
        let ensurer = FakeEnsurer {
            calls: Mutex::new(Vec::new()),
            failing: Mutex::new(Some("app/api")),
        };
        let observations = global_reconcile_observations();

        reconcile_and_publish(
            &containers,
            &local,
            &ensurer,
            &observations,
            "2026-08-24T20:00:00Z",
        )
        .await;
        assert_eq!(observations.borrow().len(), 1);

        *ensurer.failing.lock().unwrap() = None;
        reconcile_and_publish(
            &containers,
            &local,
            &ensurer,
            &observations,
            "2026-08-24T20:05:00Z",
        )
        .await;
        assert!(observations.borrow().is_empty());
    }

    struct FakeEnsurer {
        calls: Mutex<Vec<String>>,
        failing: Mutex<Option<&'static str>>,
    }

    impl GlobalSlotEnsurer for FakeEnsurer {
        async fn ensure_global_slot(&self, slot: &GlobalServiceSlot) -> Result<(), RpcError> {
            let identity = slot.identity.to_string();
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
