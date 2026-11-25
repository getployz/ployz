//! The names and the transition lists of `marks`, without a fake ZFS: the strings on disk
//! and the order each list runs in.

use crate::volume_plugin::marks::{Mark, Marks, Write};

fn inherit(mark: Mark) -> Write {
    Write::Inherit(mark)
}

fn set(mark: Mark, value: &str) -> Write {
    Write::Set(mark, value.to_owned())
}

#[test]
fn every_mark_keeps_its_name_on_disk() {
    let names = [
        (Mark::Promote, "ployz:promote"),
        (Mark::Cut, "ployz:cut"),
        (Mark::Epoch, "ployz:epoch"),
        (Mark::Seed, "ployz:seed"),
        (Mark::Pulled, "ployz:pulled"),
        (Mark::Pull, "ployz:pull"),
        (Mark::Role, "ployz:role"),
        (Mark::ResumeToken, "receive_resume_token"),
    ];
    for (mark, name) in names {
        assert_eq!(mark.name(), name);
        assert_eq!(mark.to_string(), name);
    }
}

#[test]
fn provenance_goes_in_the_order_cut_epoch_promote_seed() {
    assert_eq!(
        Marks::clear_provenance(),
        [
            inherit(Mark::Cut),
            inherit(Mark::Epoch),
            inherit(Mark::Promote),
            inherit(Mark::Seed),
        ]
    );
}

#[test]
fn a_pull_records_the_newest_snapshot_after_every_mark_it_clears() {
    assert_eq!(
        Marks::received("1234"),
        [
            inherit(Mark::Cut),
            inherit(Mark::Epoch),
            inherit(Mark::Promote),
            inherit(Mark::Seed),
            set(Mark::Pulled, "1234"),
        ]
    );
}

#[test]
fn a_landing_clears_pulled_then_sets_seed_then_promote() {
    assert_eq!(
        Marks::landed("99"),
        [
            inherit(Mark::Pulled),
            set(Mark::Seed, "99"),
            set(Mark::Promote, "on"),
        ]
    );
}
