mod cleanup;
mod commit;
mod export;
mod planning;
mod sessions;

pub use commit::apply;
pub use export::export_manifest;
pub use planning::preview;

#[cfg(test)]
pub(crate) use planning::{deployable_machines, desired_slots};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        DeployId, DeployState, DrainState, InstanceId, InstancePhase, MachineId, MachineRecord,
        MachineStatus, OverlayIp, Participation, PublicKey, ServiceReleaseSlot,
    };
    use async_trait::async_trait;
    use ployz_runtime_api::Result as RuntimeResult;
    use ployz_runtime_api::{
        DeploySession, DeploySessionFactory, PreDeployHookRequest, StartCandidateRequest,
    };
    use ployz_store_api::{DeployReadStore, MachineStore};
    use ployz_test_support::MemoryStore;
    use ployz_types::spec::{
        ContainerSpec, DeployManifest, Namespace, NetworkMode, Placement, PullPolicy, Resources,
        RestartPolicy, RolloutStrategy, ServiceSpec,
    };
    use ployz_types::time::now_unix_secs;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;
    use std::sync::{Arc, Mutex};

    #[test]
    fn deployable_machines_uses_all_store_candidates() {
        let machines = vec![
            test_machine(
                "fresh-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                90,
            ),
            test_machine(
                "stale-enabled",
                Participation::Enabled,
                MachineStatus::Up,
                69,
            ),
            test_machine(
                "down-enabled",
                Participation::Enabled,
                MachineStatus::Down,
                100,
            ),
            test_machine(
                "draining-fresh",
                Participation::Draining,
                MachineStatus::Up,
                100,
            ),
        ];

        let deployable = deployable_machines(&machines, &MachineId("local".into()));
        assert_eq!(
            deployable,
            vec![
                MachineId("down-enabled".into()),
                MachineId("draining-fresh".into()),
                MachineId("fresh-enabled".into()),
                MachineId("stale-enabled".into()),
            ]
        );
    }

    #[test]
    fn deployable_machines_falls_back_to_local_when_store_is_empty() {
        let machines = vec![];
        let deployable = deployable_machines(&machines, &MachineId("local".into()));
        assert_eq!(deployable, vec![MachineId("local".into())]);
    }

    #[test]
    fn replicated_one_reuses_existing_slot_machine() {
        let spec = ServiceSpec {
            name: "api".into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: "nginx:latest".into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                volumes: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            stop_grace_period: None,
            restart: RestartPolicy::UnlessStopped,
            pre_deploy: None,
        };
        let machines = vec![MachineId("machine-a".into()), MachineId("machine-b".into())];
        let current_slots = [ServiceReleaseSlot {
            slot_id: crate::model::SlotId("slot-0001".into()),
            machine_id: MachineId("machine-b".into()),
            active_instance_id: InstanceId("inst-1".into()),
            revision_hash: "rev-1".into(),
        }];

        let desired = desired_slots(&spec, &machines, Some(&current_slots)).expect("desired slots");
        assert_eq!(desired.len(), 1);
        assert_eq!(desired[0].slot_id, crate::model::SlotId("slot-0001".into()));
        assert_eq!(desired[0].machine_id, MachineId("machine-b".into()));
    }

    #[tokio::test]
    async fn preview_returns_error_when_manifest_contains_duplicate_services() {
        let store = Arc::new(MemoryStore::new());
        let local_machine = live_machine("local");
        store
            .upsert_self_machine(&local_machine)
            .await
            .expect("seed local machine");
        let manifest = DeployManifest {
            namespace: Namespace("prod".into()),
            services: vec![
                test_service_spec("api", "nginx:1.27"),
                test_service_spec("api", "nginx:1.28"),
            ],
        };

        let error = preview(store.as_ref(), store.as_ref(), &local_machine.id, &manifest)
            .await
            .expect_err("preview should reject duplicate service names");

        assert!(error.to_string().contains("duplicate service 'api'"));
    }

    #[tokio::test]
    async fn apply_persists_commit_and_deploy_record() {
        let store = Arc::new(MemoryStore::new());
        let local_machine = live_machine("local");
        store
            .upsert_self_machine(&local_machine)
            .await
            .expect("seed local machine");
        let manifest = test_manifest("nginx:1.27");
        let session_factory = TestDeploySessionFactory::default();

        let result = apply(
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            &session_factory,
            &local_machine.id,
            &manifest,
        )
        .await
        .expect("apply should succeed");

        assert_eq!(result.state, DeployState::Committed);

        let stored_deploy = store
            .get_deploy(&result.deploy_id)
            .await
            .expect("load deploy record")
            .expect("deploy record should exist");
        assert_eq!(stored_deploy.state, DeployState::Committed);

        let releases = store
            .list_service_releases(&manifest.namespace)
            .await
            .expect("load releases");
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].service, "api");
    }

    #[tokio::test]
    async fn apply_returns_error_when_participants_change_after_locking() {
        let store = Arc::new(MemoryStore::new());
        let local_machine = live_machine("local");
        store
            .upsert_self_machine(&local_machine)
            .await
            .expect("seed local machine");
        let manifest = test_manifest("nginx:1.27");
        let session_factory = TestDeploySessionFactory::with_participant_drift(
            Arc::clone(&store),
            live_machine("peer-2"),
        );

        let error = apply(
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            store.as_ref(),
            &session_factory,
            &local_machine.id,
            &manifest,
        )
        .await
        .expect_err("apply should fail after participant drift");

        assert!(
            error
                .to_string()
                .contains("participant set changed after lock acquisition")
        );
    }

    fn test_machine(
        id: &str,
        participation: Participation,
        status: MachineStatus,
        last_heartbeat: u64,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([7; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status,
            participation,
            last_heartbeat,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn live_machine(id: &str) -> MachineRecord {
        test_machine(
            id,
            Participation::Enabled,
            MachineStatus::Up,
            now_unix_secs(),
        )
    }

    fn test_manifest(image: &str) -> DeployManifest {
        DeployManifest {
            namespace: Namespace("prod".into()),
            services: vec![test_service_spec("api", image)],
        }
    }

    fn test_service_spec(name: &str, image: &str) -> ServiceSpec {
        ServiceSpec {
            name: name.into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: image.into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                volumes: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: Vec::new(),
            publish: Vec::new(),
            routes: Vec::new(),
            readiness: None,
            rollout: RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            stop_grace_period: None,
            restart: RestartPolicy::UnlessStopped,
            pre_deploy: None,
        }
    }

    #[derive(Default)]
    struct TestDeploySessionFactory {
        participant_drift: Option<(Arc<MemoryStore>, MachineRecord)>,
        drift_injected: Mutex<bool>,
    }

    impl TestDeploySessionFactory {
        fn with_participant_drift(store: Arc<MemoryStore>, machine: MachineRecord) -> Self {
            Self {
                participant_drift: Some((store, machine)),
                drift_injected: Mutex::new(false),
            }
        }
    }

    #[async_trait]
    impl DeploySessionFactory for TestDeploySessionFactory {
        async fn open(
            &self,
            machine: &MachineRecord,
            _namespace: &Namespace,
            _deploy_id: &DeployId,
            _coordinator_id: &MachineId,
        ) -> RuntimeResult<(
            Box<dyn DeploySession>,
            Vec<crate::model::InstanceStatusRecord>,
        )> {
            let should_inject_drift = if self.participant_drift.is_some() {
                let mut drift_injected = self
                    .drift_injected
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if *drift_injected {
                    false
                } else {
                    *drift_injected = true;
                    true
                }
            } else {
                false
            };

            if should_inject_drift && let Some((store, drift_machine)) = &self.participant_drift {
                store
                    .upsert_self_machine(drift_machine)
                    .await
                    .expect("inject participant drift");
            }

            Ok((
                Box::new(TestDeploySession {
                    machine_id: machine.id.clone(),
                }),
                Vec::new(),
            ))
        }
    }

    struct TestDeploySession {
        machine_id: MachineId,
    }

    #[async_trait]
    impl DeploySession for TestDeploySession {
        fn machine_id(&self) -> &MachineId {
            &self.machine_id
        }

        async fn inspect_namespace(
            &mut self,
        ) -> RuntimeResult<Vec<crate::model::InstanceStatusRecord>> {
            Ok(Vec::new())
        }

        async fn start_candidate(
            &mut self,
            request: StartCandidateRequest,
        ) -> RuntimeResult<crate::model::InstanceStatusRecord> {
            let spec: ServiceSpec =
                serde_json::from_str(&request.spec_json).expect("valid spec json in test");
            let revision_hash = spec.revision_hash().expect("revision hash");

            Ok(crate::model::InstanceStatusRecord {
                instance_id: request.instance_id,
                namespace: Namespace("prod".into()),
                service: request.service,
                slot_id: request.slot_id,
                machine_id: self.machine_id.clone(),
                revision_hash,
                deploy_id: DeployId("deploy-under-test".into()),
                docker_container_id: "container-under-test".into(),
                overlay_ip: None,
                backend_ports: BTreeMap::new(),
                phase: InstancePhase::Ready,
                ready: true,
                drain_state: DrainState::None,
                error: None,
                started_at: now_unix_secs(),
                updated_at: now_unix_secs(),
            })
        }

        async fn drain_instance(&mut self, _instance_id: &InstanceId) -> RuntimeResult<()> {
            Ok(())
        }

        async fn run_pre_deploy_hook(
            &mut self,
            _request: PreDeployHookRequest,
        ) -> RuntimeResult<()> {
            Ok(())
        }

        async fn remove_instance(&mut self, _instance_id: &InstanceId) -> RuntimeResult<()> {
            Ok(())
        }

        async fn close(self: Box<Self>) -> RuntimeResult<()> {
            Ok(())
        }
    }
}
