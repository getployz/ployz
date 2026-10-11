use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::{
    ConfirmBudget, Liveness, SeenBatches, drop_after, next_slot, ranked, region_for, slot_index,
};
use crate::domain::{CheckId, Job, OrgId, ProbeKey, ProbeKind, Region, Target, UnixMs};

const PERIOD: Duration = Duration::from_secs(60);

fn id(n: u128) -> CheckId {
    CheckId(Uuid::from_u128(n))
}

fn regions(names: &[&str]) -> Vec<Region> {
    names.iter().map(|n| Region::parse(n).unwrap()).collect()
}

fn job(check: CheckId) -> Job {
    Job {
        key: ProbeKey {
            check,
            region: Region::parse("fra").unwrap(),
            scheduled_at: UnixMs(0),
            kind: ProbeKind::Confirm,
        },
        target: Arc::new(Target::parse("https://example.com/").unwrap()),
        timeout: Duration::from_secs(1),
        address: 0,
    }
}

#[test]
fn next_slot_is_strictly_after_and_one_period_apart() {
    for seed in [0u128, 1, 7 << 64, u128::MAX] {
        let check = id(seed);
        let after = UnixMs(1_700_000_000_000);
        let first = next_slot(check, PERIOD, after);
        assert!(first > after, "slot {} not after {}", first.0, after.0);
        assert!(first.0 - after.0 <= 60_000);
        assert_eq!(next_slot(check, PERIOD, first).0, first.0 + 60_000);
        assert_eq!(
            slot_index(check, PERIOD, first) + 1,
            slot_index(check, PERIOD, next_slot(check, PERIOD, first))
        );
    }
}

#[test]
fn checks_with_different_ids_fire_at_different_phases() {
    let after = UnixMs(0);
    let a = next_slot(id(1 << 64), PERIOD, after);
    let b = next_slot(id(2 << 64), PERIOD, after);
    assert_ne!(a, b);
}

#[test]
fn ranked_rotates_the_configured_regions_by_id() {
    let all = regions(&["ams", "fra", "sjc"]);
    let a = ranked(id(0), &all);
    let b = ranked(id(1), &all);
    let c = ranked(id(2), &all);
    assert_eq!(a, regions(&["ams", "fra", "sjc"]));
    assert_eq!(b, regions(&["fra", "sjc", "ams"]));
    assert_eq!(c, regions(&["sjc", "ams", "fra"]));
    assert!(ranked(id(0), &[]).is_empty());
}

#[test]
fn region_for_walks_the_live_list_round_robin() {
    let live = regions(&["ams", "fra"]);
    assert_eq!(region_for(0, &live), Some(Region::parse("ams").unwrap()));
    assert_eq!(region_for(1, &live), Some(Region::parse("fra").unwrap()));
    assert_eq!(region_for(2, &live), Some(Region::parse("ams").unwrap()));
    assert_eq!(region_for(5, &[]), None);
}

#[test]
fn routine_jobs_are_dropped_half_a_period_after_their_slot() {
    assert_eq!(drop_after(UnixMs(1_000), PERIOD), UnixMs(31_000));
}

#[test]
fn budget_floor_hands_out_four_per_tick_for_a_tiny_fleet() {
    let mut budget = ConfirmBudget::new();
    budget.set_load(0.001);
    let org = OrgId(Uuid::from_u128(1));
    budget.push(org, (0..10).map(|_| job(id(1))).collect());
    assert_eq!(budget.tick().len(), 4);
    assert_eq!(budget.tick().len(), 4);
    assert_eq!(budget.tick().len(), 2);
    assert!(budget.tick().is_empty());
}

#[test]
fn budget_credit_is_capped_so_an_idle_fleet_cannot_burst_past_the_cap() {
    let mut budget = ConfirmBudget::new();
    budget.set_load(0.001);
    for _ in 0..1000 {
        assert!(budget.tick().is_empty());
    }
    let org = OrgId(Uuid::from_u128(1));
    budget.push(org, (0..100).map(|_| job(id(1))).collect());
    assert_eq!(budget.tick().len(), 32);
}

#[test]
fn budget_alternates_between_organizations() {
    let mut budget = ConfirmBudget::new();
    budget.set_load(0.001);
    let a = OrgId(Uuid::from_u128(1));
    let b = OrgId(Uuid::from_u128(2));
    budget.push(a, (0..10).map(|_| job(id(1))).collect());
    budget.push(b, (0..10).map(|_| job(id(2))).collect());
    let out = budget.tick();
    let from_a = out.iter().filter(|j| j.key.check == id(1)).count();
    let from_b = out.iter().filter(|j| j.key.check == id(2)).count();
    assert_eq!((from_a, from_b), (2, 2));
}

#[test]
fn budget_forgets_a_removed_check_and_keeps_the_others() {
    let mut budget = ConfirmBudget::new();
    let org = OrgId(Uuid::from_u128(1));
    budget.push(org, vec![job(id(1)), job(id(2))]);
    budget.forget(id(1));
    let out = budget.tick();
    assert_eq!(out.len(), 1);
    assert_eq!(out.first().map(|j| j.key.check), Some(id(2)));
}

#[test]
fn seen_batches_refuse_a_repeat_until_the_window_passes() {
    let start = Instant::now();
    let mut seen = SeenBatches::new(start);
    let batch = Uuid::now_v7();
    assert!(seen.insert(batch, start));
    assert!(!seen.insert(batch, start + Duration::from_secs(1)));
    seen.expire(start + Duration::from_secs(29 * 60));
    assert!(!seen.insert(batch, start + Duration::from_secs(29 * 60)));
    seen.expire(start + Duration::from_secs(31 * 60));
    assert!(seen.insert(batch, start + Duration::from_secs(31 * 60)));
}

#[test]
fn seen_batches_expire_only_every_ten_seconds() {
    let start = Instant::now();
    let mut seen = SeenBatches::new(start);
    let batch = Uuid::now_v7();
    assert!(seen.insert(batch, start));
    seen.expire(start + Duration::from_secs(9));
    seen.expire(start + Duration::from_secs(31 * 60 + 5));
    assert!(seen.insert(batch, start + Duration::from_secs(31 * 60 + 5)));
}

#[test]
fn a_region_goes_dead_thirty_seconds_after_its_last_pull() {
    let start = Instant::now();
    let mut live = Liveness::new();
    let fra = Region::parse("fra").unwrap();
    assert!(!live.is_live(&fra, start));
    live.mark(&fra, start);
    assert!(live.is_live(&fra, start + Duration::from_secs(29)));
    assert!(!live.is_live(&fra, start + Duration::from_secs(31)));
}

#[test]
fn live_filters_to_pulling_regions_and_falls_back_to_all() {
    let start = Instant::now();
    let mut live = Liveness::new();
    let all = regions(&["ams", "fra", "sjc"]);
    assert_eq!(live.live(&all, start), all);
    live.mark(&Region::parse("fra").unwrap(), start);
    assert_eq!(live.live(&all, start), regions(&["fra"]));
}
