use ployz_core::{
    DockerVolumeName, MachineId, MachineObservation, MembershipObservation, PoolBy, PoolRead,
    RpcErrorCode, RuntimeWatchFrame, SlotId, SlotRole, TrashId, VolumeCopy,
};

use super::{empty_request, empty_retry, named, proven};
use crate::global_fixtures::machine;
use crate::handlers::volume::restore::{Seen, Unseen};

fn name() -> DockerVolumeName {
    DockerVolumeName::parse("app-production_vol-1").unwrap()
}

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

/// The entry being restored on web-1.
fn entry() -> TrashId {
    TrashId::parse("aeaqcaibaeaqcaibaeaqcaibae").unwrap()
}

/// `web-N` answers as Machine `N`, as [`frame`] lists it, from its healthy file Pool with
/// nothing pinned.
fn seen(name: &str, copy: Result<Option<VolumeCopy>, Unseen>) -> Seen {
    Seen {
        id: id(name.chars().last().unwrap()),
        name: name.to_owned(),
        copy,
        read: Some(PoolRead::Pool {
            name: "ployz".to_owned(),
            health: "ONLINE".to_owned(),
            by: PoolBy::File,
        }),
        pinned: Some(Vec::new()),
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
        written_here: false,
    }
}

fn refused(seen: &[Seen]) -> RpcErrorCode {
    proven(&name(), entry(), id('1'), &frame(), seen)
        .unwrap_err()
        .report()
        .code
}

/// web-1 pinned the entry; web-2 answers `other`.
fn with(other: Seen) -> [Seen; 2] {
    let here = Seen {
        pinned: Some(vec![entry()]),
        ..seen("web-1", Ok(None))
    };
    [here, other]
}

#[test]
fn a_deleted_volume_comes_back_only_when_every_server_of_the_frame_answers_with_no_copy() {
    proven(
        &name(),
        entry(),
        id('1'),
        &frame(),
        &with(seen("web-2", Ok(None))),
    )
    .unwrap();

    let silent = seen("web-2", Err(Unseen::Silent("did not answer".into())));
    assert_eq!(refused(&with(silent)), RpcErrorCode::Unavailable);
    let held = seen("web-2", Ok(Some(old_copy())));
    assert_eq!(refused(&with(held)), RpcErrorCode::Conflict);
    // A member with no answer, or a frame that doesn't list this Server, leaves a copy unknown.
    let [here, _] = with(seen("web-2", Ok(None)));
    assert_eq!(refused(&[here]), RpcErrorCode::Unavailable);
    let quiet = with(seen("web-2", Ok(None)));
    assert_eq!(
        proven(&name(), entry(), id('3'), &frame(), &quiet)
            .unwrap_err()
            .report()
            .code,
        RpcErrorCode::Unavailable
    );
}

/// Two restores of one Volume on two Servers each pin first, so each one's proof sees the
/// other's pin, and neither becomes a second writer.
#[test]
fn a_restore_refuses_while_another_server_restores_the_same_volume() {
    let other = TrashId::parse("aiaqcaibaeaqcaibaeaqcaibae").unwrap();
    let restoring = Seen {
        pinned: Some(vec![other]),
        ..seen("web-2", Ok(None))
    };
    assert_eq!(refused(&with(restoring)), RpcErrorCode::Conflict);
    // A second pinned entry here is another restore too.
    let [mut here, quiet] = with(seen("web-2", Ok(None)));
    here.pinned = Some(vec![entry(), other]);
    assert_eq!(refused(&[here, quiet]), RpcErrorCode::Conflict);
}

/// "No copy" counts only from a Server that read the Pool it knows is its own, healthy, and
/// listed what a restore pinned there. An older Ployz answers without either.
#[test]
fn a_restore_refuses_an_answer_that_proves_nothing() {
    let older = Seen {
        read: None,
        pinned: None,
        ..seen("web-2", Ok(None))
    };
    assert_eq!(refused(&with(older)), RpcErrorCode::Unavailable);
    let unknown = Seen {
        read: Some(PoolRead::Unknown),
        ..seen("web-2", Ok(None))
    };
    assert_eq!(refused(&with(unknown)), RpcErrorCode::Unavailable);
    let no_pins = Seen {
        pinned: None,
        ..seen("web-2", Ok(None))
    };
    assert_eq!(refused(&with(no_pins)), RpcErrorCode::Unavailable);
    let unproven = Seen {
        read: Some(PoolRead::Unproven {
            why: "it has Pools attached that it doesn't record as its own".to_owned(),
        }),
        ..seen("web-2", Ok(None))
    };
    assert_eq!(refused(&with(unproven)), RpcErrorCode::Unavailable);
    let degraded = Seen {
        read: Some(PoolRead::Pool {
            name: "ployz-device".to_owned(),
            health: "DEGRADED".to_owned(),
            by: PoolBy::Device,
        }),
        ..seen("web-2", Ok(None))
    };
    assert_eq!(refused(&with(degraded)), RpcErrorCode::Unavailable);
    let unheld = Seen {
        read: Some(PoolRead::Unheld),
        ..seen("web-2", Ok(None))
    };
    proven(&name(), entry(), id('1'), &frame(), &with(unheld)).unwrap();
}

/// The Server that took the pin must still show it, alone, in the Pool it proves from. An
/// answer from no Pool, or one that lists no pin, comes from somewhere other than where the
/// promote lands: a Pool swapped out and back between pin, proof and promote.
#[test]
fn a_restore_refuses_when_its_own_server_no_longer_shows_the_pin() {
    let quiet = || seen("web-2", Ok(None));
    let unheld = Seen {
        read: Some(PoolRead::Unheld),
        pinned: Some(vec![entry()]),
        ..seen("web-1", Ok(None))
    };
    assert_eq!(refused(&[unheld, quiet()]), RpcErrorCode::Unavailable);
    let unpinned = seen("web-1", Ok(None));
    assert_eq!(refused(&[unpinned, quiet()]), RpcErrorCode::Unavailable);
}

/// The retry a refused `trash empty` prints names the entries it would delete, and still
/// keeps any a restore pins before the retry runs, however the refused command was typed.
#[test]
fn the_retry_of_a_refused_empty_still_keeps_entries_being_restored() {
    let ids = [
        ployz_core::TrashId::parse("aeaqcaibaeaqcaibaeaqcaibae").unwrap(),
        ployz_core::TrashId::parse("aiaqcaibaeaqcaibaeaqcaibae").unwrap(),
    ];
    let parse = |argv: Vec<String>| crate::cli::command().try_get_matches_from(argv).unwrap();
    for typed in [
        "ployz volume trash empty",
        "ployz volume trash empty --keep-restoring",
        "ployz volume trash empty --",
        "ployz --connect tcp://127.0.0.1:7000 volume trash empty",
    ] {
        let refused = parse(shell_words::split(typed).unwrap());
        let retry = parse(shell_words::split(&empty_retry(&refused, &ids).line()).unwrap());

        assert_eq!(
            empty_request(&retry, named(&retry)),
            ployz_core::EmptyTrashRequest {
                ids: ids.to_vec(),
                bulk: true,
                approval: None,
            },
            "the retry of `{typed}` must still keep pinned entries"
        );
        assert_eq!(
            crate::cli::connection(crate::cli::leaf_matches(&retry)),
            crate::cli::connection(crate::cli::leaf_matches(&refused)),
        );
    }
}
