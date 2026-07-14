//! Controller-owned passive runtime projection fanout.

use crate::fact_cache::FactCache;
use crate::intent::service::{IntentReadError, NatsIntentReader};
use crate::operation_api::runtime_snapshot_from_sources;
use futures_util::StreamExt;
use ployz_core::state::IntentSnapshot;
use ployz_core::subjects::{INTENT_CHANGED, RUNTIME_SNAPSHOT_STREAM};
use ployz_sdk_types::RuntimeSnapshot;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::watch;
use tokio::task::JoinHandle;

pub(crate) struct RunningRuntimeProjection {
    projection_task: JoinHandle<()>,
    publisher_task: JoinHandle<()>,
}

impl RunningRuntimeProjection {
    pub(crate) async fn shutdown(self) {
        self.projection_task.abort();
        self.publisher_task.abort();
        let _ = self.projection_task.await;
        let _ = self.publisher_task.await;
    }
}

struct PassiveRuntimeProjection {
    intent: IntentSnapshot,
    facts: FactCache,
    snapshots: watch::Sender<RuntimeSnapshot>,
}

impl PassiveRuntimeProjection {
    fn new(intent: IntentSnapshot, facts: FactCache) -> (Self, watch::Receiver<RuntimeSnapshot>) {
        let snapshot = assemble_snapshot(intent.clone(), &facts);
        let (snapshots, receiver) = watch::channel(snapshot);
        (
            Self {
                intent,
                facts,
                snapshots,
            },
            receiver,
        )
    }

    fn replace_intent(&mut self, intent: IntentSnapshot) {
        self.intent = intent;
        self.refresh();
    }

    fn refresh(&self) {
        self.snapshots
            .send_replace(assemble_snapshot(self.intent.clone(), &self.facts));
    }
}

pub(crate) async fn start_runtime_projection(
    client: async_nats::Client,
    intent_reader: NatsIntentReader,
    facts: FactCache,
) -> Result<RunningRuntimeProjection, RuntimeProjectionStartError> {
    let intent_changes = client.subscribe(INTENT_CHANGED).await.map_err(|error| {
        RuntimeProjectionStartError::SubscribeIntent {
            message: error.to_string(),
        }
    })?;
    let initial_intent = intent_reader
        .intent()
        .await
        .map_err(RuntimeProjectionStartError::LoadInitialIntent)?;
    let fact_changes = facts.subscribe_changes();
    let (projection, snapshots) = PassiveRuntimeProjection::new(initial_intent, facts);
    let projection_task = tokio::spawn(run_projection(
        projection,
        intent_reader,
        intent_changes,
        fact_changes,
    ));
    let publisher_task = tokio::spawn(publish_snapshots(client, snapshots));

    Ok(RunningRuntimeProjection {
        projection_task,
        publisher_task,
    })
}

async fn run_projection(
    mut projection: PassiveRuntimeProjection,
    intent_reader: NatsIntentReader,
    mut intent_changes: async_nats::Subscriber,
    mut fact_changes: watch::Receiver<u64>,
) {
    let mut consecutive_intent_failures = 0_u64;
    loop {
        tokio::select! {
            result = fact_changes.changed() => {
                if result.is_err() {
                    break;
                }
                projection.refresh();
            }
            message = intent_changes.next() => {
                let Some(_) = message else {
                    break;
                };
                match intent_reader.intent().await {
                    Ok(intent) => {
                        consecutive_intent_failures = 0;
                        projection.replace_intent(intent);
                    }
                    Err(error) => warn_intent_failure(&mut consecutive_intent_failures, &error),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::dataplane::{DataplaneProjection, MachineEndpointSubnet, WireGuardPublicKey};
    use ployz_core::machine::MachineName;
    use ployz_core::machine_runtime::{MachineContainerObservationSnapshot, MachineFactsSnapshot};
    use ployz_core::roles::InstallRolePolicy;
    use ployz_core::state::{
        ActiveMachineState, ControlPlaneEpoch, GatewayServingStatus, GatewayStatusObservation,
        MachineLifecycle, ManagedLeaseProjection,
    };
    use ployz_sdk_types::{MachineTestimony, RuntimeDerivedCollectionStatus};
    use ployz_test_support::containers;
    use ployz_test_support::fixtures::{serving_target_entry, test_disk_space};
    use ployz_test_support::ids::{machine_id, operation_id};

    #[test]
    fn initial_passive_projection_is_complete() {
        let facts = FactCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (_projection, snapshots) = PassiveRuntimeProjection::new(
            intent(vec![serving_target_entry("svc_api", "entry_2")]),
            facts,
        );
        let snapshot = snapshots.borrow();

        assert_eq!(snapshot.machines.len(), 1);
        assert_eq!(snapshot.containers[0].container_id.as_str(), "ctr_initial");
        assert_eq!(
            snapshot.projection_sources.revisions.status,
            RuntimeDerivedCollectionStatus::Complete
        );
    }

    #[tokio::test]
    async fn intent_machine_and_gateway_replacements_emit_full_snapshots() {
        let facts = FactCache::default();
        facts.record_machine_facts(machine_facts("machine_a", "ctr_initial"));
        let (mut projection, mut snapshots) =
            PassiveRuntimeProjection::new(intent(Vec::new()), facts.clone());

        projection.replace_intent(intent(vec![serving_target_entry("svc_api", "entry_2")]));
        snapshots.changed().await.expect("intent replacement");
        assert_eq!(snapshots.borrow_and_update().services.len(), 1);

        facts.record_machine_facts(machine_facts("machine_a", "ctr_replacement"));
        projection.refresh();
        snapshots.changed().await.expect("machine replacement");
        assert_eq!(
            snapshots.borrow_and_update().containers[0]
                .container_id
                .as_str(),
            "ctr_replacement"
        );

        facts.record_gateway_status(GatewayStatusObservation {
            machine_id: machine_id("machine_a"),
            listen_addr: "127.0.0.1:443".parse().expect("socket address"),
            serving: GatewayServingStatus::Current,
            route_count: 3,
        });
        projection.refresh();
        snapshots.changed().await.expect("gateway replacement");
        let MachineTestimony::Answered { gateway, .. } =
            &snapshots.borrow_and_update().machines[0].testimony
        else {
            panic!("machine testimony answered");
        };
        assert_eq!(gateway.as_ref().expect("gateway testimony").route_count, 3);
    }

    fn intent(
        serving_target_entries: Vec<ployz_core::state::ServingTargetEntry>,
    ) -> IntentSnapshot {
        let machine = active_machine();
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: machine.machine_id.clone(),
            dataplane_projection: DataplaneProjection::try_new(Vec::new(), None)
                .expect("dataplane projection"),
            active_machines: vec![machine],
            route_bindings: Vec::new(),
            serving_target_entries,
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            managed_lease: ManagedLeaseProjection::Unacquired,
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
        }
    }

    fn active_machine() -> ActiveMachineState {
        ActiveMachineState {
            machine_id: machine_id("machine_a"),
            name: MachineName::try_new("machine-a").expect("machine name"),
            activated_by: operation_id("op_machine_add"),
            roles: InstallRolePolicy::install_all(),
            lifecycle: MachineLifecycle::Active,
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: MachineEndpointSubnet::try_new("10.198.0.0/24")
                .expect("endpoint subnet"),
            wireguard_public_key: WireGuardPublicKey::try_new("public-machine-a")
                .expect("public key"),
        }
    }

    fn machine_facts(machine: &str, container: &str) -> MachineFactsSnapshot {
        let machine_id = machine_id(machine);
        let observation = containers::observation(machine, container)
            .with(containers::identity("svc_api").entry("entry_2"))
            .running_unroutable()
            .build();
        MachineFactsSnapshot::try_new(
            machine_id.clone(),
            MachineContainerObservationSnapshot::try_new(machine_id, [observation])
                .expect("container snapshot"),
            None,
            test_disk_space(),
            ployz_core::image::OciPlatform::current(),
            1_000,
        )
        .expect("machine facts")
    }
}

async fn publish_snapshots(
    client: async_nats::Client,
    mut snapshots: watch::Receiver<RuntimeSnapshot>,
) {
    let mut consecutive_failures = 0_u64;
    loop {
        let snapshot = snapshots.borrow_and_update().clone();
        match serde_json::to_vec(&snapshot) {
            Ok(payload) => match client
                .publish(RUNTIME_SNAPSHOT_STREAM, payload.into())
                .await
            {
                Ok(()) => consecutive_failures = 0,
                Err(error) => warn_publish_failure(&mut consecutive_failures, "publish", &error),
            },
            Err(error) => warn_publish_failure(&mut consecutive_failures, "encode", &error),
        }
        if snapshots.changed().await.is_err() {
            break;
        }
    }
}

fn assemble_snapshot(intent: IntentSnapshot, facts: &FactCache) -> RuntimeSnapshot {
    let (machine_facts, gateway_statuses) = facts.runtime_projection_facts();
    runtime_snapshot_from_sources(
        intent,
        &machine_facts,
        &gateway_statuses,
        current_unix_seconds(),
    )
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn warn_intent_failure(consecutive_failures: &mut u64, error: &IntentReadError) {
    *consecutive_failures = consecutive_failures.saturating_add(1);
    eprintln!(
        "ployzd runtime projection warning: phase=load-intent consecutive_failures={} error={error}",
        *consecutive_failures
    );
}

fn warn_publish_failure(
    consecutive_failures: &mut u64,
    phase: &str,
    error: &impl std::fmt::Display,
) {
    *consecutive_failures = consecutive_failures.saturating_add(1);
    eprintln!(
        "ployzd runtime projection warning: phase={phase} consecutive_failures={} error={error}",
        *consecutive_failures
    );
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum RuntimeProjectionStartError {
    #[error("failed to subscribe to intent changes: {message}")]
    SubscribeIntent { message: String },
    #[error("failed to load initial intent: {0}")]
    LoadInitialIntent(IntentReadError),
}
