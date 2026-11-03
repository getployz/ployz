//! The Store calls that dial the Organization's Servers keep only an entry of the
//! Cluster they expect, as a Deploy's does, and dial any entry without one.

use super::runner_cancellation::{CutCleanup, cut_cleanup};
use super::support::*;
use ployz::connect::ClusterExpectation;
use ployz::context::Connection;
use ployz_core::SealingPublicKey;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn key(byte: u8) -> SealingPublicKey {
    SealingPublicKey::from_bytes([byte; 32])
}

fn expecting(byte: u8) -> Option<ClusterExpectation> {
    Some(ClusterExpectation {
        sealing_public_key: key(byte),
        allow_unverified: false,
    })
}

#[tokio::test]
async fn observing_copies_keeps_only_an_entry_of_the_expected_cluster() {
    let other = DeployService::new(machine('a', "other")).sealed_with(key(9));
    let own = DeployService::new(machine('b', "own")).sealed_with(key(7));
    let (other_address, other_server) = listening(other).await;
    let (own_address, own_server) = listening(own).await;
    let (other, own) = (Connection::tcp(other_address), Connection::tcp(own_address));

    let refused = ployz::sdk::observe_copies(vec![other.clone()], expecting(7)).await;
    assert!(
        refused.is_err(),
        "only another Cluster's entry answered: {refused:?}"
    );
    ployz::sdk::observe_copies(vec![other.clone(), own], expecting(7))
        .await
        .expect("the race leaves the other Cluster's entry for its own");
    ployz::sdk::observe_copies(vec![other], None)
        .await
        .expect("without an expectation any entry answers, as before");

    other_server.abort();
    own_server.abort();
}

#[tokio::test]
async fn retiring_old_copies_keeps_only_an_entry_of_the_expected_cluster() {
    let CutCleanup {
        store,
        actor,
        id,
        server,
        ..
    } = cut_cleanup().await;
    server.abort();
    // The Deployment left a copy on `entry`; its daemon now answers with no copy there.
    let entry = DeployService::new(machine('a', "entry")).sealed_with(key(7));
    let mutations = entry.mutating_rpcs();
    let (address, server) = listening(entry).await;
    let retire = |expect| {
        ployz::sdk::retire_old(
            std::sync::Arc::clone(&store),
            actor.clone(),
            id.clone(),
            vec![Connection::tcp(address)],
            expect,
        )
    };

    let refused = retire(expecting(9)).await;
    assert!(
        refused.is_err(),
        "the only entry is another Cluster's: {refused:?}"
    );
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
    let retired = tokio::time::timeout(Duration::from_secs(10), retire(expecting(7)))
        .await
        .expect("retiring through the expected Cluster's entry ends")
        .expect("the expected Cluster's entry retires");
    assert_eq!(retired, ployz::sdk::RetireOutcome::default());

    server.abort();
}
