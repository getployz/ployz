use ployz_core::{MachineId, SlotId, SlotRole, VolumeCopy, WriterEpoch};
use ployz_store::VolumeName;

use super::super::restore::{Seen, Unseen};
use super::{consent, holders};

fn volume() -> VolumeName {
    VolumeName::parse("data").unwrap()
}

fn id(fill: char) -> MachineId {
    MachineId::parse(fill.to_string().repeat(32)).unwrap()
}

fn seen(fill: char, copy: Result<Option<VolumeCopy>, Unseen>) -> Seen {
    Seen {
        id: id(fill),
        name: format!("web-{fill}"),
        copy,
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

fn slot(role: SlotRole) -> VolumeCopy {
    VolumeCopy::Slot {
        slot: SlotId::new(7),
        readonly: true,
        newest: None,
        resume_token: None,
        pull: None,
        role: Some(role),
        pull_state: None,
    }
}

/// Cloud's run sets every copy, Writer and slot alike, so the approval names each Server
/// that reports one, sorted, and none that reports nothing or didn't answer.
#[test]
fn the_approval_names_every_server_holding_a_copy() {
    let seen = [
        seen('c', Ok(Some(slot(SlotRole::OldCopy)))),
        seen('a', Ok(Some(root()))),
        seen('d', Ok(None)),
        seen('e', Err(Unseen::Silent("down".to_owned()))),
        seen('b', Ok(Some(slot(SlotRole::Mirror)))),
    ];
    assert_eq!(holders(&seen), vec![id('a'), id('b'), id('c')]);
}

#[test]
fn yes_signs_without_asking() {
    consent(&volume(), true, |question, _, _| {
        panic!("--yes asked {question:?}")
    })
    .unwrap();
}

/// Without a terminal, and without `--yes`, nothing is signed: the refusal names the
/// `--yes` retry.
#[test]
fn no_terminal_refuses_before_signing_and_names_yes() {
    let error = consent(&volume(), false, |question, refusal, declined| {
        crate::ui::confirm(question, || refusal, declined)
    })
    .unwrap_err();
    let rpc = error.report();
    assert_eq!(rpc.code, ployz_core::RpcErrorCode::ConfirmationRequired);
    assert!(rpc.message.contains("add --yes"), "{}", rpc.message);
    assert!(rpc.message.ends_with("Nothing changed."), "{}", rpc.message);
    assert!(
        error.hints().iter().any(|hint| matches!(
            hint,
            crate::ui::Hint::Retry(retry) if retry.ends_with(" --yes")
        )),
        "{:?}",
        error.hints()
    );
}

#[test]
fn declining_signs_nothing_and_fails() {
    let mut asked = None;
    let error = consent(&volume(), false, |question, _, declined| {
        asked = Some((question.to_owned(), declined));
        Err(crate::handlers::Error::cancelled())
    })
    .unwrap_err();
    assert_eq!(
        asked,
        Some((
            "Approve resizing data?".to_owned(),
            "Cancelled. Nothing signed or resized."
        ))
    );
    assert!(error.is_interrupted(), "a no exits 130, not 0");
}
