//! EnsureContainer planning and the Machine-side Container effects behind it.

use std::sync::Arc;

use ployz_core::{MachineId, Namespace, ResolvedServiceSpec, ServiceId, ServiceMode};
use serde_json::json;

use super::Runs;
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
                grant: None,
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
    runtime
        .lifecycle()
        .start(&existing.container_id)
        .await
        .unwrap();
    runtime
        .lifecycle()
        .stop(&existing.container_id, None, None)
        .await
        .unwrap();
    runtime
        .lifecycle()
        .remove(
            &existing.container_id,
            crate::docker::VolumeDisposal::Keep,
            false,
        )
        .await
        .unwrap();
    assert!(fake.existing_container.lock().unwrap().is_none());
    let ingress = crate::images::legacy_caddy_for_tests("caddy:test")
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
    let error = local
        .create_container(
            ContainerKind::ServiceContainer,
            &Namespace::system(),
            &ingress,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, super::Error::IngressProxyServiceSpec(_)),
        "{error}"
    );
    local
        .create_container(
            ContainerKind::ServiceContainer,
            &Namespace::system(),
            &crate::images::DoorSpecs::public(ServiceId::random()).holder,
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
                    grant: None,
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
    use ployz_core::ContainerKind;
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
    let join = crate::machine::test_support::ApprovingCluster::new(1).join(&owner.record());
    owner
        .mutate(move |store| store.enroll(join))
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
                grant: None,
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
        Admission, AnonymousVolumes, ContainerAddress, ContainerId, ContainerKind,
        ContainerLocator, ContainerNow, ContainerObservation, ContainerObservationParts,
        ContainerRefusalReason, ContainerRuntimeObservation, ContainerWant, CreationKey, Drain,
        HealthObservation, LocalMachinePhase, MachineId, Namespace, NewContainer,
        ResolvedServiceSpec, RunRotation, RunTarget, ServiceName, SlotKey, StopOptions,
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
                log_driver: None,
                admission: None,
                replaced_by: None,
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
            registry_login: None,
            image_prepared: false,
            answer: ployz_core::AnswerWhen::Started,
            image_id: None,
            replaces: None,
            free_receipt: None,
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

    /// A removal that names `b` as the copy that took this one's place.
    fn named() -> AnonymousVolumes {
        serde_json::from_value(json!({"anonymous_volumes": "remove", "replaced_by": id('b')}))
            .unwrap()
    }

    fn gone() -> ContainerWant {
        ContainerWant::Gone {
            container: ContainerLocator::Key(slot()),
            drain: Drain::DEFAULT,
            anonymous_volumes: named(),
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
                    anonymous_volumes: AnonymousVolumes::keep(),
                },
                ContainerNow::Absent {
                    of: ContainerLocator::Id(id('a')),
                },
                ContainerStep::Remove {
                    id: id('a'),
                    anonymous_volumes: AnonymousVolumes::keep(),
                    logs_to_read: false,
                },
            ),
            (
                gone(),
                running("img"),
                ContainerStep::StopThenRemove {
                    id: id('a'),
                    drain: Drain::DEFAULT,
                    anonymous_volumes: named(),
                    logs_to_read: true,
                },
            ),
            (
                gone(),
                exited("img"),
                ContainerStep::Remove {
                    id: id('a'),
                    anonymous_volumes: named(),
                    logs_to_read: true,
                },
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
            assert_eq!(
                plan_container(want, &now, participating, Admission::Free),
                step,
                "{shown}"
            );
        }
    }

    #[test]
    fn a_removal_waits_for_the_log_of_a_service_that_ran_and_of_no_hook_or_unstarted_container() {
        let participating = &LocalMachinePhase::Participating;
        let discarded = |drain| ContainerWant::Gone {
            container: ContainerLocator::Key(slot()),
            drain,
            anonymous_volumes: named(),
        };
        let logs_to_read = |want, now: ContainerNow| {
            let step = plan_container(want, &now, participating, Admission::Free);
            let (ContainerStep::StopThenRemove { logs_to_read, .. }
            | ContainerStep::Remove { logs_to_read, .. }) = step
            else {
                panic!("expected a removal, got {step:?}");
            };
            logs_to_read
        };
        let service = |runtime| present_as(ContainerKind::ServiceContainer, runtime, "img");
        let exited = ContainerRuntimeObservation::Exited { code: 1 };
        let running = ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        };

        assert!(
            logs_to_read(discarded(Drain::NONE), service(running.clone())),
            "a running start discarded with no drain"
        );
        assert!(
            logs_to_read(discarded(Drain::NONE), service(exited.clone())),
            "a start that crashed before its discard"
        );
        assert!(
            !logs_to_read(
                discarded(Drain::NONE),
                service(ContainerRuntimeObservation::Created)
            ),
            "a start Docker never ran has no lines"
        );
        assert!(!logs_to_read(discarded(Drain::NONE), hook(running)));
        assert!(!logs_to_read(discarded(Drain::NONE), hook(exited)));
    }

    #[test]
    fn a_slot_holding_another_spec_is_occupied_and_a_missing_target_is_not_found() {
        let participating = &LocalMachinePhase::Participating;
        assert_eq!(
            refused(&plan_container(
                run_new("img"),
                &running("other"),
                participating,
                Admission::Free
            )),
            Some(ContainerRefusalReason::SlotOccupied)
        );
        for want in [run_existing(), stopped()] {
            let shown = format!("{want:?}");
            assert_eq!(
                refused(&plan_container(
                    want,
                    &absent(),
                    participating,
                    Admission::Free
                )),
                Some(ContainerRefusalReason::NotFound),
                "{shown}"
            );
        }
    }

    #[test]
    fn a_signed_request_never_adopts_a_copy_admitted_unsigned() {
        let participating = &LocalMachinePhase::Participating;
        let admitted = |now: ContainerNow, admission| {
            let ContainerNow::Present(mut observed) = now else {
                unreachable!()
            };
            observed
                .try_update(|parts| parts.admission = Some(admission))
                .unwrap();
            ContainerNow::Present(observed)
        };
        for now in [running("img"), exited("img")] {
            for (wanted, held, adopts) in [
                (Admission::Signed, Admission::Free, false),
                (Admission::Signed, Admission::Signed, true),
                (Admission::Free, Admission::Free, true),
            ] {
                let shown = format!("{wanted:?} over {held:?} in {now:?}");
                assert_eq!(
                    refused(&plan_container(
                        run_new("img"),
                        &admitted(now.clone(), held),
                        participating,
                        wanted,
                    )),
                    (!adopts).then_some(ContainerRefusalReason::SlotOccupied),
                    "{shown}"
                );
            }
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
                    refused(&plan_container(
                        want,
                        &running("img"),
                        &phase,
                        Admission::Free
                    )),
                    Some(ContainerRefusalReason::NotParticipating),
                    "{shown}"
                );
            }
            assert_eq!(
                plan_container(stopped(), &running("img"), &phase, Admission::Free),
                ContainerStep::Stop {
                    id: id('a'),
                    drain: Drain::DEFAULT,
                    stop: StopOptions::default(),
                }
            );
            assert_eq!(
                plan_container(gone(), &exited("img"), &phase, Admission::Free),
                ContainerStep::Remove {
                    id: id('a'),
                    anonymous_volumes: named(),
                    logs_to_read: true,
                }
            );
        }
        assert_eq!(
            plan_container(
                run_new("img"),
                &absent(),
                &LocalMachinePhase::Joining,
                Admission::Free
            ),
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
                grant: None,
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
    let admitted = super::AdmittedSpec::test_signed(&new.slot.namespace, &new.resolved_spec);
    let starts = || {
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, path)| *method == Method::POST && path.ends_with("/start"))
            .count()
    };

    assert!(matches!(
        local
            .create_and_start(new.clone(), Runs::Spec, None, admitted.clone(), None)
            .await
            .unwrap(),
        super::Made::Acted(_, None)
    ));
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
    assert!(matches!(
        local
            .create_and_start(new, Runs::Spec, None, admitted, None)
            .await
            .unwrap(),
        super::Made::Acted(_, None)
    ));
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
        settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                ..Default::default()
            },
            None,
        )
        .await
    }

    /// [`settling`] on `fake`, holding the Sealing Key kept in `sealing`.
    async fn settling_with(fake: FakeDocker, sealing: Option<std::path::PathBuf>) -> Settling {
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
                    grant: None,
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .unwrap();
        let owner = RecordOwner::spawn(store).unwrap();
        let (runtime, fake) = fake_runtime_with(fake).await;
        let runtime = match sealing {
            Some(dir) => runtime.with_sealing_key(dir),
            None => runtime,
        };
        let local = LocalMachine::new(owner)
            .with_containers(Some(runtime))
            .with_zpool("true");
        Settling {
            local,
            fake,
            data_dir,
        }
    }

    /// `local` with a replica of its own, where an Operator's unsigned create looks up
    /// the names kept signed.
    async fn with_replica(local: LocalMachine) -> (LocalMachine, tokio::task::JoinHandle<()>) {
        let (replicated, server) = crate::corrosion::fake_cluster::store().await;
        let admin = crate::corrosion::AdminClient::new("/no/such/admin.sock");
        (local.with_cluster(Some((replicated, admin))), server)
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
        let wait = async {
            while !ready() {
                // real-wait: the daemon changes this state over the fake Docker's real TCP
                // socket, or in work detached from the test, and neither signals it.
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .expect("the daemon's state change arrives");
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
            async move { local.ensure_full_access(want).await }
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
                    ..
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

        let reply =
            tokio::time::timeout(Duration::from_secs(5), local.ensure_full_access(run(new)))
                .await
                .expect("a Started ensure answers without waiting out the monitor")
                .unwrap();

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: None,
                    ..
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
                    ..
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

        let reply =
            tokio::time::timeout(Duration::from_secs(20), local.ensure_full_access(run(new)))
                .await
                .expect("a Settled ensure answers")
                .unwrap();

        assert!(
            matches!(
                reply,
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(_),
                    health: Some(HealthVerdict::Healthy),
                    ..
                }
            ),
            "an inspect slower than the one-second deadline still answers, got {reply:?}"
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A cancelled Deployment's peer pull held this Machine's shared turn, so the next
    /// Deployment's creates waited for it. Work told its caller is gone ends, and frees the
    /// turn for the exclusive work behind it.
    #[tokio::test]
    async fn a_caller_that_stops_waiting_frees_the_turn_its_work_held() {
        let Settling {
            local, data_dir, ..
        } = settling().await;
        let started = Arc::new(tokio::sync::Notify::new());
        let caller = tokio::spawn({
            let local = local.clone();
            let started = Arc::clone(&started);
            async move {
                local
                    .finish_shared_with_caller(move |_, gone| async move {
                        started.notify_one();
                        gone.cancelled().await;
                        Ok(())
                    })
                    .await
            }
        });
        started.notified().await;

        caller.abort();

        tokio::time::timeout(Duration::from_secs(30), local.wait_for_admitted_work())
            .await
            .expect("work whose caller stopped waiting still holds the turn");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A sweep of staging names that never returns must not hold the turn: the pull
    /// answers once its tag is written, and the next Deployment's creates go ahead.
    #[tokio::test]
    async fn a_stalled_staging_sweep_holds_back_no_create() {
        let root =
            std::env::temp_dir().join(format!("ployz-stalled-cleanup-{}", MachineId::random()));
        std::fs::create_dir(&root).unwrap();
        let docker = root.join("docker");
        let hex = "1".repeat(64);
        crate::docker::test_support::write_script(
            &docker,
            &format!(
                r#"#!/bin/sh
[ "$1" = --host ] && shift 2
case "$1 $2 $5" in
  'image inspect {{{{range'*) exec sleep 600 ;;
  'image inspect {{{{json'*)
    case "$3" in
      127.0.0.1:*) echo '{{"digest":"sha256:{hex}"}}' ;;
      *) echo "No such image: $3" >&2; exit 1 ;;
    esac ;;
esac
"#
            ),
        );
        let Settling {
            local, data_dir, ..
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                containerd_store: true,
                docker_cli: Some(docker),
                ..Default::default()
            },
            None,
        )
        .await;
        let request = ployz_core::PullImageFromMachineRequest {
            pull: ployz_core::PeerImagePull::Reference {
                image: format!("app@sha256:{hex}"),
            },
            source: ployz_core::ImageIngestDestination {
                management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
                port: ployz_core::UNREGISTRY_PORT,
            },
            platform: "linux/amd64".into(),
        };

        tokio::time::timeout(
            Duration::from_secs(30),
            local.pull_image_from_machine(request),
        )
        .await
        .expect("the pull waits for its sweep")
        .unwrap();
        tokio::time::timeout(Duration::from_secs(30), local.wait_for_admitted_work())
            .await
            .expect("a stalled sweep holds the turn");
        std::fs::remove_dir_all(data_dir).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Before its tag a peer pull asks Docker which image store it runs. A caller that
    /// leaves while Docker has not answered frees the turn for the next create at once.
    #[tokio::test]
    async fn a_peer_pull_whose_caller_left_during_docker_info_holds_back_no_create() {
        let stalled = Arc::new(tokio::sync::Notify::new());
        let Settling {
            local, data_dir, ..
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                containerd_store: true,
                info_stall: Some(Arc::clone(&stalled)),
                ..Default::default()
            },
            None,
        )
        .await;
        let request = ployz_core::PullImageFromMachineRequest {
            pull: ployz_core::PeerImagePull::Reference {
                image: format!("app@sha256:{}", "1".repeat(64)),
            },
            source: ployz_core::ImageIngestDestination {
                management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
                port: ployz_core::UNREGISTRY_PORT,
            },
            platform: "linux/amd64".into(),
        };
        let caller = tokio::spawn({
            let local = local.clone();
            async move { local.pull_image_from_machine(request).await }
        });
        stalled.notified().await;

        caller.abort();

        // Shorter than the Docker client's own 5s bound, which would free the turn too.
        tokio::time::timeout(Duration::from_secs(3), local.wait_for_admitted_work())
            .await
            .expect("a pull whose caller left still holds the turn");
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
            async move {
                local
                    .ensure_full_access(run(new_container("started")))
                    .await
            }
        });
        let_requests_land().await;
        let new = new_container("started");
        let gone = tokio::spawn({
            let local = local.clone();
            async move {
                local
                    .ensure_full_access(ContainerWant::Gone {
                        container: ployz_core::ContainerLocator::Key(new.slot),
                        drain: ployz_core::Drain::NONE,
                        anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
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
            async move {
                local
                    .ensure_full_access(run(new_container("started")))
                    .await
            }
        });
        let_requests_land().await;
        let new = new_container("started");
        let gone = tokio::spawn({
            let local = local.clone();
            async move {
                local
                    .ensure_full_access(ContainerWant::Gone {
                        container: ployz_core::ContainerLocator::Key(new.slot),
                        drain: ployz_core::Drain::NONE,
                        anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
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
            ..
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
                    ..
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
            async move { local.ensure_full_access(want).await }
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

    /// A drain's start of a tag spec, pinned to the image its old copy ran. The tag
    /// names another image on this Machine.
    fn pinned_run() -> (ContainerWant, String) {
        let pin = format!("sha256:{}", "a".repeat(64));
        let mut new = new_container("settled");
        new.resolved_spec.update.monitor_millis = Some(0);
        // A public registry, so an Operator's start may pull it.
        new.resolved_spec.container.image = "nginx:1.29-alpine".into();
        new.image_prepared = true;
        new.image_id = Some(ployz_core::ImageDigest::parse(&pin).unwrap());
        (run(new), pin)
    }

    fn starts(fake: &FakeDocker) -> usize {
        fake.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(method, path)| {
                *method == axum::http::Method::POST && path.ends_with("/start")
            })
            .count()
    }

    /// A free create whose request read the name before a signed create kept it, then
    /// waited for its turn: it reads the name again in that turn, and makes nothing.
    #[tokio::test]
    async fn a_free_create_reads_the_kept_name_again_in_its_turn() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let (local, _replica) = with_replica(local).await;
        fake.images.lock().unwrap().insert(
            "nginx:1.29-alpine".into(),
            Some(format!("sha256:{}", "b".repeat(64))),
        );
        let mut new = new_container("settled");
        new.resolved_spec.container.image = "nginx:1.29-alpine".into();
        let want = run(new);
        let admitted = local
            .clone()
            .at_operator_scope()
            .admit_spec(&want, None, &[], crate::machine::Admit::Record)
            .await
            .unwrap();
        assert!(admitted.as_ref().is_some_and(|spec| !spec.signed()));
        let api = ployz_core::QualifiedService::parse("app/api").unwrap();
        local
            .replicated()
            .unwrap()
            .keep_signed_service(&api)
            .await
            .unwrap();

        let reply = local.ensure_container(want, admitted).await;

        assert!(
            matches!(
                reply,
                Err(super::super::Error::DeployRefused(
                    ployz_core::authority::AuthorityRefusal::SignedName { ref service }
                )) if *service == api
            ),
            "{reply:?}"
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

    /// Ployz Cloud never learns of a create this Server's own socket admitted, so it keeps
    /// no name a later Cloud deploy of the Service would be refused under, and reports
    /// free like one.
    #[tokio::test]
    async fn a_create_this_servers_socket_admits_keeps_no_name() {
        let Settling {
            local, data_dir, ..
        } = settling().await;
        let (local, _replica) = with_replica(local).await;
        let new = new_container("settled");
        let admitted =
            super::super::AdmittedSpec::test_signed(&new.slot.namespace, &new.resolved_spec);
        assert_eq!(admitted.admission(), ployz_core::Admission::Free);

        let reply = local
            .ensure_container(run(new), Some(admitted))
            .await
            .unwrap();

        assert!(
            matches!(reply, EnsuredContainer::Ensured { .. }),
            "{reply:?}"
        );
        assert!(
            !local
                .replicated()
                .unwrap()
                .signed_service_kept(&ployz_core::QualifiedService::parse("app/api").unwrap())
                .await
                .unwrap()
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn an_operator_pinned_start_runs_the_pinned_image_not_the_tag() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let (local, _replica) = with_replica(local).await;
        let (want, pin) = pinned_run();
        // The tag names another public image here by now.
        fake.images.lock().unwrap().insert(
            "nginx:1.29-alpine".into(),
            Some(format!("sha256:{}", "b".repeat(64))),
        );
        let admitted = local
            .clone()
            .at_operator_scope()
            .admit_spec(&want, None, &[], crate::machine::Admit::Record)
            .await
            .unwrap();
        assert!(admitted.as_ref().is_some_and(|spec| !spec.signed()));

        let reply = local.ensure_container(want, admitted).await.unwrap();

        let EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            ..
        } = reply
        else {
            panic!("expected the pinned Container, got {reply:?}");
        };
        let containers = fake.named_containers.as_ref().unwrap().lock().unwrap();
        let made = containers
            .values()
            .find(|container| container["Id"] == observed.container_id.as_str())
            .unwrap();
        assert_eq!(made["Image"], pin.as_str());
        drop(containers);
        // Nothing signed it, so the Machine pulled exactly that image itself.
        assert!(
            fake.requests
                .lock()
                .unwrap()
                .iter()
                .any(|(_, path)| { path.ends_with("/images/create") })
        );
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    /// A tag built or pulled with Full access is what a later Full start runs, with the
    /// Service's data. An Operator's start pulls its own image and must leave that tag.
    #[tokio::test]
    async fn an_operator_start_runs_its_own_pull_and_leaves_the_tag() {
        for policy in [
            ployz_core::PullPolicy::Missing,
            ployz_core::PullPolicy::Always,
        ] {
            let Settling {
                local,
                fake,
                data_dir,
            } = settling().await;
            let (local, _replica) = with_replica(local).await;
            let built = Some(format!("sha256:{}", "b".repeat(64)));
            let served = format!("sha256:{}", "e".repeat(64));
            fake.images
                .lock()
                .unwrap()
                .insert("nginx:1.29-alpine".into(), built.clone());
            fake.registry
                .lock()
                .unwrap()
                .insert("nginx:1.29-alpine".into(), served.clone());
            let mut new = new_container("settled");
            new.resolved_spec.update.monitor_millis = Some(0);
            new.resolved_spec.container.image = "nginx:1.29-alpine".into();
            new.resolved_spec.container.pull_policy = policy;
            let want = run(new);
            let admitted = local
                .clone()
                .at_operator_scope()
                .admit_spec(&want, None, &[], crate::machine::Admit::Record)
                .await
                .unwrap();
            assert!(admitted.as_ref().is_some_and(|spec| !spec.signed()));

            let reply = local.ensure_container(want, admitted).await.unwrap();

            let EnsuredContainer::Ensured {
                now: ContainerNow::Present(observed),
                ..
            } = reply
            else {
                panic!("{policy:?}: expected the Container, got {reply:?}");
            };
            let containers = fake.named_containers.as_ref().unwrap().lock().unwrap();
            let made = containers
                .values()
                .find(|container| container["Id"] == observed.container_id.as_str())
                .unwrap();
            assert_eq!(made["Image"], served.as_str(), "{policy:?}");
            drop(containers);
            assert_eq!(
                fake.images.lock().unwrap().get("nginx:1.29-alpine"),
                Some(&built),
                "{policy:?}: the tag moved"
            );
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }

    fn volume_start(
        new: &NewContainer,
        login: Option<ployz_core::SealedLogin>,
    ) -> ployz_core::Start {
        ployz_core::Start::Create {
            namespace: new.slot.namespace.clone(),
            resolved_spec: new.resolved_spec.clone(),
            creation_key: "move-1".to_owned().try_into().unwrap(),
            r#gen: new.r#gen,
            rotation: RunRotation::Pending,
            versions: Vec::new(),
            rule: None,
            registry_login: login,
            check: false,
        }
    }

    fn images(fake: &FakeDocker) -> Vec<String> {
        let containers = fake.named_containers.as_ref().unwrap().lock().unwrap();
        containers
            .values()
            .map(|container| container["Image"].as_str().unwrap().to_owned())
            .collect()
    }

    // Catches a moved Volume's start running whatever the Service's name points at
    // here rather than the image its admission resolved, as a registry rule's pull
    // does, and a resend running the image its promote recorded when the admission now
    // resolves another.
    #[tokio::test]
    async fn a_volume_start_runs_the_image_its_admission_resolved() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let resolved =
            ployz_core::ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap();
        let other = format!("sha256:{}", "b".repeat(64));
        let new = new_container("settled");
        let admitted = || {
            super::super::AdmittedSpec::test_pinned(
                &new.slot.namespace,
                &new.resolved_spec,
                resolved.clone(),
            )
        };
        let place = |recorded: Option<&str>| {
            local.place_holder_image(
                new.resolved_spec.clone(),
                admitted(),
                recorded.map(str::to_owned),
                None,
            )
        };

        let disagreeing = place(Some(&other)).await.map(|placed| placed.id);
        assert!(
            matches!(
                disagreeing,
                Err(super::super::Error::Docker(
                    crate::docker::Error::PlacedImageDisagrees { .. }
                ))
            ),
            "{disagreeing:?}"
        );
        assert_eq!(
            place(Some(resolved.as_str())).await.unwrap().id,
            resolved.as_str()
        );

        let placed = place(None).await.unwrap();
        local
            .start_volume_holder(
                volume_start(&new, None),
                "app_data".parse().unwrap(),
                ployz_core::VolumeGen::new(1),
                Some(placed),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(images(&fake), [resolved.as_str()]);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches a moved Volume's start that a signed version admitted, with no image
    // resolved, pulling its private image with no login on a Machine that lacks it.
    #[tokio::test]
    async fn a_volume_start_pulls_its_absent_private_image_with_the_services_login() {
        let cluster = crate::sealed_env::test_support::Cluster::new();
        let mut new = new_container("settled");
        new.resolved_spec.container.image = "acme/private:1".to_owned();
        new.resolved_spec.container.pull_policy = ployz_core::PullPolicy::Never;
        let image = new.resolved_spec.container.image.clone();
        let Settling {
            local,
            fake,
            data_dir,
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                private: Arc::new(Mutex::new([image.clone()].into())),
                images: Arc::new(Mutex::new([(image.clone(), None)].into())),
                ..Default::default()
            },
            Some(cluster.dir.0.clone()),
        )
        .await;
        let login = cluster.seal_login(
            ployz_core::DOCKER_HUB,
            &ployz_core::RegistryAuth {
                username: Some("bot".into()),
                password: "token".into(),
            },
        );
        let placed = local
            .place_holder_image(
                new.resolved_spec.clone(),
                super::super::AdmittedSpec::test_signed(&new.slot.namespace, &new.resolved_spec),
                None,
                Some(login.clone()),
            )
            .await
            .unwrap();
        local
            .start_volume_holder(
                volume_start(&new, Some(login)),
                "app_data".parse().unwrap(),
                ployz_core::VolumeGen::new(1),
                Some(placed),
            )
            .await
            .unwrap()
            .unwrap();
        let pulled = fake.images.lock().unwrap().get(&image).cloned().flatten();
        assert_eq!(images(&fake), [pulled.expect("the start pulled the image")]);
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches a moved Volume's start opening the Service's login for an image it already
    // holds, which a Docker Hub mirror makes this Server refuse though nothing is pulled.
    #[tokio::test]
    async fn a_volume_start_opens_no_login_for_an_image_it_holds() {
        let cluster = crate::sealed_env::test_support::Cluster::new();
        let mut new = new_container("settled");
        let held = format!("acme/private@sha256:{}", "c".repeat(64));
        new.resolved_spec.container.image.clone_from(&held);
        new.resolved_spec.container.pull_policy = ployz_core::PullPolicy::Never;
        let Settling {
            local,
            fake: _,
            data_dir,
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                mirrors: Arc::new(Mutex::new(vec!["https://mirror.example".to_owned()])),
                ..Default::default()
            },
            Some(cluster.dir.0.clone()),
        )
        .await;
        let login = cluster.seal_login(
            ployz_core::DOCKER_HUB,
            &ployz_core::RegistryAuth {
                username: Some("bot".into()),
                password: "token".into(),
            },
        );
        let placed = local
            .place_holder_image(
                new.resolved_spec.clone(),
                super::super::AdmittedSpec::test_signed(&new.slot.namespace, &new.resolved_spec),
                None,
                Some(login),
            )
            .await
            .map(|placed| placed.id);
        assert!(placed.is_ok(), "{placed:?}");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches a pull its policy skips opening the Service's login anyway, which a Docker
    // Hub mirror makes this Server refuse though no registry is asked.
    #[tokio::test]
    async fn a_pull_its_policy_skips_opens_no_login() {
        let cluster = crate::sealed_env::test_support::Cluster::new();
        let mut new = new_container("settled");
        let held = format!("acme/private@sha256:{}", "c".repeat(64));
        new.resolved_spec.container.image.clone_from(&held);
        new.resolved_spec.container.pull_policy = ployz_core::PullPolicy::Never;
        let Settling {
            local,
            fake: _,
            data_dir,
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                mirrors: Arc::new(Mutex::new(vec!["https://mirror.example".to_owned()])),
                ..Default::default()
            },
            Some(cluster.dir.0.clone()),
        )
        .await;
        let login = cluster.seal_login(
            ployz_core::DOCKER_HUB,
            &ployz_core::RegistryAuth {
                username: Some("bot".into()),
                password: "token".into(),
            },
        );
        let placed = local
            .pull_image(ployz_core::PullImageRequest {
                image: held,
                policy: ployz_core::PullPolicy::Never,
                registry_login: Some(login),
                registry_auth: None,
            })
            .await;
        assert!(placed.is_ok(), "{placed:?}");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches a Volume start that found its image here, skipped the login, and then pulled
    // anonymously once the image was gone before the pull's own check.
    #[tokio::test]
    async fn a_volume_start_answers_the_image_it_found_or_pulls_it_with_the_login() {
        let cluster = crate::sealed_env::test_support::Cluster::new();
        let mut new = new_container("settled");
        let held = "acme/private:1".to_owned();
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        new.resolved_spec.container.image.clone_from(&held);
        new.resolved_spec.container.pull_policy = ployz_core::PullPolicy::Missing;
        let Settling {
            local,
            fake,
            data_dir,
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                image_barrier: Some(gate.clone()),
                private: Arc::new(Mutex::new([held.clone()].into())),
                ..Default::default()
            },
            Some(cluster.dir.0.clone()),
        )
        .await;
        let login = cluster.seal_login(
            ployz_core::DOCKER_HUB,
            &ployz_core::RegistryAuth {
                username: Some("bot".into()),
                password: "token".into(),
            },
        );
        let task = tokio::spawn(async move {
            for inspect in 1.. {
                gate.wait().await;
                if inspect == 2 {
                    fake.images.lock().unwrap().insert(held.clone(), None);
                }
                gate.wait().await;
            }
        });
        let placed = local
            .place_holder_image(
                new.resolved_spec.clone(),
                super::super::AdmittedSpec::test_signed(&new.slot.namespace, &new.resolved_spec),
                None,
                Some(login),
            )
            .await
            .map(|placed| placed.id);
        task.abort();
        assert!(placed.is_ok(), "{placed:?}");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    #[tokio::test]
    async fn a_pinned_start_never_runs_another_image_in_its_slot() {
        for state in [None, Some("running"), Some("exited")] {
            let Settling {
                local,
                fake,
                data_dir,
            } = settling().await;
            let (want, _) = pinned_run();
            let EnsuredContainer::Ensured {
                now: ContainerNow::Present(first),
                ..
            } = local.ensure_full_access(want.clone()).await.unwrap()
            else {
                panic!("the first start made its Container");
            };
            if let Some(state) = state {
                for container in fake
                    .named_containers
                    .as_ref()
                    .unwrap()
                    .lock()
                    .unwrap()
                    .values_mut()
                {
                    container["Image"] = format!("sha256:{}", "b".repeat(64)).into();
                    container["State"] = json!({"Status": state});
                }
            }
            let started = starts(&fake);

            let reply = local.ensure_full_access(want).await.unwrap();

            match (state, reply) {
                // A resend finds its own Container and answers it.
                (
                    None,
                    EnsuredContainer::Ensured {
                        now: ContainerNow::Present(again),
                        ..
                    },
                ) => assert_eq!(again.container_id, first.container_id),
                (
                    Some(_),
                    EnsuredContainer::Refused {
                        now: ContainerNow::Present(occupant),
                        refusal,
                    },
                ) => {
                    assert_eq!(
                        refusal.reason,
                        ployz_core::ContainerRefusalReason::ImageDiffers
                    );
                    assert_eq!(occupant.container_id, first.container_id);
                }
                (state, reply) => panic!("{state:?}: unexpected {reply:?}"),
            }
            assert_eq!(starts(&fake), started, "{state:?}: nothing else started");
            std::fs::remove_dir_all(data_dir).unwrap();
        }
    }

    /// An unsigned create that adopts a Container another request made in its slot, from
    /// another Free Build's image, never starts it.
    #[tokio::test]
    async fn an_adopted_container_serves_a_receipt_only_for_its_image() {
        let Settling {
            local,
            fake,
            data_dir,
        } = settling().await;
        let (local, _replica) = with_replica(local).await;
        let image = |hex: &str| {
            crate::docker_image::FreeImage::for_test(
                ployz_core::ImageDigest::parse(format!("sha256:{}", hex.repeat(64))).unwrap(),
            )
        };
        let new = new_container("settled");
        let admitted = local
            .clone()
            .at_operator_scope()
            .admit_spec(&run(new.clone()), None, &[], crate::machine::Admit::Record)
            .await
            .unwrap()
            .unwrap();
        assert!(!admitted.signed());
        let first = local
            .create_and_start(
                new.clone(),
                Runs::Free(image("a")),
                None,
                admitted.clone(),
                None,
            )
            .await
            .unwrap();
        assert!(
            matches!(first, super::super::Made::Acted(_, None)),
            "{first:?}"
        );
        let started = starts(&fake);

        // A second request read the slot empty, then its create adopted the first one's.
        let adopted = local
            .create_and_start(new, Runs::Free(image("b")), None, admitted, None)
            .await
            .unwrap();

        assert!(
            matches!(
                &adopted,
                super::super::Made::Acted(_, Some(refusal))
                    if refusal.reason == ployz_core::ContainerRefusalReason::ImageDiffers
            ),
            "{adopted:?}"
        );
        assert_eq!(starts(&fake), started, "nothing else started");
        std::fs::remove_dir_all(data_dir).unwrap();
    }

    // Catches a preview Writer's start pulling its image with no login, which a private
    // registry refuses though the start carried the Service's sealed login.
    #[tokio::test]
    async fn a_preview_start_pulls_with_the_services_login() {
        let cluster = crate::sealed_env::test_support::Cluster::new();
        let image = "acme/private:1".to_owned();
        let Settling {
            local,
            fake,
            data_dir,
        } = settling_with(
            FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                private: Arc::new(Mutex::new([image.clone()].into())),
                images: Arc::new(Mutex::new([(image.clone(), None)].into())),
                ..Default::default()
            },
            Some(cluster.dir.0.clone()),
        )
        .await;
        let home = ployz_core::PreviewHome::of(&local.record().id());
        let namespace = Namespace::parse(format!("pv-0123456789abcdef-{home}-00c0ffee")).unwrap();
        let mut spec = new_container("settled").resolved_spec;
        spec.container.image.clone_from(&image);
        spec.container.pull_policy = ployz_core::PullPolicy::Always;
        let login = cluster.seal_login(
            ployz_core::DOCKER_HUB,
            &ployz_core::RegistryAuth {
                username: Some("bot".into()),
                password: "token".into(),
            },
        );
        let start: ployz_core::Start = serde_json::from_value(json!({"kind": "create",
            "namespace": namespace, "resolved_spec": spec, "creation_key": "move-1",
            "gen": 2, "rotation": "pending", "registry_login": login}))
        .unwrap();

        let admitted = local
            .admit_volume_start(&format!("{namespace}_data").parse().unwrap(), &start)
            .await;

        let pulled = fake.images.lock().unwrap().get(&image).cloned().flatten();
        let inspected = admitted
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .and_then(|admitted| admitted.preview_image())
            .map(|preview| preview.id().to_owned());
        assert!(pulled.is_some(), "the start didn't pull: {admitted:?}");
        assert_eq!(inspected, pulled, "{admitted:?}");
        std::fs::remove_dir_all(data_dir).unwrap();
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
                        grant: None,
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
        } = Box::pin(local.ensure_full_access(want)).await.unwrap()
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
            .observation()
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
            .lifecycle()
            .remove(&fixture.id, crate::docker::VolumeDisposal::Keep, false)
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
            .lifecycle()
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

    /// The daemon exits once `close_when_idle` returns, so a retired copy that was
    /// stopping when shutdown began is removed by then, not left Exited.
    #[tokio::test]
    async fn shutdown_mid_retirement_waits_for_the_removal() {
        let fixture = Fixture::start().await;
        let sent = |wanted: axum::http::Method, suffix: &'static str| {
            let requests = fixture.fake.requests.clone();
            move || {
                requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(method, path)| *method == wanted && path.contains(suffix))
            }
        };
        let stop_sent = sent(axum::http::Method::POST, "/stop");
        let delete_sent = sent(axum::http::Method::DELETE, "/containers/");
        let grace = fixture.fake.stop_gate.clone().write_owned().await;
        let removal = tokio::spawn({
            let local = fixture.local.clone();
            let id = fixture.id;
            async move {
                local
                    .ensure_full_access(ContainerWant::Gone {
                        container: ContainerLocator::Id(id),
                        drain: Drain::NONE,
                        anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
                    })
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(10), async {
            while !stop_sent() {
                // real-wait: the stop reaches the fake Docker over its TCP socket, which
                // emits no event.
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the stop is sent");

        let closing = tokio::spawn({
            let local = fixture.local.clone();
            async move { local.close_when_idle(Duration::from_secs(10)).await }
        });
        // real-wait: let shutdown queue behind the stop before the stop ends.
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(grace);

        assert!(matches!(closing.await.unwrap(), Some(Ok(()))));
        assert!(
            delete_sent(),
            "shutdown released the record before the stopped copy was removed"
        );
        removal.await.unwrap().unwrap();
        fixture.finish();
    }

    mod removal_wait {
        use super::*;
        use crate::machine::local_machine::container::log_read_remaining;

        const GRACE: Duration = Duration::from_secs(2);

        fn removal(fixture: &Fixture, drain: Drain) -> ContainerWant {
            ContainerWant::Gone {
                container: ContainerLocator::Id(fixture.id),
                drain,
                anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
            }
        }

        async fn connect(fixture: &Fixture) {
            fixture
                .local
                .set_push_target(ployz_core::SetPushTargetRequest {
                    url: Some(
                        "https://cloud.example/api/machines/aa/observe"
                            .parse()
                            .unwrap(),
                    ),
                })
                .await
                .unwrap();
        }

        async fn removed_in(fixture: &Fixture, drain: Drain) -> Duration {
            let began = std::time::Instant::now();
            let reply = fixture
                .local
                .ensure_full_access(removal(fixture, drain))
                .await
                .unwrap();
            assert!(
                matches!(
                    reply,
                    EnsuredContainer::Ensured {
                        now: ContainerNow::Absent { .. },
                        ..
                    }
                ),
                "expected the container gone, got {reply:?}"
            );
            began.elapsed()
        }

        #[tokio::test]
        async fn removing_a_running_container_waits_for_the_collector_to_read_its_log() {
            let fixture = Fixture::start().await;
            connect(&fixture).await;

            let took = removed_in(&fixture, Drain::try_from(1).unwrap()).await;

            assert!(took >= GRACE, "removed after {took:?}");
            fixture.finish();
        }

        #[tokio::test]
        async fn a_discarded_start_waits_too_whatever_its_drain() {
            let fixture = Fixture::start().await;
            connect(&fixture).await;

            let took = removed_in(&fixture, Drain::NONE).await;

            assert!(took >= GRACE, "discarded after {took:?}");
            fixture.finish();
        }

        #[test]
        fn the_wait_is_what_remains_of_the_grace_after_the_exit() {
            let now = chrono::Utc::now();
            let ago = |millis| Some(now - chrono::Duration::milliseconds(millis));
            for (exited_at, wait) in [
                (ago(0), GRACE),
                (ago(500), Duration::from_millis(1_500)),
                (ago(2_000), Duration::ZERO),
                (ago(60_000), Duration::ZERO),
                (ago(-1_000), GRACE),
                (None, GRACE),
            ] {
                assert_eq!(
                    log_read_remaining(exited_at, now),
                    wait,
                    "exited {exited_at:?}"
                );
            }
        }

        #[tokio::test]
        async fn a_container_that_exited_long_ago_is_removed_without_waiting() {
            let fixture = Fixture::start().await;
            connect(&fixture).await;
            fixture.docker_sets(&[
                ("/State/Status", json!("exited")),
                ("/State/Running", json!(false)),
                (
                    "/State/FinishedAt",
                    json!((chrono::Utc::now() - chrono::Duration::seconds(30)).to_rfc3339()),
                ),
            ]);

            let took = removed_in(&fixture, Drain::NONE).await;

            assert!(took < GRACE, "removed after {took:?}");
            fixture.finish();
        }

        #[tokio::test]
        async fn a_removal_with_no_cloud_does_not_wait() {
            let fixture = Fixture::start().await;
            let took = removed_in(&fixture, Drain::try_from(1).unwrap()).await;
            assert!(took < GRACE, "a removal with no Collector took {took:?}");
            fixture.finish();
        }

        #[tokio::test]
        async fn the_wait_leaves_the_machines_turn_free() {
            let fixture = Fixture::start().await;
            connect(&fixture).await;
            let local = fixture.local.clone();
            let want = removal(&fixture, Drain::try_from(1).unwrap());
            let removing = tokio::spawn(async move { local.ensure_full_access(want).await });
            // real-wait: the removal reaches its wait through the fake Docker's TCP
            // socket, which emits no event.
            tokio::time::sleep(Duration::from_millis(1_500)).await;

            tokio::time::timeout(
                Duration::from_millis(300),
                fixture.local.owner().admission_lock().write_owned(),
            )
            .await
            .expect("the removal's wait holds the Machine's turn");

            assert!(!removing.is_finished(), "the removal did not wait");
            removing.await.unwrap().unwrap();
            fixture.finish();
        }
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
                    tokio::time::timeout(Duration::from_millis(150), &mut stop)
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
        async fn stop_checked_stops_a_copy_without_sockets_without_waiting_for_the_cap() {
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

mod reserved_lifecycle {
    //! A stop or removal of a reserved Container against the route that places it.

    use std::{collections::BTreeMap, sync::Mutex, time::Duration};

    use ployz_core::{
        ContainerKind, ContainerLocator, ContainerNow, ContainerWant, Drain, EnsuredContainer,
        MachineName, QualifiedService, StopOptions,
    };

    use super::*;
    use crate::docker::{
        ContainerRuntime,
        test_support::{FakeDocker, fake_runtime_with},
    };

    struct Fixture {
        data_dir: std::path::PathBuf,
        local: LocalMachine,
        runtime: ContainerRuntime,
    }

    impl Fixture {
        async fn start() -> Self {
            let data_dir =
                std::env::temp_dir().join(format!("ployzd-reserved-lock-{}", MachineId::random()));
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
                        grant: None,
                    },
                    wireguard_mtu: None,
                    pinned: Default::default(),
                })
                .unwrap();
            let owner = RecordOwner::spawn(store).unwrap();
            let (runtime, _fake) = fake_runtime_with(FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                ..Default::default()
            })
            .await;
            let local = LocalMachine::new(owner)
                .with_containers(Some(runtime.clone()))
                .with_zpool("true");
            Self {
                data_dir,
                local,
                runtime,
            }
        }

        async fn create(&self, spec: &ResolvedServiceSpec) -> ployz_core::ContainerId {
            self.local
                .create_container(
                    ContainerKind::ServiceContainer,
                    &Namespace::system(),
                    spec,
                    None,
                    None,
                    None,
                )
                .await
                .unwrap()
                .container_id
        }

        /// Whether `want` is still unanswered while the door lifecycle lock is held, and
        /// its answer once the lock is released.
        async fn under_the_lock(&self, want: ContainerWant) -> (bool, EnsuredContainer) {
            let held = self.runtime.doors().hold().await;
            let local = self.local.clone();
            let ensuring = tokio::spawn(async move { local.ensure_full_access(want).await });
            // real-wait: the want reaches the lock through the fake Docker's TCP socket,
            // which emits no event.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let waited = !ensuring.is_finished();
            drop(held);
            let answer = ensuring.await.unwrap().unwrap();
            (waited, answer)
        }

        fn finish(self) {
            std::fs::remove_dir_all(self.data_dir).unwrap();
        }
    }

    fn stopped(id: ployz_core::ContainerId) -> ContainerWant {
        ContainerWant::Stopped {
            container: ContainerLocator::Id(id),
            drain: Drain::NONE,
            stop: StopOptions::default(),
        }
    }

    fn gone(id: ployz_core::ContainerId) -> ContainerWant {
        ContainerWant::Gone {
            container: ContainerLocator::Id(id),
            drain: Drain::NONE,
            anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
        }
    }

    fn router() -> ResolvedServiceSpec {
        crate::images::private_access_router(
            "10.210.0.0/16".parse().unwrap(),
            &ployz_core::Ciphertext::from_bytes(vec![9; 48]),
            ployz_core::PrivateAccessStateId::random(),
        )
    }

    fn holder() -> ResolvedServiceSpec {
        crate::images::DoorSpecs::public(ServiceId::parse("c".repeat(32)).unwrap()).holder
    }

    #[tokio::test]
    async fn a_stop_or_removal_of_a_door_waits_for_the_doors_route_under_way() {
        let fixture = Fixture::start().await;
        let id = fixture.create(&holder()).await;
        assert_eq!(
            fixture
                .runtime
                .observation()
                .inspect_managed(&id, &fixture.local.record().id())
                .await
                .unwrap()
                .identity(),
            QualifiedService::system_ingress_holder()
        );

        let (waited, stop) = fixture.under_the_lock(stopped(id)).await;
        assert!(
            waited,
            "a door's stop answered while the route held the lock"
        );
        assert!(matches!(
            stop,
            EnsuredContainer::Ensured {
                now: ContainerNow::Present(_),
                ..
            }
        ));
        let (waited, removal) = fixture.under_the_lock(gone(id)).await;
        assert!(
            waited,
            "a door's removal answered while the route held the lock"
        );
        assert!(matches!(
            removal,
            EnsuredContainer::Ensured {
                now: ContainerNow::Absent { .. },
                ..
            }
        ));
        fixture.finish();
    }

    #[tokio::test]
    async fn a_stop_or_removal_of_the_router_waits_for_the_placement_route_under_way() {
        let fixture = Fixture::start().await;
        let id = fixture.create(&router()).await;
        assert_eq!(
            fixture
                .runtime
                .observation()
                .inspect_managed(&id, &fixture.local.record().id())
                .await
                .unwrap()
                .identity(),
            QualifiedService::system_tailscale()
        );

        let (waited, stop) = fixture.under_the_lock(stopped(id)).await;
        assert!(
            waited,
            "the router's stop answered while its route held the lock"
        );
        assert!(matches!(
            stop,
            EnsuredContainer::Ensured {
                now: ContainerNow::Present(_),
                ..
            }
        ));
        let (waited, removal) = fixture.under_the_lock(gone(id)).await;
        assert!(
            waited,
            "the router's removal answered while its route held the lock"
        );
        assert!(matches!(
            removal,
            EnsuredContainer::Ensured {
                now: ContainerNow::Absent { .. },
                ..
            }
        ));
        fixture.finish();
    }
}

/// A removal takes a Container's anonymous volumes only while the Container this Machine
/// recorded, when it made it, as taking that one's place runs, routed and not stopping,
/// whatever the caller asked.
mod anonymous_volumes {
    use std::{collections::BTreeMap, path::PathBuf, sync::Mutex};

    use ployz_core::{
        AnonymousVolumes, ContainerId, ContainerKind, ContainerLocator, ContainerNow,
        ContainerWant, CopyRotation, DeploymentLogId, Drain, EnsuredContainer, MachineName,
        NewContainer, RunRotation, RunTarget, ServiceName, StopOptions,
    };

    use super::*;
    use crate::docker::test_support::{FakeDocker, fake_runtime_with, spec_with_sources};

    use ContainerKind::{PreDeployHook as Hook, ServiceContainer as Copy};

    struct Machine {
        local: LocalMachine,
        fake: FakeDocker,
        data_dir: PathBuf,
    }

    impl Machine {
        async fn new() -> Self {
            let data_dir =
                std::env::temp_dir().join(format!("ployzd-anonymous-{}", MachineId::random()));
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
                        grant: None,
                    },
                    wireguard_mtu: None,
                    pinned: Default::default(),
                })
                .unwrap();
            let owner = RecordOwner::spawn(store).unwrap();
            let (runtime, fake) = fake_runtime_with(FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                list_named: true,
                ..Default::default()
            })
            .await;
            let local = LocalMachine::new(owner)
                .with_containers(Some(runtime))
                .with_zpool("true");
            Self {
                local,
                fake,
                data_dir,
            }
        }

        /// Run a new Container of `service` and `kind` in slot `key`, made to replace
        /// `replaces`, and route it.
        async fn run(
            &self,
            (service, kind): (&str, ContainerKind),
            key: &str,
            replaces: Option<ContainerId>,
        ) -> ContainerId {
            let id = self.create((service, kind), key, replaces).await;
            self.rotate(id, CopyRotation::In).await;
            id
        }

        /// Run a new Container as [`Self::run`] does, left Pending.
        async fn create(
            &self,
            (service, kind): (&str, ContainerKind),
            key: &str,
            replaces: Option<ContainerId>,
        ) -> ContainerId {
            let mut resolved_spec = spec_with_sources(vec![]);
            resolved_spec.name = ServiceName::parse(service).unwrap();
            let id = if service == "api" { 'a' } else { 'b' };
            resolved_spec.service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
            let new = NewContainer {
                slot: ployz_core::SlotKey {
                    namespace: Namespace::parse("app").unwrap(),
                    service: resolved_spec.name.clone(),
                    kind,
                    creation_key: ployz_core::CreationKey::try_from(key.to_owned()).unwrap(),
                },
                resolved_spec,
                r#gen: ployz_core::Gen::FIRST,
                deployment_id: None,
                registry_auth: None,
                registry_login: None,
                image_prepared: false,
                answer: ployz_core::AnswerWhen::Started,
                image_id: None,
                replaces,
                free_receipt: None,
            };
            let reply = self
                .local
                .ensure_full_access(ContainerWant::Running {
                    container: RunTarget::New(new),
                    rotation: RunRotation::Pending,
                })
                .await
                .unwrap();
            match reply {
                EnsuredContainer::Ensured {
                    now: ContainerNow::Present(observed),
                    ..
                } => observed.container_id,
                other @ (EnsuredContainer::Ensured { .. } | EnsuredContainer::Refused { .. }) => {
                    panic!("expected {key} running, got {other:?}")
                }
            }
        }

        /// Record `id` at `rotation`, the write an In, Held or Out want ends with. An In
        /// want also reads the Cluster's replicated copies, which this Machine has none of.
        async fn rotate(&self, id: ContainerId, rotation: CopyRotation) {
            let lifecycle = self.local.docker().unwrap().lifecycle();
            let turn = lifecycle.rotation_turn(&id).await.unwrap();
            turn.write(&rotation).await.unwrap();
        }

        async fn stop(&self, id: ContainerId) {
            self.local
                .ensure_full_access(ContainerWant::Stopped {
                    container: ContainerLocator::Id(id),
                    drain: Drain::NONE,
                    stop: StopOptions::default(),
                })
                .await
                .unwrap();
        }

        /// Take `id` out of routing and remove it, as cleanup does, naming `replaced_by`
        /// as the copy that took its place, and answer whether its anonymous volumes went
        /// too. Naming none asks to keep them.
        async fn remove(&self, id: ContainerId, replaced_by: Option<ContainerId>) -> bool {
            self.rotate(id, CopyRotation::Out).await;
            // The wire form, since a caller may name any copy.
            let anonymous_volumes: AnonymousVolumes = match replaced_by {
                Some(by) => serde_json::from_value(serde_json::json!({
                    "anonymous_volumes": "remove",
                    "replaced_by": by,
                }))
                .unwrap(),
                None => AnonymousVolumes::keep(),
            };
            let reply = self
                .local
                .ensure_full_access(ContainerWant::Gone {
                    container: ContainerLocator::Id(id),
                    drain: Drain::NONE,
                    anonymous_volumes,
                })
                .await
                .unwrap();
            assert!(
                matches!(
                    reply,
                    EnsuredContainer::Ensured {
                        now: ContainerNow::Absent { .. },
                        ..
                    }
                ),
                "expected {id} gone, got {reply:?}"
            );
            self.fake
                .removals
                .lock()
                .unwrap()
                .iter()
                .find(|(removed, _)| *removed == id.to_string())
                .map(|(_, volumes)| *volumes)
                .expect("Docker removed the Container")
        }

        fn finish(self) {
            std::fs::remove_dir_all(self.data_dir).unwrap();
        }
    }

    fn held() -> CopyRotation {
        CopyRotation::Held(DeploymentLogId::parse("deploy-1").unwrap())
    }

    #[tokio::test]
    async fn a_removal_naming_a_copy_that_replaced_nothing_keeps_them() {
        let machine = Machine::new().await;
        // A copy taken off its Server, an earlier run's Pending copy, and an old hook
        // removed before the new hook is made: nothing was made to replace any of them,
        // whatever copy the removal names.
        for (kind, key) in [(Copy, "copy"), (Hook, "hook")] {
            let old = machine.run(("api", kind), key, None).await;
            let beside = machine
                .run(("api", kind), &format!("{key}-beside"), None)
                .await;
            assert!(
                !machine.remove(old, Some(beside)).await,
                "the {key}'s anonymous volumes went with nothing in its place"
            );
        }
        machine.finish();
    }

    #[tokio::test]
    async fn a_removal_naming_its_running_replacement_takes_them() {
        let machine = Machine::new().await;
        let old = machine.run(("api", Copy), "old", None).await;
        let new = machine.run(("api", Copy), "new", Some(old)).await;

        assert!(machine.remove(old, Some(new)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_stop_first_replacement_of_a_held_copy_takes_them() {
        let machine = Machine::new().await;
        // Stop-first holds the old copy for its run and stops it before the new one is made.
        let old = machine.run(("api", Copy), "old", None).await;
        machine.rotate(old, held()).await;
        machine.stop(old).await;
        let new = machine.run(("api", Copy), "new", Some(old)).await;

        assert!(machine.remove(old, Some(new)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_removal_naming_a_copy_other_than_its_recorded_replacement_keeps_them() {
        let machine = Machine::new().await;
        // `b` was made to replace `a`, and `c` is an unrelated copy beside them, as a
        // scale-down that keeps only `c` finds them.
        let a = machine.run(("api", Copy), "a", None).await;
        let b = machine.run(("api", Copy), "b", Some(a)).await;
        let c = machine.run(("api", Copy), "c", None).await;

        assert!(
            !machine.remove(a, Some(c)).await,
            "a copy that replaced nothing freed a's data"
        );
        assert!(!machine.remove(b, Some(c)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_replacement_of_another_copy_keeps_them() {
        let machine = Machine::new().await;
        // Two old copies of one Service, and one new copy made to replace the first.
        let first = machine.run(("api", Copy), "first", None).await;
        let second = machine.run(("api", Copy), "second", None).await;
        let new = machine.run(("api", Copy), "new", Some(first)).await;

        assert!(
            !machine.remove(second, Some(new)).await,
            "a copy made to replace another took this one's place"
        );
        // Undoing a later copy while the old one still runs: the old one replaces nothing.
        let undone = machine.run(("api", Copy), "undone", Some(first)).await;
        assert!(!machine.remove(undone, Some(first)).await);
        assert!(machine.remove(first, Some(new)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_second_claim_on_a_copy_records_nothing() {
        let machine = Machine::new().await;
        let old = machine.run(("api", Copy), "old", None).await;
        let first = machine.run(("api", Copy), "first", Some(old)).await;
        // A second copy names the same old one, then the first copy goes.
        let second = machine.run(("api", Copy), "second", Some(old)).await;
        machine.remove(first, None).await;

        assert!(
            !machine.remove(old, Some(second)).await,
            "a second copy made for a slot already taken freed the old copy's data"
        );
        machine.finish();
    }

    #[tokio::test]
    async fn a_replacement_of_another_service_or_kind_keeps_them() {
        let machine = Machine::new().await;
        for (replacement, key) in [(("web", Copy), "web"), (("api", Hook), "hook")] {
            let old = machine
                .run(("api", Copy), &format!("{key}-old"), None)
                .await;
            let new = machine.run(replacement, key, Some(old)).await;
            assert!(!machine.remove(old, Some(new)).await, "{key}");
        }
        machine.finish();
    }

    #[tokio::test]
    async fn a_replacement_that_is_not_routed_keeps_them() {
        let machine = Machine::new().await;
        // A replacement that never came In, and one taken Out while it still runs.
        let pending = machine.run(("api", Copy), "pending", None).await;
        let new = machine
            .create(("api", Copy), "pending-new", Some(pending))
            .await;
        assert!(!machine.remove(pending, Some(new)).await);

        let out = machine.run(("api", Copy), "out", None).await;
        let new = machine.run(("api", Copy), "out-new", Some(out)).await;
        machine.rotate(new, CopyRotation::Out).await;
        assert!(!machine.remove(out, Some(new)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_replacement_that_is_stopping_keeps_them() {
        let machine = Machine::new().await;
        let old = machine.run(("api", Copy), "old", None).await;
        let new = machine.run(("api", Copy), "new", Some(old)).await;
        // A stop has marked the replacement and is draining it; it still runs.
        let _stopping = machine
            .local
            .docker()
            .unwrap()
            .lifecycle()
            .withdraw_locked(new)
            .await;

        assert!(!machine.remove(old, Some(new)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_replacement_that_stopped_or_went_keeps_them() {
        let machine = Machine::new().await;
        let stopped = machine.run(("api", Copy), "stopped", None).await;
        let new = machine
            .run(("api", Copy), "stopped-new", Some(stopped))
            .await;
        machine.rotate(new, held()).await;
        machine.stop(new).await;
        assert!(!machine.remove(stopped, Some(new)).await);

        // A scale-down that removes the replacement first, then the copy it replaced.
        let left = machine.run(("api", Copy), "left", None).await;
        let removed = machine.run(("api", Copy), "left-new", Some(left)).await;
        machine.remove(removed, None).await;
        assert!(!machine.remove(left, Some(removed)).await);
        machine.finish();
    }

    #[tokio::test]
    async fn a_replacement_on_another_server_keeps_them() {
        let (here, there) = (Machine::new().await, Machine::new().await);
        let old = here.run(("api", Copy), "old", None).await;
        let moved = there.run(("api", Copy), "moved", Some(old)).await;

        assert!(!here.remove(old, Some(moved)).await);
        here.finish();
        there.finish();
    }

    #[tokio::test]
    async fn a_removal_that_names_no_copy_keeps_them_beside_its_replacement() {
        let machine = Machine::new().await;
        let old = machine.run(("api", Copy), "old", None).await;
        machine.run(("api", Copy), "new", Some(old)).await;

        assert!(!machine.remove(old, None).await);
        machine.finish();
    }
}
