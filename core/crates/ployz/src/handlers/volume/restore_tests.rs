use ployz_core::{
    DockerVolumeName, ExpectedCopy, MachineId, MachineName, SlotId, SlotRole, Snapshot,
    SnapshotGuid, VolumeCopy, WantedCopy, WriterEpoch,
};
use ployz_store::VolumeName;

use super::{Planned, Seen, plan};

fn volume() -> VolumeName {
    VolumeName::parse("data").unwrap()
}

fn docker() -> DockerVolumeName {
    DockerVolumeName::parse("app-production_vol-1").unwrap()
}

fn server(name: &str) -> MachineName {
    MachineName::parse(name).unwrap()
}

fn seen(name: &str, copy: Option<VolumeCopy>) -> Seen {
    Seen {
        id: MachineId::parse(name[name.len() - 1..].repeat(32)).unwrap(),
        name: server(name),
        copy: Ok(copy),
    }
}

fn silent(name: &str) -> Seen {
    Seen {
        copy: Err("did not answer".to_owned()),
        ..seen(name, None)
    }
}

fn snapshot(name: &str, guid: u64, created: i64) -> Snapshot {
    Snapshot {
        name: name.parse().unwrap(),
        guid: SnapshotGuid::new(guid),
        created_unix_seconds: created,
    }
}

fn old_copy(newest: Option<Snapshot>) -> VolumeCopy {
    VolumeCopy::Slot {
        slot: SlotId::new(7),
        readonly: true,
        newest,
        resume_token: None,
        pull: None,
        role: Some(SlotRole::OldCopy),
    }
}

fn root(readonly: bool, epoch: u32) -> VolumeCopy {
    VolumeCopy::Root {
        readonly,
        newest: Some(snapshot("w-3-1", 41, 1_700_000_000)),
        epoch: WriterEpoch::new(epoch),
        seed: false,
    }
}

fn planned(seen: &[Seen], from: &str) -> Planned {
    plan(&volume(), &docker(), &server(from), seen).unwrap()
}

fn refused(seen: &[Seen], from: &str) -> String {
    match plan(&volume(), &docker(), &server(from), seen) {
        Ok(planned) => panic!("expected a refusal, planned {planned:?}"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn a_removed_servers_old_copy_becomes_the_writer_in_a_newer_epoch() {
    let newest = snapshot("w-4-2", 99, 1_790_000_000);
    let Planned::Restore {
        machine,
        request,
        lost_after,
    } = planned(
        &[
            seen("web-1", Some(old_copy(Some(newest.clone())))),
            seen("web-2", None),
        ],
        "web-1",
    )
    else {
        panic!("expected a restore");
    };

    assert_eq!(machine, seen("web-1", None).id);
    assert_eq!(request.name, docker());
    assert_eq!(
        request.expect,
        ExpectedCopy::Mirror {
            newest: SnapshotGuid::new(99),
            slot: SlotId::new(7),
            role: None,
        }
    );
    assert_eq!(
        request.want,
        WantedCopy::Writer {
            at: SnapshotGuid::new(99),
            epoch: WriterEpoch::new(5),
            start: None,
        }
    );
    assert_eq!(lost_after, Some(newest));
}

#[test]
fn a_read_only_root_keeps_its_epoch_and_loses_no_writes() {
    let Planned::Restore {
        request,
        lost_after,
        ..
    } = planned(&[seen("web-1", Some(root(true, 3)))], "web-1")
    else {
        panic!("expected a restore");
    };

    assert_eq!(
        request.expect,
        ExpectedCopy::Writer {
            epoch: WriterEpoch::new(3)
        }
    );
    assert_eq!(
        request.want,
        WantedCopy::Writer {
            at: SnapshotGuid::new(41),
            epoch: WriterEpoch::new(3),
            start: None,
        }
    );
    assert_eq!(lost_after, None);
}

#[test]
fn a_copy_that_is_already_the_writer_needs_nothing() {
    assert!(matches!(
        planned(
            &[seen("web-1", Some(root(false, 5))), seen("web-2", None)],
            "web-1"
        ),
        Planned::AlreadyWriter
    ));
}

#[test]
fn restore_refuses_while_a_server_is_silent() {
    assert_eq!(
        refused(
            &[
                seen("web-1", Some(old_copy(Some(snapshot("f-2", 9, 1))))),
                silent("web-2"),
            ],
            "web-1"
        ),
        "Restore needs every Server to answer, since a copy on one that didn't would become a second writer. web-2 did not answer"
    );
}

#[test]
fn restore_refuses_beside_another_copy() {
    assert_eq!(
        refused(
            &[
                seen("web-1", Some(old_copy(Some(snapshot("f-2", 9, 1))))),
                seen("web-2", Some(old_copy(Some(snapshot("f-2", 9, 1))))),
            ],
            "web-1"
        ),
        "data also has a copy on web-2; Restore needs web-1's copy to be the only one. Remove web-2 with ployz server rm first, or keep its copy and restore from it instead"
    );
}

#[test]
fn restore_refuses_a_server_with_no_copy_or_no_snapshot() {
    assert_eq!(
        refused(&[seen("web-1", None)], "web-1"),
        "web-1 holds no copy of data"
    );
    assert_eq!(
        refused(&[seen("web-1", Some(old_copy(None)))], "web-1"),
        "web-1's copy of data holds no snapshot to restore"
    );
}

#[test]
fn restore_refuses_a_server_outside_the_cluster() {
    assert_eq!(
        refused(&[seen("web-1", None)], "web-9"),
        "web-9 is not a Server of this Cluster"
    );
}
