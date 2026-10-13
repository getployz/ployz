//! EnsureContainer planning and the Machine-side Container effects behind it.

use std::sync::Arc;

use ployz_core::{MachineId, Namespace, ResolvedServiceSpec, ServiceId, ServiceMode};
use serde_json::json;

use crate::machine::{LocalMachine, LocalMachineError, LocalMachineStore, RecordOwner};
use crate::storage::test_support::FakePlugin;

#[tokio::test]
async fn fresh_service_revocation_preserves_management_and_trusted_ingress() {
    use crate::docker::test_support::{FakeDocker, fake_runtime_with};
    use ployz_core::{AdvertisedEndpoint, ContainerKind, MachineName};
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-role-admission-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            },
            wireguard_mtu: None,
        })
        .unwrap();
    let owner = RecordOwner::spawn(store).unwrap();
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        create_barrier: Some(Arc::new(tokio::sync::Barrier::new(1))),
        ..Default::default()
    })
    .await;
    let local = LocalMachine::new(owner.clone()).with_containers(Some(runtime.clone()));
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
        "container": {"image":"example.test/api", "pull_policy":"missing"}
    })).unwrap();
    let namespace = Namespace::parse("app").unwrap();
    let existing = local
        .create_container(
            ContainerKind::ServiceContainer,
            &namespace,
            &spec,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    let update = serde_json::from_value(json!({
        "accepts_services": false, "accepts_ingress": true, "accepts_builds": true
    }))
    .unwrap();
    owner
        .mutate(move |store| store.update(update, &[]))
        .await
        .unwrap()
        .unwrap();
    for kind in [
        ContainerKind::ServiceContainer,
        ContainerKind::PreDeployHook,
    ] {
        let error = local
            .create_container(kind, &namespace, &spec, None, None, None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("accept"), "{error}");
    }
    let error = local
        .prepare_volumes(vec![ployz_core::ServiceStorageSpec::from(&spec)])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("accept"), "{error}");
    let labels = fake
        .request_bodies
        .lock()
        .unwrap()
        .iter()
        .find(|(path, _)| path.ends_with("/containers/create"))
        .unwrap()
        .1
        .get("Labels")
        .unwrap()
        .clone();
    fake.existing_container.lock().unwrap().as_mut().unwrap()["Config"] = json!({"Labels": labels});
    runtime.start(&existing.container_id).await.unwrap();
    runtime
        .stop(&existing.container_id, None, None)
        .await
        .unwrap();
    runtime
        .remove(&existing.container_id, false, false)
        .await
        .unwrap();
    assert!(fake.existing_container.lock().unwrap().is_none());
    let ingress = ployz_core::caddy_service_spec("caddy:test".into(), Default::default())
        .to_resolved(ServiceId::random(), Default::default())
        .unwrap();
    let error = local
        .create_container(
            ContainerKind::ServiceContainer,
            &namespace,
            &ingress,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("accept"), "{error}");
    local
        .create_container(
            ContainerKind::ServiceContainer,
            &Namespace::system(),
            &ingress,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[tokio::test]
async fn cancelled_create_finishes_before_reset_cleanup() {
    for before_create in [false, true] {
        use crate::docker::test_support::{FakeDocker, fake_runtime_with};
        use ployz_core::{AdvertisedEndpoint, ContainerKind, LocalMachinePhase, MachineName};
        let data_dir =
            std::env::temp_dir().join(format!("ployzd-admission-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
            })
            .unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            create_barrier: (!before_create).then(|| barrier.clone()),
            image_barrier: before_create.then(|| barrier.clone()),
            ..Default::default()
        })
        .await;
        let owner = RecordOwner::spawn(store).unwrap();
        let plugin = FakePlugin::default();
        plugin.reply("Storage.Demote", json!({"Ok": []}));
        let (plugin, _plugin_server) = plugin.serve(&data_dir);
        let local = LocalMachine::new(owner.clone()).with_containers(Some(runtime.clone()));
        let resetting = LocalMachine::new(owner)
            .with_containers(Some(runtime))
            .with_plugin(plugin);
        let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Replicated { replicas: 1.try_into().unwrap() }).unwrap(),
        "container":{"image":"example.test/api", "pull_policy":"missing"}
    })).unwrap();
        let namespace = Namespace::parse("app").unwrap();
        let creating = tokio::spawn({
            let local = local.clone();
            let spec = spec.clone();
            let namespace = namespace.clone();
            async move {
                local
                    .create_container(
                        ContainerKind::ServiceContainer,
                        &namespace,
                        &spec,
                        None,
                        None,
                        None,
                    )
                    .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), barrier.wait())
            .await
            .unwrap();
        creating.abort();
        let _ = creating.await;
        let mut reset = Box::pin(resetting.leave(ployz_core::LeaveRequest::default()));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut reset)
                .await
                .is_err(),
            "reset must wait for the admitted create even after its caller is cancelled"
        );
        barrier.wait().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), reset)
            .await
            .unwrap()
            .unwrap();
        assert!(fake.existing_container.lock().unwrap().is_none());
        assert_eq!(local.record().phase(), LocalMachinePhase::Resetting);
        assert!(matches!(
            local
                .create_container(
                    ContainerKind::ServiceContainer,
                    &namespace,
                    &spec,
                    None,
                    None,
                    None,
                )
                .await,
            Err(LocalMachineError::NotParticipating)
        ));
        std::fs::remove_dir_all(data_dir).unwrap();
    }
}

#[tokio::test]
async fn joining_admits_direct_create() {
    use crate::docker::test_support::{FakeDocker, fake_runtime_with};
    use ployz_core::{AdvertisedEndpoint, ContainerKind, Machine, MachineName};
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-joining-admission-{}", MachineId::random()));
    let owner = RecordOwner::spawn(LocalMachineStore::open(&data_dir).unwrap()).unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let (runtime, _) = fake_runtime_with(FakeDocker {
        create_barrier: Some(barrier.clone()),
        ..Default::default()
    })
    .await;
    let local = LocalMachine::new(owner.clone()).with_containers(Some(runtime));
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": ServiceId::random(), "name": "api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
        "container": {"image":"example.test/api", "pull_policy":"missing"}
    })).unwrap();
    let namespace = Namespace::parse("app").unwrap();
    assert!(matches!(
        local
            .create_container(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                None,
                None,
                None,
            )
            .await,
        Err(LocalMachineError::NotParticipating)
    ));
    let record = owner.record();
    let machine = Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: record.id(),
        name: MachineName::parse("joining").unwrap(),
        subnet: "10.210.0.0/24".parse().unwrap(),
        public_key: record.private_key().public_key(),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
        runtime: Default::default(),
        build_concurrency: None,
    };
    let mut peer = machine.clone();
    peer.id = MachineId::random();
    owner
        .mutate(move |store| {
            store.enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Member {
                    registration: ployz_core::Registered {
                        assigned_machine: machine,
                        visible_peers: vec![peer],
                        target_versions: Default::default(),
                    },
                },
                wireguard_mtu: None,
            })
        })
        .await
        .unwrap()
        .unwrap();
    let release = tokio::spawn(async move {
        barrier.wait().await;
        barrier.wait().await;
    });
    local
        .create_container(
            ContainerKind::ServiceContainer,
            &namespace,
            &spec,
            None,
            None,
            None,
        )
        .await
        .unwrap();
    release.await.unwrap();
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[tokio::test]
async fn removal_warning_restarts_and_keeps_recovery_available_without_container_admission() {
    use crate::{
        corrosion::{AdminClient, fake_cluster},
        docker::test_support::{FakeDocker, fake_runtime_with},
    };
    use ployz_core::{
        AdvertisedEndpoint, ContainerKind, LeaveRequest, Left, LocalMachinePhase, MachineName,
    };
    let data_dir =
        std::env::temp_dir().join(format!("ployzd-removal-admission-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            },
            wireguard_mtu: None,
        })
        .unwrap();
    let (replicated, server) = fake_cluster::store().await;
    server.abort();
    let _ = server.await;
    let (runtime, _) = fake_runtime_with(FakeDocker::default()).await;
    let owner = RecordOwner::spawn(store).unwrap();
    let restarting = owner.restart_requested();
    let plugin = FakePlugin::default();
    plugin.reply("Storage.Demote", json!({"Ok": []}));
    let (plugin, _plugin_server) = plugin.serve(&data_dir);
    let local = LocalMachine::new(owner)
        .with_containers(Some(runtime))
        .with_cluster(Some((replicated, AdminClient::new("/no/such/admin.sock"))))
        .with_plugin(plugin);
    let left = local.leave(LeaveRequest {}).await.unwrap();
    assert!(
        matches!(
            left,
            Left::Left {
                reset_warning: Some(_),
                ..
            }
        ),
        "{left:?}"
    );
    assert!(*restarting.borrow());
    assert_eq!(local.record().phase(), LocalMachinePhase::Resetting);
    assert!(
        local.record().machine().is_some(),
        "historical Machine remains available for recovery"
    );
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": ServiceId::random(), "name":"api", "mode": serde_json::to_value(ServiceMode::Global).unwrap(),
        "container":{"image":"example.test/api", "pull_policy":"missing"}
    })).unwrap();
    let namespace = Namespace::parse("app").unwrap();
    assert!(matches!(
        local
            .create_container(
                ContainerKind::ServiceContainer,
                &namespace,
                &spec,
                None,
                None,
                None,
            )
            .await,
        Err(LocalMachineError::NotParticipating)
    ));
    std::fs::remove_dir_all(data_dir).unwrap();
}

mod plan {
    use std::collections::BTreeMap;

    use ployz_core::{
        AnonymousVolumes, ContainerAddress, ContainerId, ContainerKind, ContainerLocator,
        ContainerNow, ContainerObservation, ContainerObservationParts, ContainerRefusalReason,
        ContainerRuntimeObservation, ContainerWant, CreationKey, Drain, HealthObservation,
        LocalMachinePhase, MachineId, Namespace, NewContainer, ResolvedServiceSpec, RunTarget,
        SlotKey, StopOptions,
    };
    use serde_json::json;

    use super::super::{ContainerStep, plan_container};

    fn id(digit: char) -> ContainerId {
        ContainerId::parse(digit.to_string().repeat(64)).unwrap()
    }

    fn spec(image: &str) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "2".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": image, "pull_policy": "missing" }
        }))
        .unwrap()
    }

    fn slot() -> SlotKey {
        SlotKey {
            namespace: Namespace::parse("app").unwrap(),
            kind: ContainerKind::ServiceContainer,
            creation_key: CreationKey::try_from("k1".to_owned()).unwrap(),
        }
    }

    fn present(runtime: ContainerRuntimeObservation, image: &str) -> ContainerNow {
        present_as(ContainerKind::ServiceContainer, runtime, image)
    }

    fn present_as(
        kind: ContainerKind,
        runtime: ContainerRuntimeObservation,
        image: &str,
    ) -> ContainerNow {
        ContainerNow::Present(
            ContainerObservation::try_from(ContainerObservationParts {
                container_id: id('a'),
                display_name: "api-a".into(),
                created_at_unix_nanos: 0,
                machine_id: MachineId::parse("1".repeat(32)).unwrap(),
                namespace: Namespace::parse("app").unwrap(),
                kind,
                runtime,
                effective_healthcheck: None,
                resolved_spec: spec(image),
                address: Some(ContainerAddress([10, 210, 1, 2].into())),
                labels: BTreeMap::new(),
            })
            .unwrap(),
        )
    }

    fn running(image: &str) -> ContainerNow {
        present(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            image,
        )
    }

    fn exited(image: &str) -> ContainerNow {
        present(ContainerRuntimeObservation::Exited { code: 0 }, image)
    }

    fn hook_slot() -> SlotKey {
        SlotKey {
            kind: ContainerKind::PreDeployHook,
            ..slot()
        }
    }

    fn hook(runtime: ContainerRuntimeObservation) -> ContainerNow {
        present_as(ContainerKind::PreDeployHook, runtime, "img")
    }

    fn run_hook() -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::New(NewContainer {
                slot: hook_slot(),
                ..new_container("img")
            }),
        }
    }

    fn absent() -> ContainerNow {
        ContainerNow::Absent {
            of: ContainerLocator::Key(slot()),
        }
    }

    fn new_container(image: &str) -> NewContainer {
        NewContainer {
            slot: slot(),
            resolved_spec: spec(image),
            deployment_id: None,
            registry_auth: None,
            answer: ployz_core::AnswerWhen::Started,
        }
    }

    fn run_new(image: &str) -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::New(new_container(image)),
        }
    }

    fn run_existing() -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::Existing(ContainerLocator::Id(id('a'))),
        }
    }

    fn stopped() -> ContainerWant {
        ContainerWant::Stopped {
            container: ContainerLocator::Id(id('a')),
            drain: Drain::DEFAULT,
            stop: StopOptions::default(),
        }
    }

    fn gone() -> ContainerWant {
        ContainerWant::Gone {
            container: ContainerLocator::Key(slot()),
            drain: Drain::DEFAULT,
            anonymous_volumes: AnonymousVolumes::Remove,
        }
    }

    fn refused(step: &ContainerStep) -> Option<ContainerRefusalReason> {
        let ContainerStep::Refuse(refusal) = step else {
            return None;
        };
        Some(refusal.reason)
    }

    #[test]
    fn each_want_takes_the_one_step_from_what_is_observed() {
        let participating = &LocalMachinePhase::Participating;
        let table: Vec<(ContainerWant, ContainerNow, ContainerStep)> = vec![
            (
                run_new("img"),
                absent(),
                ContainerStep::CreateAndStart(new_container("img")),
            ),
            (run_new("img"), running("img"), ContainerStep::Done),
            (run_new("img"), exited("img"), ContainerStep::Start(id('a'))),
            (run_existing(), running("img"), ContainerStep::Done),
            (run_existing(), exited("img"), ContainerStep::Start(id('a'))),
            (
                stopped(),
                running("img"),
                ContainerStep::Stop {
                    id: id('a'),
                    drain: Drain::DEFAULT,
                    stop: StopOptions::default(),
                },
            ),
            (stopped(), exited("img"), ContainerStep::Hold(id('a'))),
            (gone(), absent(), ContainerStep::Done),
            (
                ContainerWant::Gone {
                    container: ContainerLocator::Id(id('a')),
                    drain: Drain::DEFAULT,
                    anonymous_volumes: AnonymousVolumes::Keep,
                },
                ContainerNow::Absent {
                    of: ContainerLocator::Id(id('a')),
                },
                ContainerStep::Remove(id('a'), AnonymousVolumes::Keep),
            ),
            (
                gone(),
                running("img"),
                ContainerStep::StopThenRemove {
                    id: id('a'),
                    drain: Drain::DEFAULT,
                    anonymous_volumes: AnonymousVolumes::Remove,
                },
            ),
            (
                gone(),
                exited("img"),
                ContainerStep::Remove(id('a'), AnonymousVolumes::Remove),
            ),
            (
                run_hook(),
                hook(ContainerRuntimeObservation::Created),
                ContainerStep::CreateAndStart(NewContainer {
                    slot: hook_slot(),
                    ..new_container("img")
                }),
            ),
            (
                run_hook(),
                hook(ContainerRuntimeObservation::Exited { code: 0 }),
                ContainerStep::Done,
            ),
        ];
        for (want, now, step) in table {
            let shown = format!("{want:?} / {now:?}");
            assert_eq!(plan_container(want, &now, participating), step, "{shown}");
        }
    }

    #[test]
    fn a_slot_holding_another_spec_is_occupied_and_a_missing_target_is_not_found() {
        let participating = &LocalMachinePhase::Participating;
        assert_eq!(
            refused(&plan_container(
                run_new("img"),
                &running("other"),
                participating
            )),
            Some(ContainerRefusalReason::SlotOccupied)
        );
        for want in [run_existing(), stopped()] {
            let shown = format!("{want:?}");
            assert_eq!(
                refused(&plan_container(want, &absent(), participating)),
                Some(ContainerRefusalReason::NotFound),
                "{shown}"
            );
        }
    }

    #[test]
    fn only_a_running_want_needs_the_machine_to_participate() {
        for phase in [
            LocalMachinePhase::Uninitialized,
            LocalMachinePhase::Resetting,
            LocalMachinePhase::Unrecognized("later".into()),
        ] {
            for want in [run_new("img"), run_existing()] {
                let shown = format!("{want:?} while {phase:?}");
                assert_eq!(
                    refused(&plan_container(want, &running("img"), &phase)),
                    Some(ContainerRefusalReason::NotParticipating),
                    "{shown}"
                );
            }
            assert_eq!(
                plan_container(stopped(), &running("img"), &phase),
                ContainerStep::Stop {
                    id: id('a'),
                    drain: Drain::DEFAULT,
                    stop: StopOptions::default(),
                }
            );
            assert_eq!(
                plan_container(gone(), &exited("img"), &phase),
                ContainerStep::Remove(id('a'), AnonymousVolumes::Remove)
            );
        }
        assert_eq!(
            plan_container(run_new("img"), &absent(), &LocalMachinePhase::Joining),
            ContainerStep::CreateAndStart(new_container("img"))
        );
    }
}

#[tokio::test]
async fn a_racing_create_that_adopts_an_exited_hook_does_not_start_it() {
    use std::{collections::BTreeMap, sync::Mutex};

    use axum::http::Method;
    use ployz_core::{MachineName, NewContainer};

    use crate::docker::test_support::{FakeDocker, fake_runtime_with, spec_with_sources};
    let data_dir = std::env::temp_dir().join(format!("ployzd-hook-adopt-{}", MachineId::random()));
    let mut store = LocalMachineStore::open(&data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                    "192.0.2.1:51820".parse().unwrap(),
                )],
            },
            wireguard_mtu: None,
        })
        .unwrap();
    let owner = RecordOwner::spawn(store).unwrap();
    let (runtime, fake) = fake_runtime_with(FakeDocker {
        named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
        ..Default::default()
    })
    .await;
    let local = LocalMachine::new(owner)
        .with_containers(Some(runtime))
        .with_zpool("true");
    let new: NewContainer = serde_json::from_value(json!({
        "slot": {"namespace":"app", "kind":"pre_deploy_hook", "creation_key":"h1"},
        "resolved_spec": spec_with_sources(vec![]),
    }))
    .unwrap();
    let starts = || {
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, path)| *method == Method::POST && path.ends_with("/start"))
            .count()
    };

    assert_eq!(local.create_and_start(new.clone()).await.unwrap(), None);
    assert_eq!(starts(), 1);
    // The first request's hook ran and exited; a second request that saw the slot empty
    // reaches the create and adopts it.
    for container in fake
        .named_containers
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .values_mut()
    {
        container["State"] = json!({"Status":"exited", "ExitCode":0});
    }
    assert_eq!(local.create_and_start(new).await.unwrap(), None);
    assert_eq!(starts(), 1, "the adopted hook is not run again");
    std::fs::remove_dir_all(data_dir).unwrap();
}

mod settled {
    use std::{collections::BTreeMap, sync::Mutex, time::Duration};

    use ployz_core::{
        ConfiguredHealthcheck, ContainerNow, ContainerRuntimeObservation, ContainerWant,
        EnsuredContainer, HealthObservation, HealthVerdict, HealthcheckCommand, HealthcheckSpec,
        MachineName, NewContainer, RunTarget,
    };
    use serde_json::{Value, json};

    use super::*;
    use crate::docker::test_support::{FakeDocker, fake_runtime_with, spec_with_sources};

    struct Settling {
        local: LocalMachine,
        fake: FakeDocker,
        data_dir: std::path::PathBuf,
    }

    async fn settling() -> Settling {
        let data_dir = std::env::temp_dir().join(format!("ployzd-settled-{}", MachineId::random()));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("local").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
            })
            .unwrap();
        let owner = RecordOwner::spawn(store).unwrap();
        let (runtime, fake) = fake_runtime_with(FakeDocker {
            named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
            ..Default::default()
        })
        .await;
        let local = LocalMachine::new(owner)
            .with_containers(Some(runtime))
            .with_zpool("true");
        Settling {
            local,
            fake,
            data_dir,
        }
    }

    fn new_container(answer: &str) -> NewContainer {
        serde_json::from_value(json!({
            "slot": {"namespace":"app", "kind":"service_container", "creation_key":"s1"},
            "resolved_spec": spec_with_sources(vec![]),
            "answer": answer,
        }))
        .unwrap()
    }

    fn run(new: NewContainer) -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::New(new),
        }
    }

    fn settled_run(deadline_millis: u64) -> ContainerWant {
        let mut new = new_container("settled");
        new.resolved_spec.update.monitor_millis = Some(0);
        new.resolved_spec.container.healthcheck =
            Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
                test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
                interval_millis: Some(1_000),
                timeout_millis: Some(1_000),
                start_period_millis: None,
                start_interval_millis: None,
                retries: Some(1),
                deadline_millis: Some(deadline_millis),
            }));
        run(new)
    }

    async fn report_once_started(fake: &FakeDocker, state: Value) {
        let containers = fake.named_containers.as_ref().unwrap();
        loop {
            {
                let mut containers = containers.lock().unwrap();
                if let Some(current) = containers
                    .values_mut()
                    .filter_map(|container| container.get_mut("State"))
                    .find(|current| current.get("Status") == Some(&Value::from("running")))
                {
                    *current = state;
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn ensure_reporting(want: ContainerWant, state: Value) -> (EnsuredContainer, Duration) {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let started = std::time::Instant::now();
        let ensure = tokio::spawn({
            let local = local.clone();
            async move { local.ensure_container(want).await }
        });
        report_once_started(&fake, state).await;
        let reply = tokio::time::timeout(Duration::from_secs(10), ensure)
            .await
            .expect("a Settled ensure answers by its deadline")
            .unwrap()
            .unwrap();
        let waited = started.elapsed();
        std::fs::remove_dir_all(data_dir).unwrap();
        (reply, waited)
    }

    #[tokio::test]
    async fn ensure_settled_answers_healthy_once_the_check_passes() {
        let (reply, _) = ensure_reporting(
            settled_run(60_000),
            json!({"Status":"running", "Health":{"Status":"healthy"}}),
        )
        .await;

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: Some(HealthVerdict::Healthy),
                }
            ),
            "expected a Healthy verdict, got {reply:?}"
        );
    }

    #[tokio::test]
    async fn ensure_started_answers_without_waiting_for_health() {
        let Settling {
            local, data_dir, ..
        } = settling().await;
        let mut new = new_container("started");
        new.resolved_spec.update.monitor_millis = Some(60_000);

        let reply = tokio::time::timeout(Duration::from_secs(5), local.ensure_container(run(new)))
            .await
            .expect("a Started ensure answers without waiting out the monitor")
            .unwrap();

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: None,
                }
            ),
            "expected no verdict, got {reply:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn ensure_settled_without_a_healthcheck_waits_the_default_monitor() {
        let (reply, waited) =
            ensure_reporting(run(new_container("settled")), json!({"Status":"running"})).await;

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: Some(HealthVerdict::Healthy),
                }
            ),
            "expected a Healthy verdict, got {reply:?}"
        );
        assert!(
            waited >= Duration::from_secs(5) && waited < Duration::from_secs(6),
            "waited {waited:?}"
        );
    }

    #[tokio::test]
    async fn removal_by_slot_waits_for_the_create_that_fills_it() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let held = tokio::spawn({
            let local = local.clone();
            let (started, release) = (Arc::clone(&started), Arc::clone(&release));
            async move { local.hold_mutation_for_test(started, release).await }
        });
        started.notified().await;
        let create = tokio::spawn({
            let local = local.clone();
            async move { local.ensure_container(run(new_container("started"))).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let new = new_container("started");
        let gone = tokio::spawn({
            let local = local.clone();
            async move {
                local
                    .ensure_container(ContainerWant::Gone {
                        container: ployz_core::ContainerLocator::Key(new.slot),
                        drain: ployz_core::Drain::NONE,
                        anonymous_volumes: ployz_core::AnonymousVolumes::Remove,
                    })
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !gone.is_finished(),
            "the removal answered before the create ran"
        );

        release.notify_one();
        held.await.unwrap().unwrap();
        create.await.unwrap().unwrap();
        let reply = gone.await.unwrap().unwrap();

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Absent { .. },
                    ..
                }
            ),
            "expected the slot empty, got {reply:?}"
        );
        assert!(
            fake.named_containers
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn removal_by_slot_finishes_after_its_caller_stops_waiting() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let held = tokio::spawn({
            let local = local.clone();
            let (started, release) = (Arc::clone(&started), Arc::clone(&release));
            async move { local.hold_mutation_for_test(started, release).await }
        });
        started.notified().await;
        let create = tokio::spawn({
            let local = local.clone();
            async move { local.ensure_container(run(new_container("started"))).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let new = new_container("started");
        let gone = tokio::spawn({
            let local = local.clone();
            async move {
                local
                    .ensure_container(ContainerWant::Gone {
                        container: ployz_core::ContainerLocator::Key(new.slot),
                        drain: ployz_core::Drain::NONE,
                        anonymous_volumes: ployz_core::AnonymousVolumes::Remove,
                    })
                    .await
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        gone.abort();
        assert!(gone.await.unwrap_err().is_cancelled());

        release.notify_one();
        held.await.unwrap().unwrap();
        create.await.unwrap().unwrap();
        let named = fake.named_containers.as_ref().unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !named.lock().unwrap().is_empty() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the abandoned removal still empties the slot");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn ensure_settled_answers_failed_when_health_fails() {
        let (reply, _) = ensure_reporting(
            settled_run(60_000),
            json!({"Status":"running", "Health":{"Status":"unhealthy"}}),
        )
        .await;

        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(_),
            health: Some(HealthVerdict::Failed { observation, .. }),
        } = reply
        else {
            panic!("expected a Failed verdict, got {reply:?}");
        };
        assert_eq!(
            observation,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            }
        );
    }

    #[tokio::test]
    async fn ensure_settled_answers_timed_out_at_deadline() {
        let (reply, waited) = ensure_reporting(
            settled_run(1_500),
            json!({"Status":"running", "Health":{"Status":"starting"}}),
        )
        .await;

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: Some(HealthVerdict::TimedOut { .. }),
                }
            ),
            "expected a TimedOut verdict, got {reply:?}"
        );
        assert!(
            waited >= Duration::from_millis(1_500) && waited < Duration::from_millis(2_500),
            "waited {waited:?}"
        );
    }
}
