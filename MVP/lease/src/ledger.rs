use super::*;

pub(super) fn reduce_lease_state(
    facts: &[LeaseFact],
    resource: &LeaseResource,
    now: LeaseTimestamp,
) -> LeaseState {
    let mut highest_epoch = None;
    let mut candidates = Vec::new();
    for fact in facts {
        let LeaseFact::Claimed(claim) = fact else {
            continue;
        };
        if &claim.resource != resource {
            continue;
        }
        match highest_epoch {
            Some(epoch) if claim.epoch < epoch => {}
            Some(epoch) if claim.epoch == epoch => {
                candidates.push(LeaseCandidate::from_claim(claim));
            }
            Some(_) | None => {
                highest_epoch = Some(claim.epoch);
                candidates.clear();
                candidates.push(LeaseCandidate::from_claim(claim));
            }
        }
    }
    let Some(highest_epoch) = highest_epoch else {
        return LeaseState::Vacant {
            resource: resource.clone(),
            next_epoch: LeaseEpoch::first(),
        };
    };
    candidates.sort_by_key(|candidate| candidate.content_hash);

    let (winner, losers) = candidates
        .split_first()
        .expect("highest_epoch set implies lease candidates are non-empty");

    let expires_at = latest_expiry(facts, winner);
    let current = LeaseCurrent {
        resource: winner.resource.clone(),
        holder: winner.holder.clone(),
        epoch: winner.epoch,
        acquired_at: winner.acquired_at,
        expires_at,
        content_hash: winner.content_hash,
    };
    let superseded = losers
        .iter()
        .map(|loser| LeaseSuperseded {
            resource: loser.resource.clone(),
            holder: loser.holder.clone(),
            epoch: loser.epoch,
            content_hash: loser.content_hash,
            by_epoch: winner.epoch,
            by_holder: winner.holder.clone(),
            by_content_hash: winner.content_hash,
            at: winner.acquired_at,
        })
        .collect::<Vec<_>>();

    if let Some(release) = latest_release(facts, winner, expires_at, now) {
        return LeaseState::Released {
            previous: current,
            release,
            next_epoch: highest_epoch.next().ok(),
            superseded,
        };
    }

    if expires_at <= now {
        return LeaseState::Expired {
            previous: current,
            expired_at: expires_at,
            next_epoch: highest_epoch.next().ok(),
            superseded,
        };
    }

    LeaseState::Active {
        current,
        superseded,
    }
}

#[derive(Debug, Clone)]
struct LeaseCandidate {
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
    content_hash: LeaseContentHash,
}

impl LeaseCandidate {
    fn from_claim(claim: &LeaseClaimed) -> Self {
        Self {
            resource: claim.resource.clone(),
            holder: claim.holder.clone(),
            epoch: claim.epoch,
            acquired_at: claim.acquired_at,
            expires_at: claim.expires_at,
            content_hash: claimed_content_hash(claim),
        }
    }
}

fn latest_release(
    facts: &[LeaseFact],
    candidate: &LeaseCandidate,
    expires_at: LeaseTimestamp,
    now: LeaseTimestamp,
) -> Option<LeaseRelease> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            LeaseFact::Released(released)
                if released.resource == candidate.resource
                    && released.holder == candidate.holder
                    && released.epoch == candidate.epoch
                    && released.claim_hash == candidate.content_hash =>
            {
                release_if_observable(released.release, candidate.acquired_at, expires_at, now)
            }
            LeaseFact::Claimed(_) | LeaseFact::Renewed(_) | LeaseFact::Released(_) => None,
        })
        .max_by_key(|release| release_order(*release))
}

fn latest_expiry(facts: &[LeaseFact], candidate: &LeaseCandidate) -> LeaseTimestamp {
    let mut renewals = facts
        .iter()
        .filter_map(|fact| match fact {
            LeaseFact::Renewed(renewed)
                if renewed.resource == candidate.resource
                    && renewed.holder == candidate.holder
                    && renewed.epoch == candidate.epoch
                    && renewed.claim_hash == candidate.content_hash =>
            {
                Some((
                    renewed.renewed_at,
                    renewed_content_hash(renewed),
                    renewed.expires_at,
                ))
            }
            LeaseFact::Claimed(_) | LeaseFact::Renewed(_) | LeaseFact::Released(_) => None,
        })
        .collect::<Vec<_>>();
    renewals.sort_by_key(|(renewed_at, content_hash, _expires_at)| (*renewed_at, *content_hash));

    let mut expires_at = candidate.expires_at;
    for (renewed_at, _content_hash, renewed_expires_at) in renewals {
        if renewed_at >= candidate.acquired_at
            && renewed_at < expires_at
            && renewed_expires_at > renewed_at
        {
            expires_at = renewed_expires_at;
        }
    }
    expires_at
}

fn release_if_observable(
    release: LeaseRelease,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
    now: LeaseTimestamp,
) -> Option<LeaseRelease> {
    match release {
        LeaseRelease::At(released_at)
            if released_at >= acquired_at && released_at < expires_at && released_at <= now =>
        {
            Some(release)
        }
        LeaseRelease::DroppedWithoutTimestamp => Some(release),
        LeaseRelease::At(_) => None,
    }
}

fn release_order(release: LeaseRelease) -> u64 {
    match release {
        LeaseRelease::At(timestamp) => timestamp.value(),
        LeaseRelease::DroppedWithoutTimestamp => 0,
    }
}

fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hash_u64(hasher, value.len() as u64);
    hasher.update(value.as_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

pub(super) fn claimed_content_hash(fact: &LeaseClaimed) -> LeaseContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lease-claimed");
    hash_str(&mut hasher, fact.resource.as_str());
    hash_str(&mut hasher, fact.holder.as_str());
    hash_u64(&mut hasher, fact.epoch.value());
    hash_u64(&mut hasher, fact.acquired_at.value());
    hash_u64(&mut hasher, fact.expires_at.value());
    LeaseContentHash(*hasher.finalize().as_bytes())
}

pub(super) fn renewed_content_hash(fact: &LeaseRenewed) -> LeaseContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lease-renewed");
    hash_str(&mut hasher, fact.resource.as_str());
    hash_str(&mut hasher, fact.holder.as_str());
    hash_u64(&mut hasher, fact.epoch.value());
    hash_content_hash(&mut hasher, fact.claim_hash);
    hash_u64(&mut hasher, fact.renewed_at.value());
    hash_u64(&mut hasher, fact.expires_at.value());
    LeaseContentHash(*hasher.finalize().as_bytes())
}

pub(super) fn released_content_hash(fact: &LeaseReleased) -> LeaseContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lease-released");
    hash_str(&mut hasher, fact.resource.as_str());
    hash_str(&mut hasher, fact.holder.as_str());
    hash_u64(&mut hasher, fact.epoch.value());
    hash_content_hash(&mut hasher, fact.claim_hash);
    hash_release(&mut hasher, fact.release);
    LeaseContentHash(*hasher.finalize().as_bytes())
}

fn hash_content_hash(hasher: &mut blake3::Hasher, value: LeaseContentHash) {
    hasher.update(&value.0);
}

fn hash_release(hasher: &mut blake3::Hasher, release: LeaseRelease) {
    match release {
        LeaseRelease::At(timestamp) => {
            hasher.update(b"at");
            hash_u64(hasher, timestamp.value());
        }
        LeaseRelease::DroppedWithoutTimestamp => {
            hasher.update(b"dropped-without-timestamp");
        }
    }
}
