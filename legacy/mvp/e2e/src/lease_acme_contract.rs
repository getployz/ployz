use std::time::Instant;

use mvp_acme::{
    AcmeChallengeCoordinator, AcmeChallengeId, AcmeChallengeToken, AcmeCoordinationError,
    AcmeHostname, AcmeKeyAuthorization, harness as acme_harness,
};
use mvp_lease::{
    LeaseAcquirePolicy, LeaseClaimed, LeaseCommandContext, LeaseContentHash, LeaseDuration,
    LeaseEpoch, LeaseError, LeaseFact, LeaseHolder, LeaseResource, LeaseState, LeaseTimestamp,
    harness::visible_nodes,
};
use serde::Serialize;

use crate::assertions::{assert_eq_named, assert_error, expect_error};
use crate::metrics::{reset_dir, scenario_dir, write_json};

const SCENARIO: &str = "lease-acme-contract";
const TOKEN_A: &str = "tokA0123456789abcdefgh";
const TOKEN_CONFLICT: &str = "tokC0123456789abcdefgh";
const TOKEN_DROP: &str = "tokD0123456789abcdefgh";
const TOKEN_LOCAL: &str = "tokL0123456789abcdefgh";

#[derive(Debug, Serialize)]
struct LeaseAcmeContractReport {
    scenario: &'static str,
    first_publish_succeeded: bool,
    visible_nodes_at_decision: usize,
    conflict_detected: bool,
    expired_takeover_succeeded: bool,
    stale_publish_rejected: bool,
    stale_delete_rejected: bool,
    local_only_publish_succeeded: bool,
    superseded_candidate_count: usize,
    superseded_local_mutation_rejected: bool,
    release_on_drop_recorded: bool,
    lease_facts_recorded: usize,
    challenge_records: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;

    let policy =
        LeaseAcquirePolicy::new(LeaseDuration::from_secs(10).map_err(|error| error.to_string())?);
    let mut coordinator = AcmeChallengeCoordinator::new(policy.clone());
    let issuer_a = holder("issuer-a");
    let issuer_b = holder("issuer-b");
    let hostname = hostname("Example.COM.")?;
    let token = token(TOKEN_A)?;

    let first = coordinator
        .acquire_challenge(
            issuer_a.clone(),
            hostname.clone(),
            token.clone(),
            at(100),
            context(["node-a", "node-b"]),
        )
        .map_err(|error| format!("issuer-a should acquire first challenge lease: {error}"))?;
    coordinator
        .publish_challenge(&first, key_auth(first.id().token(), "keyauthA")?, at(101))
        .map_err(|error| format!("issuer-a should publish challenge: {error}"))?;
    let first_publish_succeeded = coordinator
        .active_challenge(first.id(), at(101))
        .is_some_and(|record| record.holder() == &issuer_a);
    let visible_nodes_at_decision = first.visible_nodes().len();

    let conflict = expect_error(
        "issuer-b active challenge conflict",
        coordinator.acquire_challenge(
            issuer_b.clone(),
            hostname.clone(),
            token.clone(),
            at(102),
            context(["node-a", "node-b"]),
        ),
    )?;
    assert_error("issuer-b active conflict", &conflict, |error| {
        matches!(
            error,
            AcmeCoordinationError::Lease(LeaseError::Conflict(conflict))
                if conflict.conflicting_holder == issuer_a
                    && conflict.visible_nodes.len() == 2
        )
    })?;
    let conflict_detected = true;

    let second = coordinator
        .acquire_challenge(
            issuer_b.clone(),
            hostname.clone(),
            token.clone(),
            at(111),
            context(["node-a", "node-b"]),
        )
        .map_err(|error| format!("issuer-b should acquire after expiry: {error}"))?;
    coordinator
        .publish_challenge(&second, key_auth(second.id().token(), "keyauthB")?, at(112))
        .map_err(|error| format!("issuer-b should publish after takeover: {error}"))?;
    let expired_takeover_succeeded = coordinator
        .active_challenge(second.id(), at(112))
        .is_some_and(|record| {
            record.holder() == &issuer_b
                && record.epoch() == second_epoch()
                && record.key_authorization().as_str()
                    == format!("{}.keyauthB", second.id().token().as_str())
        });

    let stale_publish = expect_error(
        "stale issuer publish",
        coordinator.publish_challenge(
            &first,
            key_auth(first.id().token(), "keyauthStale")?,
            at(113),
        ),
    )?;
    assert_error("stale publish rejected", &stale_publish, |error| {
        matches!(
            error,
            AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
        )
    })?;
    let stale_publish_rejected = true;
    assert_active_record(
        &coordinator,
        second.id(),
        at(113),
        &issuer_b,
        second_epoch(),
        &format!("{}.keyauthB", second.id().token().as_str()),
    )?;
    assert_eq_named(
        "challenge count after stale publish",
        acme_harness::challenge_count(&coordinator),
        1,
    )?;

    let stale_delete = expect_error(
        "stale issuer delete",
        coordinator.delete_challenge(&first, at(114)),
    )?;
    assert_error("stale delete rejected", &stale_delete, |error| {
        matches!(
            error,
            AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
        )
    })?;
    assert_active_record(
        &coordinator,
        second.id(),
        at(114),
        &issuer_b,
        second_epoch(),
        &format!("{}.keyauthB", second.id().token().as_str()),
    )?;
    let stale_delete_rejected = acme_harness::challenge_count(&coordinator) == 1;

    let local_only_publish_succeeded = local_only_publish_succeeded(&policy)?;
    let superseded_candidate_count = superseded_candidate_count(&policy)?;
    let superseded_local_mutation_rejected = superseded_local_mutation_rejected(&policy)?;
    let release_on_drop_recorded = release_on_drop_recorded(&policy)?;

    let report = LeaseAcmeContractReport {
        scenario: SCENARIO,
        first_publish_succeeded,
        visible_nodes_at_decision,
        conflict_detected,
        expired_takeover_succeeded,
        stale_publish_rejected,
        stale_delete_rejected,
        local_only_publish_succeeded,
        superseded_candidate_count,
        superseded_local_mutation_rejected,
        release_on_drop_recorded,
        lease_facts_recorded: acme_harness::lease_fact_count(&coordinator),
        challenge_records: acme_harness::challenge_count(&coordinator),
        elapsed_ms: started.elapsed().as_millis(),
    };

    assert_eq_named(
        "first publish succeeded",
        report.first_publish_succeeded,
        true,
    )?;
    assert_eq_named("visible nodes", report.visible_nodes_at_decision, 2)?;
    assert_eq_named("conflict detected", report.conflict_detected, true)?;
    assert_eq_named(
        "expired takeover succeeded",
        report.expired_takeover_succeeded,
        true,
    )?;
    assert_eq_named(
        "stale publish rejected",
        report.stale_publish_rejected,
        true,
    )?;
    assert_eq_named("stale delete rejected", report.stale_delete_rejected, true)?;
    assert_eq_named(
        "local-only publish succeeded",
        report.local_only_publish_succeeded,
        true,
    )?;
    assert_eq_named(
        "superseded candidates",
        report.superseded_candidate_count,
        1,
    )?;
    assert_eq_named(
        "superseded local mutation rejected",
        report.superseded_local_mutation_rejected,
        true,
    )?;
    assert_eq_named(
        "release on drop recorded",
        report.release_on_drop_recorded,
        true,
    )?;
    assert_eq_named("challenge records", report.challenge_records, 1)?;

    let json = write_json(&root.join("lease-acme-contract-metrics.json"), &report)?;
    println!("{json}");
    eprintln!("PASS {SCENARIO}");
    Ok(())
}

fn local_only_publish_succeeded(policy: &LeaseAcquirePolicy) -> Result<bool, String> {
    let mut coordinator = AcmeChallengeCoordinator::new(policy.clone());
    let lease = coordinator
        .acquire_challenge(
            holder("issuer-local"),
            hostname("local.example.com")?,
            token(TOKEN_LOCAL)?,
            at(100),
            context([]),
        )
        .map_err(|error| format!("local-only acquire: {error}"))?;
    coordinator
        .publish_challenge(
            &lease,
            key_auth(lease.id().token(), "keyauthLocal")?,
            at(101),
        )
        .map_err(|error| format!("local-only publish: {error}"))?;

    Ok(lease.visible_nodes().is_empty()
        && coordinator
            .active_challenge(lease.id(), at(101))
            .is_some_and(|record| record.holder() == &holder("issuer-local")))
}

fn superseded_candidate_count(policy: &LeaseAcquirePolicy) -> Result<usize, String> {
    let id = AcmeChallengeId::new(hostname("conflict.example.com")?, token(TOKEN_CONFLICT)?);
    let resource = id.lease_resource().clone();
    let coordinator = AcmeChallengeCoordinator::new(policy.clone());
    acme_harness::record_observed_lease_fact(
        &coordinator,
        claim(resource.clone(), holder("issuer-a"), LeaseEpoch::first()),
    );
    acme_harness::record_observed_lease_fact(
        &coordinator,
        claim(resource, holder("issuer-b"), LeaseEpoch::first()),
    );

    let LeaseState::Active { superseded, .. } =
        acme_harness::lease_state(&coordinator, &id, at(101))
    else {
        return Err(
            "conflicting claims should reduce to active winner with superseded loser".into(),
        );
    };
    Ok(superseded.len())
}

fn superseded_local_mutation_rejected(policy: &LeaseAcquirePolicy) -> Result<bool, String> {
    let mut coordinator = AcmeChallengeCoordinator::new(policy.clone());
    let lease = coordinator
        .acquire_challenge(
            holder("issuer-local"),
            hostname("same-epoch.example.com")?,
            token(TOKEN_CONFLICT)?,
            at(100),
            context(["node-a"]),
        )
        .map_err(|error| format!("same-epoch acquire: {error}"))?;
    coordinator
        .publish_challenge(
            &lease,
            key_auth(lease.id().token(), "keyauthLocal")?,
            at(101),
        )
        .map_err(|error| format!("same-epoch publish: {error}"))?;
    let challenge_count = acme_harness::challenge_count(&coordinator);
    let winning_foreign = winning_claim_against(lease.id(), lease.claim_hash());
    acme_harness::record_observed_lease_fact(&coordinator, LeaseFact::Claimed(winning_foreign));

    let publish = expect_error(
        "superseded same-epoch publish",
        coordinator.publish_challenge(
            &lease,
            key_auth(lease.id().token(), "keyauthStale")?,
            at(102),
        ),
    )?;
    let delete = expect_error(
        "superseded same-epoch delete",
        coordinator.delete_challenge(&lease, at(102)),
    )?;

    Ok(matches!(
        publish,
        AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
    ) && matches!(
        delete,
        AcmeCoordinationError::Lease(LeaseError::Superseded { .. })
    ) && acme_harness::challenge_count(&coordinator) == challenge_count
        && coordinator.active_challenge(lease.id(), at(102)).is_none())
}

fn winning_claim_against(id: &AcmeChallengeId, local_hash: LeaseContentHash) -> LeaseClaimed {
    for suffix in 0..10_000 {
        let claim = LeaseClaimed::new(
            id.lease_resource().clone(),
            holder(&format!("foreign-{suffix}")),
            LeaseEpoch::first(),
            at(100),
            at(110),
        );
        let hash = LeaseFact::Claimed(claim.clone()).content_hash();
        if hash < local_hash {
            return claim;
        }
    }
    panic!("expected to find deterministic lower hash claim");
}

fn release_on_drop_recorded(policy: &LeaseAcquirePolicy) -> Result<bool, String> {
    let id = AcmeChallengeId::new(hostname("drop.example.com")?, token(TOKEN_DROP)?);
    let coordinator = AcmeChallengeCoordinator::new(policy.clone());
    {
        let _lease = coordinator
            .acquire_challenge(
                holder("issuer-a"),
                id.hostname().clone(),
                id.token().clone(),
                at(100),
                context(["node-a"]),
            )
            .map_err(|error| format!("drop lease acquire: {error}"))?;
    }

    Ok(matches!(
        acme_harness::lease_state(&coordinator, &id, at(101)),
        LeaseState::Released { .. }
    ))
}

fn claim(resource: LeaseResource, holder: LeaseHolder, epoch: LeaseEpoch) -> LeaseFact {
    LeaseFact::Claimed(LeaseClaimed::new(resource, holder, epoch, at(100), at(110)))
}

fn hostname(value: &str) -> Result<AcmeHostname, String> {
    AcmeHostname::parse(value).map_err(|error| format!("parse hostname {value}: {error}"))
}

fn token(value: &str) -> Result<AcmeChallengeToken, String> {
    AcmeChallengeToken::parse(value).map_err(|error| format!("parse token {value}: {error}"))
}

fn key_auth(token: &AcmeChallengeToken, thumbprint: &str) -> Result<AcmeKeyAuthorization, String> {
    let value = format!("{}.{thumbprint}", token.as_str());
    AcmeKeyAuthorization::parse_for_token(token, value.clone())
        .map_err(|error| format!("parse key authorization {value}: {error}"))
}

fn holder(value: &str) -> LeaseHolder {
    LeaseHolder::new(value)
}

fn at(value: u64) -> LeaseTimestamp {
    LeaseTimestamp::from_secs(value)
}

fn context<const N: usize>(nodes: [&str; N]) -> LeaseCommandContext {
    LeaseCommandContext::new(visible_nodes(nodes))
}

fn second_epoch() -> LeaseEpoch {
    LeaseEpoch::from_u64(2).expect("second epoch")
}

fn assert_active_record(
    coordinator: &AcmeChallengeCoordinator,
    id: &AcmeChallengeId,
    now: LeaseTimestamp,
    holder: &LeaseHolder,
    epoch: LeaseEpoch,
    key_authorization: &str,
) -> Result<(), String> {
    let record = coordinator
        .active_challenge(id, now)
        .ok_or_else(|| format!("expected active challenge for {}", id.lease_resource()))?;
    assert_eq_named("active challenge holder", record.holder(), holder)?;
    assert_eq_named("active challenge epoch", record.epoch(), epoch)?;
    assert_eq_named(
        "active challenge key authorization",
        record.key_authorization().as_str(),
        key_authorization,
    )
}
