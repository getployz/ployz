use ployz_core::authority::{
    ApprovedVolumeChange, Signer, SignerKey, Timestamp, VolumeApproval, VolumeWorkflow,
};
use ployz_core::{
    DockerVolumeName, MachineId, MachineName, Namespace, OperationId, RpcErrorCode,
    SealingPublicKey, SlotId, SlotRole, Snapshot, SnapshotGuid, VolumeCopy, WriterEpoch,
};
use ployz_store::VolumeName;

use super::super::approve::named;
use super::super::restore::{Seen, plan};
use super::{local_time, removal_workflow, restore_workflow};

fn volume() -> VolumeName {
    VolumeName::parse("data").unwrap()
}

fn server(name: &str) -> MachineName {
    MachineName::parse(name).unwrap()
}

fn seen(name: &str, copy: Option<VolumeCopy>) -> Seen {
    Seen {
        id: MachineId::parse(name[name.len() - 1..].repeat(32)).unwrap(),
        name: name.to_owned(),
        copy: Ok(copy),
    }
}

fn snapshot(guid: u64, created: i64) -> Snapshot {
    Snapshot {
        name: "w-4-2".parse().unwrap(),
        guid: SnapshotGuid::new(guid),
        created_unix_seconds: created,
    }
}

fn slot(role: SlotRole, newest: Option<Snapshot>) -> VolumeCopy {
    VolumeCopy::Slot {
        slot: SlotId::new(7),
        readonly: true,
        newest,
        resume_token: None,
        pull: None,
        role: Some(role),
        pull_state: None,
    }
}

fn writer() -> VolumeCopy {
    VolumeCopy::Root {
        readonly: false,
        newest: Some(snapshot(41, 1_700_000_000)),
        epoch: WriterEpoch::new(3),
        seed: false,
        mark: None,
    }
}

fn body(workflow: VolumeWorkflow) -> VolumeApproval {
    let signer = Signer::from_seed(&[7; 32]);
    let issued_at = Timestamp::from_millis(1_800_000_000_000).unwrap();
    VolumeApproval {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        operation: OperationId::random(),
        namespace: Namespace::parse("shop").unwrap(),
        volume: DockerVolumeName::parse("shop_vol-1").unwrap(),
        workflow,
        issued_at,
        expires_at: issued_at.plus(60_000),
        signer: SignerKey::Device(signer.public().into()),
    }
}

fn restored(seen: &[Seen], from: &str) -> VolumeWorkflow {
    let docker = DockerVolumeName::parse("shop_vol-1").unwrap();
    let planned = plan(&volume(), &docker, &server(from), seen).unwrap();
    restore_workflow(&volume(), &server(from), seen, planned)
        .unwrap()
        .expect("a Restore to sign")
}

#[test]
fn a_restore_signs_the_snapshot_it_restores_and_shows_the_time_it_signs() {
    let newest = snapshot(99, 1_790_000_000);
    let seen = [seen(
        "web-2",
        Some(slot(SlotRole::Mirror, Some(newest.clone()))),
    )];
    let workflow = restored(&seen, "web-2");
    let VolumeWorkflow::Restore {
        machine,
        at,
        loses_after,
    } = workflow.clone()
    else {
        panic!("not a Restore: {workflow:?}");
    };
    assert_eq!(machine, seen[0].id);
    assert_eq!(at, newest.guid);
    assert_eq!(loses_after.as_millis(), 1_790_000_000_000);
    let body = body(workflow);
    let change = ApprovedVolumeChange::restoring(&body, "data", named(&seen), local_time).unwrap();
    let line = change.line();
    assert!(line.contains(&local_time(loses_after)), "{line}");
    assert!(line.contains("web-2"), "{line}");
}

#[test]
fn a_restore_of_a_writer_signs_nothing() {
    let seen = [seen("web-1", Some(writer()))];
    let docker = DockerVolumeName::parse("shop_vol-1").unwrap();
    let planned = plan(&volume(), &docker, &server("web-1"), &seen).unwrap();
    assert!(
        restore_workflow(&volume(), &server("web-1"), &seen, planned)
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_removal_names_the_writer_that_keeps_the_volume() {
    let seen = [
        seen("web-1", Some(writer())),
        seen("web-2", Some(slot(SlotRole::Mirror, Some(snapshot(41, 1))))),
    ];
    let workflow = removal_workflow(&volume(), &server("web-2"), &seen, (None, false))
        .unwrap()
        .unwrap();
    assert_eq!(
        workflow,
        VolumeWorkflow::MirrorRemoval {
            mirror: seen[1].id,
            writer: Some(seen[0].id),
            role: SlotRole::Mirror,
        }
    );
    let change = ApprovedVolumeChange::of(&body(workflow), "data", named(&seen)).unwrap();
    assert!(
        change.line().contains("web-1 keeps writing data"),
        "{}",
        change.line()
    );
}

#[test]
fn a_last_copy_signs_its_loss_only_after_the_typed_name() {
    let seen = [seen(
        "web-2",
        Some(slot(SlotRole::OldCopy, Some(snapshot(41, 1)))),
    )];
    let refused = removal_workflow(&volume(), &server("web-2"), &seen, (None, true)).unwrap_err();
    assert_eq!(refused.report().code, RpcErrorCode::ConfirmationRequired);
    let wrong = removal_workflow(&volume(), &server("web-2"), &seen, (Some("other"), true));
    assert!(wrong.is_err());
    let workflow = removal_workflow(&volume(), &server("web-2"), &seen, (Some("data"), true))
        .unwrap()
        .unwrap();
    assert_eq!(
        workflow,
        VolumeWorkflow::MirrorRemoval {
            mirror: seen[0].id,
            writer: None,
            role: SlotRole::OldCopy,
        }
    );
    let change = ApprovedVolumeChange::of(&body(workflow), "data", named(&seen)).unwrap();
    assert!(
        change.line().contains("may be its last copy"),
        "{}",
        change.line()
    );
}

#[test]
fn a_removal_signs_nothing_for_an_empty_slot_and_refuses_a_changed_role() {
    let empty = [
        seen("web-1", Some(writer())),
        seen("web-2", Some(slot(SlotRole::Mirror, None))),
    ];
    assert!(
        removal_workflow(&volume(), &server("web-2"), &empty, (None, false))
            .unwrap()
            .is_none()
    );
    let live = [
        seen("web-1", Some(writer())),
        seen("web-2", Some(slot(SlotRole::Mirror, Some(snapshot(41, 1))))),
    ];
    let refused = removal_workflow(&volume(), &server("web-2"), &live, (None, true)).unwrap_err();
    assert!(
        refused.to_string().contains("no longer an old copy"),
        "{refused}"
    );
}

#[test]
fn a_removal_beside_a_server_that_did_not_answer_signs_nothing() {
    let silent = Seen {
        copy: Err(super::super::restore::Unseen::Silent(
            "timed out".to_owned(),
        )),
        ..seen("web-1", None)
    };
    let seen = [
        silent,
        seen("web-2", Some(slot(SlotRole::Mirror, Some(snapshot(41, 1))))),
    ];
    let refused =
        removal_workflow(&volume(), &server("web-2"), &seen, (Some("data"), false)).unwrap_err();
    assert_eq!(refused.report().code, RpcErrorCode::Unavailable);
    assert!(
        refused.to_string().contains("web-1 did not answer"),
        "{refused}"
    );
}
