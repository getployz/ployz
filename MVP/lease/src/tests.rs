use super::*;
use harness::visible_nodes;

fn resource() -> LeaseResource {
    LeaseResource::from_segments(["acme", "http01", "example.com", "token-a"])
}

fn holder(value: &str) -> LeaseHolder {
    LeaseHolder::new(value)
}

fn at(value: u64) -> LeaseTimestamp {
    LeaseTimestamp::from_secs(value)
}

fn policy() -> LeaseAcquirePolicy {
    LeaseAcquirePolicy::new(LeaseDuration::from_secs(10).expect("ttl"))
}

fn context() -> LeaseCommandContext {
    LeaseCommandContext::new(visible_nodes(["node-a", "node-b"]))
}

fn local_context() -> LeaseCommandContext {
    LeaseCommandContext::new(VisibleNodes::new(Vec::new()))
}

fn second_epoch() -> LeaseEpoch {
    LeaseEpoch::from_u64(2).expect("second epoch")
}

fn claim(
    holder: LeaseHolder,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
) -> LeaseClaimed {
    LeaseClaimed::new(
        resource(),
        holder,
        LeaseEpoch::first(),
        acquired_at,
        expires_at,
    )
}

fn claim_hash(claim: &LeaseClaimed) -> LeaseContentHash {
    LeaseFact::Claimed(claim.clone()).content_hash()
}

#[test]
fn first_local_claim_becomes_active_and_reports_visible_nodes() {
    let book = LeaseBook::new();
    let decision = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("acquire succeeds");

    let LeaseDecision::Acquired(acquired) = decision else {
        panic!("expected acquired");
    };
    assert_eq!(acquired.guard().epoch, LeaseEpoch::first());
    assert_eq!(acquired.visible_nodes.len(), 2);
    assert!(matches!(
        book.state(&resource(), at(101)),
        LeaseState::Active { .. }
    ));
}

#[test]
fn local_claim_does_not_require_visible_peer_witnesses() {
    let book = LeaseBook::new();
    let decision = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            local_context(),
        )
        .expect("local acquire succeeds");

    let LeaseDecision::Acquired(acquired) = decision else {
        panic!("expected acquired");
    };
    assert!(acquired.visible_nodes.is_empty());
    assert_eq!(book.fact_count(), 1);
}

#[test]
fn active_claim_returns_conflict_before_mutation() {
    let book = LeaseBook::new();
    let first = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("first acquire");
    let before_conflict = book.fact_count();
    let second = book
        .try_acquire(
            resource(),
            holder("issuer-b"),
            at(101),
            &policy(),
            context(),
        )
        .expect("contention is a decision");

    assert!(matches!(first, LeaseDecision::Acquired(_)));
    assert!(matches!(
        second,
        LeaseDecision::Conflict(conflict)
            if conflict.conflicting_holder == holder("issuer-a")
                && conflict.visible_nodes.len() == 2
    ));
    assert_eq!(book.fact_count(), before_conflict);
    assert!(matches!(
        book.state(&resource(), at(101)),
        LeaseState::Active { current, superseded }
            if current.holder == holder("issuer-a") && superseded.is_empty()
    ));
}

#[test]
fn renewal_by_current_holder_extends_expiry() {
    let book = LeaseBook::new();
    let acquired = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("acquire")
        .into_acquired()
        .expect("acquired");
    let guard = acquired.guard();

    book.renew(guard, at(105), &policy()).expect("renew");

    assert!(matches!(
        book.state(&resource(), at(114)),
        LeaseState::Active {
            current,
            ..
        } if current.expires_at == at(115)
    ));
}

#[test]
fn stale_holder_cannot_renew() {
    let book = LeaseBook::new();
    let stale = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("first acquire")
        .into_acquired()
        .expect("acquired")
        .into_guard();
    drop(stale);
    let newer = book
        .try_acquire(
            resource(),
            holder("issuer-b"),
            at(101),
            &policy(),
            context(),
        )
        .expect("second acquire")
        .into_acquired()
        .expect("acquired");

    let error = book
        .renew(newer.guard(), at(112), &policy())
        .expect_err("expired guard cannot renew");

    assert!(matches!(error, LeaseError::StaleGuard { .. }));
}

#[test]
fn guard_from_another_book_cannot_mutate_matching_state() {
    let original_book = LeaseBook::new();
    let foreign_guard = original_book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("acquire")
        .into_acquired()
        .expect("acquired")
        .into_guard();
    let other_book = LeaseBook::new();
    other_book.importer().record(LeaseFact::Claimed(claim(
        holder("issuer-a"),
        at(100),
        at(110),
    )));

    let error = other_book
        .renew(&foreign_guard, at(101), &policy())
        .expect_err("foreign guard cannot renew");

    assert!(matches!(error, LeaseError::ForeignGuard { .. }));
}

#[test]
fn explicit_release_ends_ownership() {
    let book = LeaseBook::new();
    let mut guard = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("acquire")
        .into_acquired()
        .expect("acquired")
        .into_guard();

    book.release(&mut guard, at(101)).expect("release");

    assert!(matches!(
        book.state(&resource(), at(102)),
        LeaseState::Released {
            next_epoch,
            ..
        } if next_epoch == Some(second_epoch())
    ));
}

#[test]
fn dropping_local_guard_records_best_effort_release() {
    let book = LeaseBook::new();
    {
        let _guard = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("acquire")
            .into_acquired()
            .expect("acquired")
            .into_guard();
    }

    assert!(matches!(
        book.state(&resource(), at(101)),
        LeaseState::Released { .. }
    ));
    assert!(book.fact_count() >= 2);
}

#[test]
fn expired_lease_allows_next_holder_with_incremented_epoch() {
    let book = LeaseBook::new();
    let guard = book
        .try_acquire(
            resource(),
            holder("issuer-a"),
            at(100),
            &policy(),
            context(),
        )
        .expect("first acquire")
        .into_acquired()
        .expect("acquired")
        .into_guard();
    std::mem::forget(guard);

    let second = book
        .try_acquire(
            resource(),
            holder("issuer-b"),
            at(111),
            &policy(),
            context(),
        )
        .expect("second acquire");

    assert!(matches!(
        second,
        LeaseDecision::Acquired(acquired)
            if acquired.guard().holder == holder("issuer-b")
                && acquired.guard().epoch == second_epoch()
    ));
}

#[test]
fn conflicting_same_epoch_claims_reduce_deterministically_with_superseded_loser() {
    let first_claim = claim(holder("issuer-a"), at(100), at(110));
    let second_claim = claim(holder("issuer-b"), at(100), at(110));
    let first_hash = claim_hash(&first_claim);
    let second_hash = claim_hash(&second_claim);
    let winner_hash = first_hash.min(second_hash);
    let loser_hash = first_hash.max(second_hash);

    let forward = active_state_for_claim_order([first_claim.clone(), second_claim.clone()]);
    let reversed = active_state_for_claim_order([second_claim, first_claim]);

    for (current, superseded) in [forward, reversed] {
        assert_eq!(current.epoch, LeaseEpoch::first());
        assert_eq!(current.content_hash, winner_hash);
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0].content_hash, loser_hash);
        assert_eq!(superseded[0].by_content_hash, winner_hash);
        assert_eq!(superseded[0].by_epoch, LeaseEpoch::first());
    }
}

fn active_state_for_claim_order(claims: [LeaseClaimed; 2]) -> (LeaseCurrent, Vec<LeaseSuperseded>) {
    let book = LeaseBook::new();
    for claim in claims {
        book.importer().record(LeaseFact::Claimed(claim));
    }

    let LeaseState::Active {
        current,
        superseded,
    } = book.state(&resource(), at(101))
    else {
        panic!("expected deterministic active state");
    };
    (current, superseded)
}

#[test]
fn renew_for_superseded_same_holder_claim_does_not_extend_winner() {
    let book = LeaseBook::new();
    let first_claim = claim(holder("issuer-a"), at(100), at(110));
    let second_claim = claim(holder("issuer-a"), at(101), at(111));
    let first_hash = LeaseFact::Claimed(first_claim.clone()).content_hash();
    let second_hash = LeaseFact::Claimed(second_claim.clone()).content_hash();
    let winner_hash = first_hash.min(second_hash);
    let loser_hash = first_hash.max(second_hash);
    book.importer().record(LeaseFact::Claimed(first_claim));
    book.importer().record(LeaseFact::Claimed(second_claim));
    book.importer().record(LeaseFact::Renewed(LeaseRenewed::new(
        resource(),
        holder("issuer-a"),
        LeaseEpoch::first(),
        loser_hash,
        at(105),
        at(200),
    )));

    assert!(matches!(
        book.state(&resource(), at(150)),
        LeaseState::Expired { previous, .. } if previous.content_hash == winner_hash
    ));
}

#[test]
fn imported_stale_renewal_after_expiry_does_not_resurrect_claim() {
    let book = LeaseBook::new();
    let expired_claim = claim(holder("issuer-a"), at(100), at(110));
    let expired_hash = claim_hash(&expired_claim);
    book.importer()
        .record(LeaseFact::Claimed(expired_claim.clone()));
    book.importer().record(LeaseFact::Renewed(LeaseRenewed::new(
        resource(),
        holder("issuer-a"),
        LeaseEpoch::first(),
        expired_hash,
        at(150),
        at(160),
    )));

    assert!(matches!(
        book.state(&resource(), at(120)),
        LeaseState::Expired { previous, .. } if previous.content_hash == expired_hash
    ));

    let second = book
        .try_acquire(
            resource(),
            holder("issuer-b"),
            at(120),
            &policy(),
            context(),
        )
        .expect("takeover after real expiry");

    assert!(matches!(
        second,
        LeaseDecision::Acquired(acquired)
            if acquired.guard().holder == holder("issuer-b")
                && acquired.guard().epoch == second_epoch()
    ));
}

#[test]
fn release_for_superseded_same_holder_claim_does_not_release_winner() {
    let book = LeaseBook::new();
    let first_claim = claim(holder("issuer-a"), at(100), at(110));
    let second_claim = claim(holder("issuer-a"), at(101), at(111));
    let first_hash = LeaseFact::Claimed(first_claim.clone()).content_hash();
    let second_hash = LeaseFact::Claimed(second_claim.clone()).content_hash();
    let winner_hash = first_hash.min(second_hash);
    let loser_hash = first_hash.max(second_hash);
    book.importer().record(LeaseFact::Claimed(first_claim));
    book.importer().record(LeaseFact::Claimed(second_claim));
    book.importer()
        .record(LeaseFact::Released(LeaseReleased::new_at(
            resource(),
            holder("issuer-a"),
            LeaseEpoch::first(),
            loser_hash,
            at(102),
        )));

    assert!(matches!(
        book.state(&resource(), at(103)),
        LeaseState::Active { current, .. } if current.content_hash == winner_hash
    ));
}

#[test]
fn zero_epoch_is_unrepresentable() {
    let error = LeaseEpoch::from_u64(0).expect_err("zero epoch fails");

    assert_eq!(error, LeaseEpochError::Zero);
}

#[test]
fn max_epoch_is_reserved_to_prevent_silent_fencing_overflow() {
    let error = LeaseEpoch::from_u64(u64::MAX).expect_err("max epoch fails");
    let max_minus_one = LeaseEpoch::from_u64(u64::MAX - 1).expect("max minus one");

    assert_eq!(error, LeaseEpochError::MaxValue);
    assert_eq!(max_minus_one.next(), Err(LeaseEpochError::MaxValue));
}

#[test]
fn exhausted_epoch_boundary_is_reported_without_panicking() {
    let max_minus_one = LeaseEpoch::from_u64(u64::MAX - 1).expect("max minus one");
    let expired_book = LeaseBook::new();
    expired_book
        .importer()
        .record(LeaseFact::Claimed(LeaseClaimed::new(
            resource(),
            holder("issuer-a"),
            max_minus_one,
            at(100),
            at(110),
        )));

    assert!(matches!(
        expired_book.state(&resource(), at(120)),
        LeaseState::Expired {
            next_epoch: None,
            ..
        }
    ));
    let error = expired_book
        .try_acquire(
            resource(),
            holder("issuer-b"),
            at(120),
            &policy(),
            context(),
        )
        .expect_err("epoch boundary is a structured failure");
    assert!(matches!(
        error,
        LeaseError::EpochOverflow {
            last_epoch,
            ..
        } if last_epoch == max_minus_one
    ));

    let released_book = LeaseBook::new();
    let released_claim = LeaseClaimed::new(
        resource(),
        holder("issuer-a"),
        max_minus_one,
        at(100),
        at(160),
    );
    let claim_hash = LeaseFact::Claimed(released_claim.clone()).content_hash();
    released_book
        .importer()
        .record(LeaseFact::Claimed(released_claim));
    released_book
        .importer()
        .record(LeaseFact::Released(LeaseReleased::new_at(
            resource(),
            holder("issuer-a"),
            max_minus_one,
            claim_hash,
            at(110),
        )));

    assert!(matches!(
        released_book.state(&resource(), at(120)),
        LeaseState::Released {
            next_epoch: None,
            ..
        }
    ));
}

#[test]
fn segmented_resource_encoding_escapes_delimiters() {
    let resource = LeaseResource::from_segments(["acme", "http01", "Example.COM", "a.b~c"]);

    assert_eq!(resource.as_str(), "acme.http01.Example~2ECOM.a~2Eb~7Ec");
}
