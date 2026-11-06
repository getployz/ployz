use std::collections::BTreeSet;

use ployz_core::MachineId;

use super::Misses;

fn id(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

#[test]
fn two_misses_in_a_row_name_a_peer_and_one_answer_clears_it() {
    let (a, b) = (id('a'), id('b'));
    let probed = BTreeSet::from([a, b]);
    let mut misses = Misses::default();

    assert!(misses.round(&probed, &BTreeSet::from([b])).is_empty());
    assert_eq!(misses.round(&probed, &BTreeSet::from([b])), vec![a]);
    assert_eq!(misses.round(&probed, &BTreeSet::from([b])), vec![a]);
    assert!(misses.round(&probed, &probed).is_empty());
}

#[test]
fn an_answer_between_misses_starts_the_count_again() {
    let a = id('a');
    let probed = BTreeSet::from([a]);
    let mut misses = Misses::default();

    assert!(misses.round(&probed, &BTreeSet::new()).is_empty());
    assert!(misses.round(&probed, &probed).is_empty());
    assert!(misses.round(&probed, &BTreeSet::new()).is_empty());
}

#[test]
fn a_peer_no_longer_probed_is_forgotten() {
    let a = id('a');
    let probed = BTreeSet::from([a]);
    let mut misses = Misses::default();

    misses.round(&probed, &BTreeSet::new());
    assert_eq!(misses.round(&probed, &BTreeSet::new()), vec![a]);
    assert!(misses.round(&BTreeSet::new(), &BTreeSet::new()).is_empty());
    assert!(misses.round(&probed, &BTreeSet::new()).is_empty());
}
