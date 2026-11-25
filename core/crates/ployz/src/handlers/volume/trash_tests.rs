use ployz_core::{
    DockerVolumeName, MachineId, MachineObservation, MembershipObservation, RpcErrorCode,
    RuntimeWatchFrame, SlotId, SlotRole, VolumeCopy,
};

use super::{emptying, no_copy, retry_args};
use crate::global_fixtures::machine;
use crate::handlers::volume::restore::{Seen, Unseen};

fn name() -> DockerVolumeName {
    DockerVolumeName::parse("app-production_vol-1").unwrap()
}

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

/// `web-N` answers as Machine `N`, as [`frame`] lists it.
fn seen(name: &str, copy: Result<Option<VolumeCopy>, Unseen>) -> Seen {
    Seen {
        id: id(name.chars().last().unwrap()),
        name: name.to_owned(),
        copy,
    }
}

/// web-1 and web-2.
fn frame() -> RuntimeWatchFrame {
    let machines = [('1', "web-1"), ('2', "web-2")]
        .into_iter()
        .map(|(hex, name)| MachineObservation::new(machine(hex, name), MembershipObservation::Up))
        .collect();
    RuntimeWatchFrame {
        machines,
        containers: Vec::new(),
        volumes: Vec::new(),
        certificates: Vec::new(),
        incomplete_ids: Default::default(),
        unanswered: Vec::new(),
        answered: None,
        observed_at: String::new(),
    }
}

fn old_copy() -> VolumeCopy {
    VolumeCopy::Slot {
        slot: SlotId::new(7),
        readonly: true,
        newest: None,
        resume_token: None,
        pull: None,
        role: Some(SlotRole::Mirror),
        pull_state: None,
        origin: None,
    }
}

#[test]
fn a_deleted_volume_comes_back_only_when_every_server_of_the_frame_answers_with_no_copy() {
    let quiet = [seen("web-1", Ok(None)), seen("web-2", Ok(None))];
    no_copy(&name(), id('1'), &quiet, &frame()).unwrap();

    let refused = |here, seen: &[Seen]| {
        no_copy(&name(), here, seen, &frame())
            .unwrap_err()
            .report()
            .code
    };
    let with = |other| [seen("web-1", Ok(None)), other];
    let silent = seen("web-2", Err(Unseen::Silent("did not answer".into())));
    assert_eq!(refused(id('1'), &with(silent)), RpcErrorCode::Unavailable);
    let held = seen("web-2", Ok(Some(old_copy())));
    assert_eq!(refused(id('1'), &with(held)), RpcErrorCode::Conflict);
    // A member with no answer, or a frame that doesn't list this Server, leaves a copy unknown.
    assert_eq!(
        refused(id('1'), &[seen("web-1", Ok(None))]),
        RpcErrorCode::Unavailable
    );
    assert_eq!(refused(id('3'), &quiet), RpcErrorCode::Unavailable);
}

/// The retry a refused `trash empty` prints names the entries it would delete, and still
/// keeps any a restore pins before the retry runs.
#[test]
fn the_retry_of_a_refused_empty_still_keeps_entries_being_restored() {
    let ids = [
        ployz_core::TrashId::parse("aeaqcaibaeaqcaibaeaqcaibae").unwrap(),
        ployz_core::TrashId::parse("aiaqcaibaeaqcaibaeaqcaibae").unwrap(),
    ];
    let argv = ["ployz", "volume", "trash", "empty"]
        .map(str::to_owned)
        .into_iter()
        .chain(retry_args(&ids));
    let root = crate::cli::command().try_get_matches_from(argv).unwrap();

    let (named, bulk) = emptying(&root);

    assert_eq!(named, ids);
    assert!(
        bulk,
        "a retry that names entries must still keep pinned ones"
    );
}
