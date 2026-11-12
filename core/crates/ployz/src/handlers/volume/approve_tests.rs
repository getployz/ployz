use ployz_core::{
    DockerVolumeName, Hint, MachineId, MachineName, SlotId, SlotRole, Snapshot, SnapshotGuid,
    VolumeCopy, WriterEpoch,
};
use ployz_store::VolumeName;

use super::super::restore::{Seen, Unseen};
use super::{MirrorOn, consent, holders, keeps_mirror, parties, starting};

fn volume() -> VolumeName {
    VolumeName::parse("data").unwrap()
}

fn seen(name: &str, copy: Option<VolumeCopy>) -> Seen {
    Seen {
        id: MachineId::parse(name[name.len() - 1..].repeat(32)).unwrap(),
        name: name.to_owned(),
        copy: Ok(copy),
    }
}

fn root(readonly: bool) -> VolumeCopy {
    VolumeCopy::Root {
        readonly,
        newest: Some(Snapshot {
            name: "w-3-1".parse().unwrap(),
            guid: SnapshotGuid::new(41),
            created_unix_seconds: 1_700_000_000,
        }),
        epoch: WriterEpoch::new(3),
        seed: false,
        mark: None,
    }
}

fn slot(role: Option<SlotRole>) -> VolumeCopy {
    VolumeCopy::Slot {
        slot: SlotId::new(7),
        readonly: true,
        newest: None,
        resume_token: None,
        pull: None,
        role,
        pull_state: None,
    }
}

fn names(found: Result<(&Seen, &Seen), crate::handlers::Error>) -> (String, String) {
    let (writer, mirror) = found.unwrap();
    (writer.name.clone(), mirror.name.clone())
}

fn refused(found: Result<(&Seen, &Seen), crate::handlers::Error>) -> String {
    match found {
        Ok((writer, mirror)) => panic!(
            "expected a refusal, got {} and {}",
            writer.name, mirror.name
        ),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_mirror_names_the_one_writer_and_the_server_asked_for() {
    let reports = [
        seen("web-1", Some(root(false))),
        seen("web-2", None),
        seen("web-3", Some(slot(Some(SlotRole::OldCopy)))),
    ];
    let web2 = MachineName::parse("web-2").unwrap();
    assert_eq!(
        names(parties(&volume(), &reports, MirrorOn::Server(&web2))),
        ("web-1".to_owned(), "web-2".to_owned())
    );
}

#[test]
fn a_full_sync_names_the_current_mirror_and_never_an_old_copy() {
    let reports = [
        seen("web-1", Some(root(false))),
        seen("web-2", Some(slot(Some(SlotRole::OldCopy)))),
        seen("web-3", Some(slot(Some(SlotRole::Mirror)))),
    ];
    assert_eq!(
        names(parties(&volume(), &reports, MirrorOn::Current)),
        ("web-1".to_owned(), "web-3".to_owned())
    );
    let only_old = [
        seen("web-1", Some(root(false))),
        seen("web-2", Some(slot(Some(SlotRole::OldCopy)))),
    ];
    assert!(refused(parties(&volume(), &only_old, MirrorOn::Current)).contains("no Mirror"));
}

#[test]
fn approves_nothing_it_cannot_name_from_what_the_servers_report() {
    let web2 = MachineName::parse("web-2").unwrap();
    let web1 = MachineName::parse("web-1").unwrap();
    let on = MirrorOn::Server(&web2);
    let no_writer = [
        seen("web-1", Some(root(true))),
        Seen {
            copy: Err(Unseen::Silent("did not answer".to_owned())),
            ..seen("web-2", None)
        },
    ];
    assert!(refused(parties(&volume(), &no_writer, on)).contains("No Server reports a writer"));
    let two = [
        seen("web-1", Some(root(false))),
        seen("web-3", Some(root(false))),
    ];
    assert!(refused(parties(&volume(), &two, on)).contains("writers on web-1 and web-3"));
    let writer_only = [seen("web-1", Some(root(false)))];
    assert!(refused(parties(&volume(), &writer_only, on)).contains("No Server named web-2"));
    assert!(
        refused(parties(&volume(), &writer_only, MirrorOn::Server(&web1)))
            .contains("web-1 writes data")
    );
}

/// A spec mounting the Provisioned Volumes `sources`, each of 1 GB, running `image`.
fn spec(image: &str, sources: &[&str]) -> ployz_core::ResolvedServiceSpec {
    use std::collections::BTreeMap;
    use std::num::NonZeroU64;

    use ployz_core::{
        ContainerPath, Namespace, ProvisionedVolumeMaximumBytes, RawVolumeSource, ServiceMount,
        ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference,
    };

    let mut spec: ployz_core::ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": ployz_core::ServiceId::random(),
        "name": "db",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": image, "pull_policy": "missing" },
    }))
    .unwrap();
    let volumes: Vec<ServiceVolume> = sources
        .iter()
        .enumerate()
        .map(|(index, name)| ServiceVolume {
            reference: ServiceVolumeReference::parse(format!("v{index}")).unwrap(),
            source: RawVolumeSource::Provisioned {
                name: DockerVolumeName::parse(*name).unwrap(),
                maximum_bytes: ProvisionedVolumeMaximumBytes::new(
                    NonZeroU64::new(1_000_000_000).unwrap(),
                ),
                labels: BTreeMap::new(),
            }
            .admit()
            .unwrap(),
        })
        .collect();
    let mounts = volumes
        .iter()
        .enumerate()
        .map(|(index, volume)| ServiceMount {
            volume: volume.reference.clone(),
            target: ContainerPath::parse(format!("/data/{index}")).unwrap(),
            read_only: false,
            no_copy: false,
            subpath: None,
        })
        .collect();
    spec.set_volume_graph(
        ServiceVolumeGraph::parse(volumes, mounts)
            .unwrap()
            .scope_to_namespace(&Namespace::parse("app").unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    spec
}

fn sizes(spec: &ployz_core::ResolvedServiceSpec) -> Vec<(String, u64)> {
    spec.volumes()
        .iter()
        .filter_map(|volume| {
            if let ployz_core::RawVolumeSource::Provisioned {
                name,
                maximum_bytes,
                ..
            } = volume.source.kind()
            {
                Some((name.to_string(), maximum_bytes.get()))
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn a_move_starts_the_running_spec_from_the_copied_image_at_the_recorded_size() {
    let pinned = format!("postgres@sha256:{}", "a".repeat(64));
    let running = spec(&pinned, &["data", "logs"]);
    let docker = DockerVolumeName::parse("app_data").unwrap();
    let recorded = ployz_core::ProvisionedVolumeMaximumBytes::new(
        std::num::NonZeroU64::new(4_000_000_000).unwrap(),
    );

    let started = starting(running.clone(), &docker, Some(recorded)).unwrap();
    assert_eq!(started.container.pull_policy, ployz_core::PullPolicy::Never);
    assert_eq!(
        sizes(&started),
        vec![
            ("app_data".to_owned(), 4_000_000_000),
            ("app_logs".to_owned(), 1_000_000_000)
        ]
    );
    assert_eq!(started.container.image, pinned);

    let unrecorded = starting(running, &docker, None).unwrap();
    assert_eq!(
        sizes(&unrecorded),
        vec![
            ("app_data".to_owned(), 1_000_000_000),
            ("app_logs".to_owned(), 1_000_000_000)
        ]
    );
}

#[test]
fn a_move_of_a_service_whose_image_is_not_pinned_by_digest_is_refused() {
    let docker = DockerVolumeName::parse("app_data").unwrap();
    let error = starting(spec("postgres:17", &["data"]), &docker, None)
        .expect_err("a tag no Server starts under approval");
    assert!(
        error.to_string().contains("isn't pinned by digest"),
        "{error}"
    );
}

#[test]
fn a_move_names_the_mirrors_but_not_an_old_copy() {
    assert!(keeps_mirror(&seen(
        "web-3",
        Some(slot(Some(SlotRole::Mirror)))
    )));
    assert!(keeps_mirror(&seen("web-3", Some(slot(None)))));
    assert!(!keeps_mirror(&seen(
        "web-3",
        Some(slot(Some(SlotRole::OldCopy)))
    )));
    assert!(!keeps_mirror(&seen("web-1", Some(root(false)))));
    assert!(!keeps_mirror(&seen("web-4", None)));
}

fn container(
    id: char,
    machine: &str,
    kind: ployz_core::ContainerKind,
    runtime: ployz_core::ContainerRuntimeObservation,
    spec: ployz_core::ResolvedServiceSpec,
) -> ployz_core::ContainerObservation {
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        rotation: None,
        r#gen: None,
        container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: "db".into(),
        created_at_unix_nanos: 0,
        machine_id: seen(machine, None).id,
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        kind,
        runtime,
        effective_healthcheck: None,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
        stopped: false,
        run: None,
    })
    .unwrap()
}

#[test]
fn a_move_reads_only_the_running_service_container_and_never_one_an_earlier_cut_stopped() {
    use ployz_core::{ContainerKind, ContainerRuntimeObservation, HealthObservation};

    let running = || ContainerRuntimeObservation::Running {
        health: HealthObservation::NotConfigured,
    };
    let docker = DockerVolumeName::parse("app_data").unwrap();
    let containers = [
        container(
            'a',
            "web-1",
            ContainerKind::ServiceContainer,
            running(),
            spec("db@sha256:1", &["data"]),
        ),
        container(
            'b',
            "web-1",
            ContainerKind::ServiceContainer,
            ContainerRuntimeObservation::Exited { code: 0 },
            spec("db@sha256:0", &["data"]),
        ),
        container(
            'c',
            "web-2",
            ContainerKind::ServiceContainer,
            running(),
            spec("db@sha256:1", &["data"]),
        ),
        container(
            'd',
            "web-1",
            ContainerKind::ServiceContainer,
            running(),
            spec("db@sha256:1", &["other"]),
        ),
    ];
    let found = holders(&containers, &seen("web-1", None).id, &docker);
    assert_eq!(
        found
            .iter()
            .map(|container| container.container_id)
            .collect::<Vec<_>>(),
        [containers[0].container_id]
    );
}

/// `--yes` signs a Move nobody watches without asking.
#[test]
fn yes_signs_a_move_without_asking() {
    consent(&volume(), true, |question, _, _| {
        panic!("--yes asked {question:?}")
    })
    .unwrap();
}

/// Without a terminal or `--yes`, nothing is signed, and the refusal names the retry.
#[test]
fn a_move_with_no_terminal_refuses_before_signing_and_names_yes() {
    let error = consent(&volume(), false, |question, refusal, declined| {
        crate::ui::confirm(question, || refusal, declined)
    })
    .unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::ConfirmationRequired);
    assert!(rpc.message.contains("add --yes"), "{}", rpc.message);
    assert!(rpc.message.ends_with("Nothing moved."), "{}", rpc.message);
    assert!(
        error.hints().iter().any(|hint| matches!(
            hint,
            Hint::Retry(retry) if retry.ends_with(" --yes")
        )),
        "{:?}",
        error.hints()
    );
}

/// A person asked names the Volume, and their no fails the Move before it signs.
#[test]
fn declining_a_move_signs_nothing_and_fails() {
    let mut asked = None;
    let error = consent(&volume(), false, |question, _, declined| {
        asked = Some((question.to_owned(), declined));
        Err(crate::handlers::Error::cancelled())
    })
    .unwrap_err();
    assert_eq!(
        asked,
        Some((
            "Approve the move of data?".to_owned(),
            "Cancelled. Nothing signed or moved."
        ))
    );
    assert!(error.is_interrupted(), "a no exits 130, not 0");
}
