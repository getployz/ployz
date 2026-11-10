use ployz_core::{
    CopyTransition, Drain, MachineId, ManagementAddress, SlotId, SnapshotGuid, VolumeCopy,
    WantedCopy, WriterEpoch,
};

use super::{approved_step, withdrawn};
use crate::machine::CopyStep;

fn id(c: char) -> MachineId {
    MachineId::parse(c.to_string().repeat(32)).unwrap()
}

fn pull(from: Option<ManagementAddress>) -> WantedCopy {
    WantedCopy::Mirror {
        from,
        target: "w-1-1".parse().unwrap(),
        drain: Drain::NONE,
    }
}

fn root() -> VolumeCopy {
    VolumeCopy::Root {
        readonly: false,
        newest: None,
        epoch: WriterEpoch::new(1),
        seed: false,
        mark: None,
    }
}

fn slot() -> VolumeCopy {
    VolumeCopy::Slot {
        slot: SlotId::new(1),
        readonly: true,
        newest: None,
        resume_token: None,
        pull: None,
        role: None,
        pull_state: None,
    }
}

#[test]
fn checks_a_pull_that_moves_data_and_every_run_snapshot() {
    let source = ManagementAddress("fd00::2".parse().unwrap());
    let want = pull(Some(source));
    for transition in [CopyTransition::NoneToMirror, CopyTransition::MirrorToMirror] {
        assert!(matches!(
            approved_step(&want, Some(transition)),
            Some(CopyStep::Pull { transition: checked, from }) if checked == transition && *from == source
        ));
    }
    assert!(approved_step(&want, None).is_none());
    assert!(approved_step(&want, Some(CopyTransition::WriterToMirror)).is_none());
    assert!(approved_step(&pull(None), Some(CopyTransition::NoneToMirror)).is_none());

    let snapshot = WantedCopy::Snapshot {
        name: "w-1-1".parse().unwrap(),
    };
    for transition in [None, Some(CopyTransition::WriterToSnapshot)] {
        assert!(matches!(
            approved_step(&snapshot, transition),
            Some(CopyStep::Snapshot)
        ));
    }
    let writer = WantedCopy::Writer {
        at: SnapshotGuid::new(1),
        epoch: WriterEpoch::new(2),
        start: None,
    };
    assert!(approved_step(&writer, Some(CopyTransition::WriterToWriter)).is_none());
    assert!(approved_step(&WantedCopy::Forget { recipient: None }, None).is_none());
}

#[test]
fn a_forget_withdraws_the_mirror_itself_or_the_recipient_the_writer_names() {
    let this = id('a');
    let recipient = Some(id('b'));
    assert_eq!(
        withdrawn(
            Some(&slot()),
            None,
            Some(CopyTransition::MirrorToForget),
            &this
        ),
        Some(this)
    );
    assert_eq!(withdrawn(Some(&slot()), recipient, None, &this), Some(this));
    assert_eq!(withdrawn(None, None, None, &this), Some(this));
    assert_eq!(
        withdrawn(
            Some(&root()),
            recipient,
            Some(CopyTransition::WriterToForget),
            &this
        ),
        recipient
    );
    assert_eq!(withdrawn(Some(&root()), recipient, None, &this), recipient);
    assert_eq!(
        withdrawn(
            Some(&root()),
            None,
            Some(CopyTransition::WriterToForget),
            &this
        ),
        None
    );
    assert_eq!(
        withdrawn(
            Some(&root()),
            recipient,
            Some(CopyTransition::SeedToForget),
            &this
        ),
        None
    );
}
