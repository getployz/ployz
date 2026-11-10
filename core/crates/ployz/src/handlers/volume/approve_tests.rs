use ployz_core::{
    MachineId, MachineName, SlotId, SlotRole, Snapshot, SnapshotGuid, VolumeCopy, WriterEpoch,
};
use ployz_store::VolumeName;

use super::super::restore::{Seen, Unseen};
use super::{MirrorOn, parties};

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
    assert!(refused(parties(&volume(), &only_old, MirrorOn::Current)).contains("no mirror"));
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
