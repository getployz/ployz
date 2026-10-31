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
            pinned: Default::default(),
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
                pinned: Default::default(),
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
        plugin.reply(
            "Storage.Demote",
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
        );
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
                        sealing_key: None,
                        devices: Vec::new(),
                    },
                },
                wireguard_mtu: None,
                pinned: Default::default(),
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
            pinned: Default::default(),
        })
        .unwrap();
    let (replicated, server) = fake_cluster::store().await;
    server.abort();
    let _ = server.await;
    let (runtime, _) = fake_runtime_with(FakeDocker::default()).await;
    let owner = RecordOwner::spawn(store).unwrap();
    let restarting = owner.restart_requested();
    let plugin = FakePlugin::default();
    plugin.reply(
        "Storage.Demote",
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
    );
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
        LocalMachinePhase, MachineId, Namespace, NewContainer, ResolvedServiceSpec, RunRotation,
        RunTarget, ServiceName, SlotKey, StopOptions,
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
            service: ServiceName::parse("api").unwrap(),
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
                rotation: None,
                r#gen: None,
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
                stopped: false,
                run: None,
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
                r#gen: ployz_core::Gen::FIRST,
                slot: hook_slot(),
                ..new_container("img")
            }),
            rotation: RunRotation::Pending,
        }
    }

    fn absent() -> ContainerNow {
        ContainerNow::Absent {
            of: ContainerLocator::Key(slot()),
        }
    }

    fn new_container(image: &str) -> NewContainer {
        NewContainer {
            r#gen: ployz_core::Gen::FIRST,
            slot: slot(),
            resolved_spec: spec(image),
            deployment_id: None,
            registry_auth: None,
            image_prepared: false,
            answer: ployz_core::AnswerWhen::Started,
        }
    }

    fn run_new(image: &str) -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::New(new_container(image)),
            rotation: RunRotation::Pending,
        }
    }

    fn run_existing() -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::Existing(ContainerLocator::Id(id('a'))),
            rotation: RunRotation::Pending,
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
                    r#gen: ployz_core::Gen::FIRST,
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
            pinned: Default::default(),
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
        "slot": {"namespace":"app", "service":"api", "kind":"pre_deploy_hook", "creation_key":"h1"},
        "resolved_spec": spec_with_sources(vec![]),
        "gen": 1,
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

    assert_eq!(
        local.create_and_start(new.clone(), None).await.unwrap(),
        None
    );
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
    assert_eq!(local.create_and_start(new, None).await.unwrap(), None);
    assert_eq!(starts(), 1, "the adopted hook is not run again");
    std::fs::remove_dir_all(data_dir).unwrap();
}

mod settled {
    use std::{
        collections::BTreeMap,
        sync::{Mutex, atomic::Ordering},
        time::Duration,
    };

    use ployz_core::{
        ConfiguredHealthcheck, ContainerNow, ContainerRuntimeObservation, ContainerWant,
        EnsuredContainer, HealthObservation, HealthVerdict, HealthcheckCommand, HealthcheckSpec,
        MachineName, NewContainer, RunRotation, RunTarget,
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
                pinned: Default::default(),
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
            "slot": {"namespace":"app", "service":"api", "kind":"service_container", "creation_key":"s1"},
            "resolved_spec": spec_with_sources(vec![]),
            "gen": 1,
            "answer": answer,
        }))
        .unwrap()
    }

    fn run(new: NewContainer) -> ContainerWant {
        ContainerWant::Running {
            container: RunTarget::New(new),
            rotation: RunRotation::Pending,
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

    async fn eventually(mut ready: impl FnMut() -> bool) {
        while !ready() {
            // real-wait: the daemon changes this state over the fake Docker's real TCP
            // socket, or in work detached from the test, and neither signals it.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn let_requests_land() {
        // real-wait: requests travel the fake Docker's TCP socket and admission's queue,
        // which emit no event.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    async fn report_once_started(fake: &FakeDocker, state: Value) {
        let containers = fake.named_containers.as_ref().unwrap();
        let mut state = Some(state);
        eventually(|| {
            let mut containers = containers.lock().unwrap();
            let Some(current) = containers
                .values_mut()
                .filter_map(|container| container.get_mut("State"))
                .find(|current| current.get("Status") == Some(&Value::from("running")))
            else {
                return false;
            };
            *current = state.take().expect("reported once");
            true
        })
        .await;
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
            waited >= Duration::from_secs(1) && waited < Duration::from_secs(3),
            "waited {waited:?}"
        );
    }

    #[tokio::test]
    async fn ensure_settled_without_a_healthcheck_outlasts_a_slow_first_inspect() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        *fake.slow_running_inspects.lock().unwrap() = Duration::from_secs(2);
        let mut new = new_container("settled");
        new.resolved_spec.container.healthcheck = Some(HealthcheckSpec::Disabled);

        let reply = tokio::time::timeout(Duration::from_secs(20), local.ensure_container(run(new)))
            .await
            .expect("a Settled ensure answers")
            .unwrap();

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: Some(HealthVerdict::Healthy),
                }
            ),
            "an inspect slower than the one-second deadline still answers, got {reply:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
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
        let_requests_land().await;
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
        let_requests_land().await;
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
        let_requests_land().await;
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
        // One turn lets the removal detach from this caller.
        tokio::task::yield_now().await;
        gone.abort();
        assert!(gone.await.unwrap_err().is_cancelled());

        release.notify_one();
        held.await.unwrap().unwrap();
        create.await.unwrap().unwrap();
        let named = fake.named_containers.as_ref().unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            eventually(|| named.lock().unwrap().is_empty()),
        )
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

    /// The settle's polls see a starting Container until `after` past the settle's
    /// start, then `state`. Docker stalls every inspect after the first to show an OOM
    /// kill, so a verdict that answers read the kill from the poll that decided it.
    /// Answers the verdict and how long after the settle began it came.
    async fn ensure_stalling_after_oom(
        want: ContainerWant,
        after: Duration,
        state: Value,
    ) -> (HealthVerdict, Duration) {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        fake.stall_after_oom.store(true, Ordering::SeqCst);
        let ensure = tokio::spawn({
            let local = local.clone();
            async move { local.ensure_container(want).await }
        });
        let starting = json!({"Status":"running", "Health":{"Status":"starting"}});
        report_once_started(&fake, starting).await;
        let id = fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values()
            .find_map(|container| container.get("Id")?.as_str().map(str::to_owned))
            .unwrap();
        let id = ployz_core::ContainerId::parse(id).unwrap();
        eventually(|| local.settles_for_test(&id) > 0).await;
        let settling_since = std::time::Instant::now();
        // real-wait: lands the state between the settle's polls, which run on the real
        // clock against the fake Docker's TCP socket.
        tokio::time::sleep(after).await;
        report_once_started(&fake, state).await;
        let reply = tokio::time::timeout(Duration::from_secs(10), ensure)
            .await
            .expect("a Settled ensure answers by its deadline")
            .unwrap()
            .unwrap();
        let waited = settling_since.elapsed();
        std::fs::remove_dir_all(data_dir).unwrap();
        let EnsuredContainer::Ensured {
            health: Some(health),
            ..
        } = reply
        else {
            panic!("expected a verdict, got {reply:?}");
        };
        (health, waited)
    }

    #[tokio::test]
    async fn ensure_settled_says_docker_killed_the_exited_container_out_of_memory() {
        for (state, oom_killed) in [
            (
                json!({"Status":"exited", "ExitCode":137, "OOMKilled":true}),
                true,
            ),
            (
                json!({"Status":"exited", "ExitCode":137, "OOMKilled":false}),
                false,
            ),
            (
                json!({"Status":"running", "Health":{"Status":"unhealthy"}}),
                false,
            ),
        ] {
            let (health, waited) = ensure_stalling_after_oom(
                settled_run(60_000),
                Duration::from_millis(500),
                state.clone(),
            )
            .await;

            assert!(
                matches!(health, HealthVerdict::Failed { .. }),
                "{state}: {health:?}"
            );
            assert_eq!(health.oom_killed(), oom_killed, "{state}: {health:?}");
            assert!(
                waited >= Duration::from_millis(500) && waited < Duration::from_secs(2),
                "{state}: waited {waited:?}"
            );
        }
    }

    #[tokio::test]
    async fn ensure_settled_times_out_on_time_for_a_container_killed_out_of_memory() {
        // The kill shows well before the 6.5s deadline, more than the inspect grace
        // ahead of it, and every inspect after it stalls, so the stalled one ends at the
        // deadline.
        let (health, waited) = ensure_stalling_after_oom(
            settled_run(6_500),
            Duration::from_millis(500),
            json!({"Status":"running", "OOMKilled":true, "Health":{"Status":"starting"}}),
        )
        .await;

        assert!(
            matches!(health, HealthVerdict::TimedOut { .. }),
            "{health:?}"
        );
        assert!(health.oom_killed(), "{health:?}");
        assert!(
            waited >= Duration::from_millis(6_400) && waited < Duration::from_millis(7_500),
            "waited {waited:?}"
        );
    }
}

mod stopped_on_request {
    use std::{collections::BTreeMap, path::PathBuf, sync::Mutex, time::Duration};

    use ployz_core::{
        ContainerId, ContainerLocator, ContainerNow, ContainerObservation, ContainerRun,
        ContainerRuntimeObservation, ContainerWant, Drain, EnsuredContainer, MachineName,
        NewContainer, PortPublication, RunRotation, RunTarget, StopOptions,
    };
    use serde_json::json;

    use super::*;
    use crate::docker::{
        ContainerRuntime,
        test_support::{FakeDocker, fake_runtime_with, spec_with_sources},
    };

    struct Fixture {
        data_dir: PathBuf,
        local: LocalMachine,
        runtime: ContainerRuntime,
        fake: FakeDocker,
        id: ContainerId,
    }

    impl Fixture {
        /// One running Container serving port 8080 whose app exits 1 when stopped, as
        /// one that exits non-zero on SIGTERM does. Docker reports no pid until
        /// [`Self::sockets`] sets one, so a drain waits its full length.
        async fn start() -> Self {
            Self::publishing(vec![
                serde_json::from_value(json!({
                    "mode": "ingress",
                    "hostname": "api.example.test",
                    "load_balancer_port": 443,
                    "container_port": 8080,
                    "http_protocol": "http",
                }))
                .unwrap(),
            ])
            .await
        }

        async fn publishing(ports: Vec<PortPublication>) -> Self {
            let data_dir =
                std::env::temp_dir().join(format!("ployzd-stopped-mark-{}", MachineId::random()));
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
                    pinned: Default::default(),
                })
                .unwrap();
            let owner = RecordOwner::spawn(store).unwrap();
            let (runtime, fake) = fake_runtime_with(FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                stamp_starts: true,
                stop_exit_code: 1,
                ..Default::default()
            })
            .await;
            let local = LocalMachine::new(owner)
                .with_containers(Some(runtime.clone()))
                .with_zpool("true")
                .with_proc_root(data_dir.join("proc"));
            let mut spec = spec_with_sources(vec![]);
            spec.ports = ports;
            let new: NewContainer = serde_json::from_value(json!({
                "slot": {"namespace":"app", "service":"api", "kind":"service_container", "creation_key":"s1"},
                "resolved_spec": spec,
                "gen": 1,
            }))
            .unwrap();
            let id = ensure(
                &local,
                ContainerWant::Running {
                    container: RunTarget::New(new),
                    rotation: RunRotation::Pending,
                },
            )
            .await
            .container_id;
            Self {
                data_dir,
                local,
                runtime,
                fake,
                id,
            }
        }

        fn start_want(&self) -> ContainerWant {
            ContainerWant::Running {
                container: RunTarget::Existing(ContainerLocator::Id(self.id)),
                rotation: RunRotation::Pending,
            }
        }

        fn stop_want(&self) -> ContainerWant {
            ContainerWant::Stopped {
                container: ContainerLocator::Id(self.id),
                drain: Drain::NONE,
                stop: StopOptions::default(),
            }
        }

        /// Set each field, named by its JSON pointer, of the Container's Docker
        /// inspect behind the daemon's back.
        fn docker_sets(&self, fields: &[(&str, serde_json::Value)]) {
            for container in self
                .fake
                .named_containers
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .values_mut()
            {
                for (pointer, value) in fields {
                    let (parent, key) = pointer.rsplit_once('/').unwrap();
                    container
                        .pointer_mut(parent)
                        .and_then(serde_json::Value::as_object_mut)
                        .unwrap()
                        .insert(key.into(), value.clone());
                }
            }
        }

        /// Stop the Container with a drain, and let `meanwhile` change Docker once the
        /// drain has begun, well before the stop is issued.
        async fn stop_while_draining(
            &self,
            meanwhile: &[(&str, serde_json::Value)],
        ) -> ContainerObservation {
            let mut stopping = self.runtime.subscribe_stopping_for_test();
            let local = self.local.clone();
            let want = ContainerWant::Stopped {
                container: ContainerLocator::Id(self.id),
                drain: Drain::try_from(500).unwrap(),
                stop: StopOptions::default(),
            };
            let ensuring = tokio::spawn(async move { ensure(&local, want).await });
            tokio::time::timeout(
                Duration::from_secs(10),
                stopping.wait_for(|stopping| stopping.contains_key(&self.id)),
            )
            .await
            .expect("the drain began")
            .unwrap();
            self.docker_sets(meanwhile);
            ensuring.await.unwrap()
        }

        /// Give the Container a pid whose namespace lists one socket per `(port, state)`,
        /// a listener on 0.0.0.0 and any other on 10.1.0.5.
        fn sockets(&self, sockets: &[(u16, &str)]) {
            let net = self.data_dir.join("proc/4242/net");
            std::fs::create_dir_all(&net).unwrap();
            let header = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";
            let mut tcp = header.to_owned();
            for (row, (port, state)) in sockets.iter().enumerate() {
                let (local, remote) = if *state == "0A" {
                    ("00000000", "00000000:0000")
                } else {
                    ("0A010005", "0A010002:C350")
                };
                tcp.push_str(&format!("   {row}: {local}:{port:04X} {remote} {state} 00000000:00000000 00:00000000 00000000     0        0 1 1 0000000000000000 20 4 30 10 -1\n"));
            }
            std::fs::write(net.join("tcp"), tcp).unwrap();
            std::fs::write(net.join("tcp6"), header).unwrap();
            self.docker_sets(&[("/State/Pid", json!(4242))]);
        }

        async fn stop_timed(&self, drain: Drain) -> (ContainerObservation, Duration) {
            let began = std::time::Instant::now();
            let stopped = ensure(
                &self.local,
                ContainerWant::Stopped {
                    container: ContainerLocator::Id(self.id),
                    drain,
                    stop: StopOptions::default(),
                },
            )
            .await;
            (stopped, began.elapsed())
        }

        fn stop_rows(&self) -> i64 {
            rusqlite::Connection::open(self.runtime.spec_store_path_for_test())
                .unwrap()
                .query_row("SELECT COUNT(*) FROM stops", [], |row| row.get(0))
                .unwrap()
        }

        fn finish(self) {
            std::fs::remove_dir_all(self.data_dir).unwrap();
        }
    }

    async fn ensure(local: &LocalMachine, want: ContainerWant) -> ContainerObservation {
        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            ..
        } = Box::pin(local.ensure_container(want)).await.unwrap()
        else {
            panic!("the want was refused");
        };
        observed
    }

    #[tokio::test]
    async fn a_stopped_want_marks_only_the_run_it_stopped() {
        let fixture = Fixture::start().await;

        let stopped = ensure(&fixture.local, fixture.stop_want()).await;
        assert_eq!(
            stopped.runtime,
            ContainerRuntimeObservation::Exited { code: 1 }
        );
        assert!(stopped.stopped, "the Machine stopped this run");

        let started = ensure(&fixture.local, fixture.start_want()).await;
        assert!(
            matches!(started.runtime, ContainerRuntimeObservation::Running { .. }),
            "{:?}",
            started.runtime
        );
        assert!(!started.stopped, "a start begins a run nobody stopped");

        fixture.docker_sets(&[
            ("/State/Status", json!("exited")),
            ("/State/ExitCode", json!(1)),
        ]);
        let crashed = fixture
            .runtime
            .inspect_managed(&fixture.id, &fixture.local.record().id())
            .await
            .unwrap();
        assert_eq!(
            crashed.runtime,
            ContainerRuntimeObservation::Exited { code: 1 }
        );
        assert!(!crashed.stopped, "a run that exits on its own crashed");
        fixture.finish();
    }

    #[tokio::test]
    async fn a_run_that_exits_on_its_own_during_the_drain_stays_crashed() {
        let fixture = Fixture::start().await;

        let crashed = fixture
            .stop_while_draining(&[
                ("/State/Status", json!("exited")),
                ("/State/ExitCode", json!(1)),
                ("/State/FinishedAt", json!(chrono::Utc::now().to_rfc3339())),
            ])
            .await;

        assert_eq!(
            crashed.runtime,
            ContainerRuntimeObservation::Exited { code: 1 }
        );
        assert!(
            !crashed.stopped,
            "the run ended before the stop was issued, so the stop ended nothing"
        );
        assert_eq!(fixture.stop_rows(), 0);
        fixture.finish();
    }

    #[tokio::test]
    async fn a_run_restarted_during_the_drain_reads_stopped_once_the_stop_ends_it() {
        let fixture = Fixture::start().await;

        let stopped = fixture
            .stop_while_draining(&[
                ("/RestartCount", json!(1)),
                (
                    "/State/StartedAt",
                    json!((chrono::Utc::now() + chrono::Duration::seconds(1)).to_rfc3339()),
                ),
            ])
            .await;

        assert_eq!(
            stopped.runtime,
            ContainerRuntimeObservation::Exited { code: 1 }
        );
        assert!(
            stopped.stopped,
            "the stop ended the restarted run, the one Docker reports"
        );
        fixture.finish();
    }

    #[tokio::test]
    async fn a_stop_recorded_after_its_container_went_leaves_no_row() {
        let fixture = Fixture::start().await;
        assert!(
            ensure(&fixture.local, fixture.stop_want()).await.stopped,
            "the stop is recorded"
        );
        assert_eq!(fixture.stop_rows(), 1);

        fixture
            .runtime
            .remove(&fixture.id, true, false)
            .await
            .unwrap();
        assert_eq!(
            fixture.stop_rows(),
            0,
            "a removal takes the stop row with it"
        );

        // A stop that lost the race to that removal records only afterwards.
        fixture
            .runtime
            .record_stop(
                &fixture.id,
                &ContainerRun {
                    restart_count: 0,
                    started_at_unix_nanos: 1,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fixture.stop_rows(),
            0,
            "no row outlives its Container's spec"
        );
        fixture.finish();
    }

    mod stop_checked {
        use super::*;

        const ESTABLISHED: &str = "01";
        const LISTEN: &str = "0A";
        const TIME_WAIT: &str = "06";

        fn assert_exited(stopped: &ContainerObservation) {
            assert_eq!(
                stopped.runtime,
                ContainerRuntimeObservation::Exited { code: 1 }
            );
        }

        #[tokio::test]
        async fn stop_checked_ends_the_drain_once_the_copy_is_idle() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN)]);

            let (stopped, took) = fixture.stop_timed(Drain::MAX).await;

            assert_exited(&stopped);
            assert!(
                took < Duration::from_secs(10),
                "a copy listening with no connection stops long before the 30 s cap, took {took:?}"
            );
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_keeps_an_idle_copy_withdrawn_until_its_withdrawal_spreads() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN)]);

            let (stopped, took) = fixture.stop_timed(Drain::MAX).await;

            assert_exited(&stopped);
            assert!(
                took >= super::super::super::WITHDRAWAL_PROPAGATION,
                "Internal DNS elsewhere hears of the withdrawal before the stop, took {took:?}"
            );
            assert!(took < Duration::from_secs(10), "took {took:?}");
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_waits_for_a_connection_opened_while_the_withdrawal_spreads() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN)]);
            let mut stopping = fixture.runtime.subscribe_stopping_for_test();

            let (stopped, _) = {
                let stop = fixture.stop_timed(Drain::MAX);
                tokio::pin!(stop);
                tokio::select! {
                    _ = &mut stop => panic!("the stop ended before the withdrawal spread"),
                    withdrawn = stopping.wait_for(|stopping| stopping.contains_key(&fixture.id)) => {
                        withdrawn.unwrap();
                    }
                }
                // The drain has read the copy idle by now; the client connects after.
                assert!(
                    tokio::time::timeout(Duration::from_millis(300), &mut stop)
                        .await
                        .is_err(),
                    "the stop waits for the withdrawal to spread"
                );
                fixture.sockets(&[(8080, LISTEN), (8080, ESTABLISHED)]);
                assert!(
                    tokio::time::timeout(
                        super::super::super::WITHDRAWAL_PROPAGATION * 2,
                        &mut stop
                    )
                    .await
                    .is_err(),
                    "a client that connected before DNS heard keeps the copy draining"
                );
                fixture.sockets(&[(8080, LISTEN), (8080, TIME_WAIT)]);
                tokio::time::timeout(Duration::from_secs(10), stop)
                    .await
                    .expect("the drain ends once the connection closes")
            };

            assert_exited(&stopped);
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_keeps_an_idle_copy_withdrawn_no_longer_than_a_short_drain() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN)]);

            let (stopped, took) = fixture.stop_timed(Drain::try_from(250).unwrap()).await;

            assert_exited(&stopped);
            assert!(
                took >= Duration::from_millis(250)
                    && took < super::super::super::WITHDRAWAL_PROPAGATION,
                "the drain still caps the wait, took {took:?}"
            );
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_ends_the_drain_when_the_last_connection_closes() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN), (8080, ESTABLISHED)]);
            let mut stopping = fixture.runtime.subscribe_stopping_for_test();

            let (stopped, _) = {
                let stop = fixture.stop_timed(Drain::MAX);
                tokio::pin!(stop);
                tokio::select! {
                    _ = &mut stop => panic!("the stop ended while a connection was open"),
                    drained = stopping.wait_for(|stopping| stopping.contains_key(&fixture.id)) => {
                        drained.unwrap();
                    }
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(300), &mut stop)
                        .await
                        .is_err(),
                    "the drain waits while the connection is open"
                );
                fixture.sockets(&[(8080, LISTEN), (8080, TIME_WAIT)]);
                tokio::time::timeout(Duration::from_secs(10), stop)
                    .await
                    .expect("the drain ends once the connection closes")
            };

            assert_exited(&stopped);
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_waits_for_a_connection_whose_listener_closed_during_the_drain() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN), (8080, ESTABLISHED)]);
            let mut stopping = fixture.runtime.subscribe_stopping_for_test();

            let (stopped, took) = {
                let stop = fixture.stop_timed(Drain::try_from(500).unwrap());
                tokio::pin!(stop);
                tokio::select! {
                    _ = &mut stop => panic!("the stop ended while a connection was open"),
                    drained = stopping.wait_for(|stopping| stopping.contains_key(&fixture.id)) => {
                        drained.unwrap();
                    }
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), &mut stop)
                        .await
                        .is_err(),
                    "the drain reads the listener"
                );
                fixture.sockets(&[(8080, ESTABLISHED)]);
                stop.await
            };

            assert_exited(&stopped);
            assert!(
                took >= Duration::from_millis(500),
                "the connection still holds the copy, took {took:?}"
            );
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_waits_the_full_drain_on_a_busy_copy() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, LISTEN), (8080, ESTABLISHED)]);

            let (stopped, took) = fixture.stop_timed(Drain::try_from(500).unwrap()).await;

            assert_exited(&stopped);
            assert!(took >= Duration::from_millis(500), "took {took:?}");
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_waits_the_full_drain_when_the_sockets_cannot_be_read() {
            let fixture = Fixture::start().await;

            let (stopped, took) = fixture.stop_timed(Drain::try_from(500).unwrap()).await;

            assert_exited(&stopped);
            assert!(
                took >= Duration::from_millis(500),
                "no pid is no evidence of idleness, took {took:?}"
            );
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_waits_for_a_connection_on_a_port_the_service_never_declared() {
            let fixture = Fixture::publishing(vec![]).await;
            fixture.sockets(&[(9000, LISTEN), (9000, ESTABLISHED)]);

            let (stopped, took) = fixture.stop_timed(Drain::try_from(500).unwrap()).await;

            assert_exited(&stopped);
            assert!(
                took >= Duration::from_millis(500),
                "private traffic needs no published port, took {took:?}"
            );
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_stops_a_copy_without_sockets_at_once() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[]);

            let (stopped, took) = fixture.stop_timed(Drain::MAX).await;

            assert_exited(&stopped);
            assert!(
                took < Duration::from_secs(10),
                "a worker with no connection serves no client, took {took:?}"
            );
            fixture.finish();
        }

        #[tokio::test]
        async fn stop_checked_waits_for_a_connection_of_a_copy_without_a_listener() {
            let fixture = Fixture::start().await;
            fixture.sockets(&[(8080, ESTABLISHED)]);

            let (stopped, took) = fixture.stop_timed(Drain::try_from(500).unwrap()).await;

            assert_exited(&stopped);
            assert!(
                took >= Duration::from_millis(500),
                "a listener closed before the drain may leave a request it still answers, took {took:?}"
            );
            fixture.finish();
        }
    }
}

#[test]
fn a_sealed_value_the_spec_cannot_open_is_a_refusal_but_an_unreadable_key_is_an_error() {
    use ployz_core::ContainerRefusalReason;

    use crate::sealed_env::OpenError;

    let unopenable = ContainerRefusalReason::SealedValueUnopenable;
    for (open, reason) in [
        (
            OpenError::KeyMissing,
            Some(ContainerRefusalReason::SealingKeyMissing),
        ),
        (OpenError::MissingIdentity("TOKEN".into()), Some(unopenable)),
        (OpenError::SealedPort, Some(unopenable)),
        (OpenError::BadName("A=B".into()), Some(unopenable)),
        (OpenError::Unbound("TOKEN".into()), Some(unopenable)),
        (OpenError::Unopenable("TOKEN".into()), Some(unopenable)),
        (OpenError::ShortLine("TOKEN".into()), Some(unopenable)),
        (
            OpenError::KeyUnreadable(std::io::ErrorKind::PermissionDenied.into()),
            None,
        ),
    ] {
        let label = open.to_string();
        let refused = super::create_refusal(crate::docker::Error::Sealed(open).into())
            .ok()
            .map(|refusal| refusal.reason);
        assert_eq!(refused, reason, "{label}");
    }
}
