//! Backfilling one Sealing Key: Generate on a Machine, then Share and Install.

use ployz_core::{
    GenerateSealingKeyRequest, InstallSealingKeyRequest, Machine, MachineRpc, RpcResponseBody,
    SealingPublicKey, SealingRefusal, ShareSealingKeyRequest, WireGuardPublicKey, op,
};
use tonic::Request;

use super::*;
use crate::{
    corrosion::{ReplicatedStore, fake_cluster::FakeCluster},
    machine::SealingError,
};

struct Peer {
    local: LocalMachine,
    machine: Machine,
    dir: std::path::PathBuf,
}

impl Peer {
    async fn participating(prefix: &str, replicated: &ReplicatedStore) -> Self {
        let (dir, store, machine) = open_store(prefix);
        replicated.publish_local_machine(&machine).await.unwrap();
        let local = LocalMachine::new(store).with_cluster(Some((
            replicated.clone(),
            AdminClient::new("/no/such/ployz-admin.sock"),
        )));
        Self {
            local,
            machine,
            dir,
        }
    }

    /// A founder enrolled before Sealing Keys existed: Participating, holding none.
    async fn needing_access(prefix: &str, replicated: &ReplicatedStore) -> Self {
        let peer = Self::participating(prefix, replicated).await;
        SealingKey::set(&peer.dir, None).unwrap();
        peer
    }

    fn key(&self) -> Option<SealingKey> {
        SealingKey::load(&self.dir).unwrap()
    }

    fn held(&self) -> SealingPublicKey {
        self.key().expect("holds a Sealing Key").public_key()
    }

    async fn share_to(&self, recipient: &Peer) -> ployz_core::SealedSealingKey {
        self.local
            .share_sealing_key(ShareSealingKeyRequest {
                machine: recipient.machine.id,
                public_key: recipient.machine.public_key,
            })
            .await
            .unwrap()
            .sealed
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn refusal(error: LocalMachineError) -> SealingError {
    let LocalMachineError::Sealing(error) = error else {
        panic!("not a Sealing refusal: {error}")
    };
    error
}

#[tokio::test]
async fn generate_makes_one_key_and_then_repeats_it() {
    let (replicated, server) = fake_cluster::store().await;
    let peer = Peer::needing_access("ployzd-backfill-generate", &replicated).await;
    assert_eq!(peer.key(), None);

    let first = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await
        .unwrap();
    let again = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await
        .unwrap();

    assert_eq!(first.public_key, peer.held());
    assert_eq!(again, first);
    assert_eq!(
        replicated.sealing_public_key().await.unwrap(),
        Some(first.public_key)
    );
    assert_eq!(
        sealing(&peer.local).await,
        Some(ployz_core::Sealing::Held {
            public_key: first.public_key
        })
    );
    server.abort();
}

#[tokio::test]
async fn generate_refuses_when_the_cluster_already_names_a_key() {
    let (replicated, server) = fake_cluster::store().await;
    let peer = Peer::needing_access("ployzd-backfill-published", &replicated).await;
    let elsewhere = SealingKey::generate().public_key();
    replicated
        .publish_sealing_public_key(elsewhere)
        .await
        .unwrap();

    let refused = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await
        .unwrap_err();

    assert!(matches!(refusal(refused), SealingError::Published(key) if key == elsewhere));
    assert_eq!(peer.key(), None, "a refused Generate writes no key");
    server.abort();
}

#[tokio::test]
async fn generate_refuses_while_the_cluster_awaits_recovery() {
    let cluster = FakeCluster::start();
    let replicated = cluster.store();
    let awaited = SealingKey::generate().public_key();
    cluster.write_sealing_awaits_recovery(&awaited.to_string());
    let (dir, store, founder) = open_store("ployzd-backfill-awaits");
    SealingKey::set(&dir, None).unwrap();
    replicated.publish_local_machine(&founder).await.unwrap();
    let service = machine_service(store, replicated, None);

    let request = Request::new(
        op::GenerateSealingKey::into_request(GenerateSealingKeyRequest {})
            .encode()
            .unwrap(),
    );
    let response = service
        .generate_sealing_key(request)
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();

    let RpcResponseBody::Error(error) = &response.body else {
        panic!("Generate over a recovery marker must refuse: {response:?}");
    };
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(
        error.sealing_refusal(),
        Some(SealingRefusal::AwaitsRecovery)
    );
    assert_eq!(SealingKey::load(&dir).unwrap(), None);
    drop(service);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn share_and_install_carry_the_key_to_a_machine_needing_access() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-holder", &replicated).await;
    let joiner = Peer::needing_access("ployzd-backfill-joiner", &replicated).await;
    assert_eq!(
        sealing(&joiner.local).await,
        Some(ployz_core::Sealing::NeedsAccess)
    );

    let sealed = holder.share_to(&joiner).await;
    let install = InstallSealingKeyRequest {
        from: holder.machine.id,
        sealed,
        public_key: holder.held(),
    };
    let installed = joiner
        .local
        .install_sealing_key(install.clone())
        .await
        .unwrap();
    let again = joiner.local.install_sealing_key(install).await.unwrap();

    assert_eq!(installed.public_key, holder.held());
    assert_eq!(again, installed);
    assert_eq!(joiner.key(), holder.key());
    assert_eq!(
        replicated.sealing_public_key().await.unwrap(),
        Some(holder.held())
    );
    server.abort();
}

#[tokio::test]
async fn install_refuses_a_key_sealed_by_someone_other_than_the_named_sender() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-sender", &replicated).await;
    let other = Peer::participating("ployzd-backfill-other-sender", &replicated).await;
    let joiner = Peer::needing_access("ployzd-backfill-sender-joiner", &replicated).await;
    let sealed = holder.share_to(&joiner).await;

    let wrong_sender = joiner
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: other.machine.id,
            sealed: sealed.clone(),
            public_key: holder.held(),
        })
        .await
        .unwrap_err();
    let unknown = MachineId::random();
    let unknown_sender = joiner
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: unknown,
            sealed,
            public_key: holder.held(),
        })
        .await
        .unwrap_err();

    assert!(matches!(
        refusal(wrong_sender),
        SealingError::Unopenable { from, .. } if from == other.machine.id
    ));
    assert!(matches!(
        refusal(unknown_sender),
        SealingError::UnknownMachine(id) if id == unknown
    ));
    assert_eq!(joiner.key(), None);
    server.abort();
}

#[tokio::test]
async fn install_refuses_a_key_that_is_not_the_one_named() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-named", &replicated).await;
    let joiner = Peer::needing_access("ployzd-backfill-named-joiner", &replicated).await;
    let sealed = holder.share_to(&joiner).await;
    let named = SealingKey::generate().public_key();

    let refused = joiner
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed,
            public_key: named,
        })
        .await
        .unwrap_err();

    assert!(matches!(
        refusal(refused),
        SealingError::SealedAnother { opened, requested }
            if opened == holder.held() && requested == named
    ));
    assert_eq!(joiner.key(), None);
    server.abort();
}

#[tokio::test]
async fn install_refuses_to_replace_a_key_this_machine_or_the_cluster_holds() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-replace", &replicated).await;
    let other_holder = Peer::participating("ployzd-backfill-replace-held", &replicated).await;
    let joiner = Peer::needing_access("ployzd-backfill-replace-joiner", &replicated).await;
    let own = other_holder.held();

    let held_another = other_holder
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed: holder.share_to(&other_holder).await,
            public_key: holder.held(),
        })
        .await
        .unwrap_err();
    replicated.publish_sealing_public_key(own).await.unwrap();
    let published = joiner
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed: holder.share_to(&joiner).await,
            public_key: holder.held(),
        })
        .await
        .unwrap_err();

    assert!(matches!(
        refusal(held_another),
        SealingError::HeldAnother { held, requested }
            if held == own && requested == holder.held()
    ));
    assert_eq!(other_holder.held(), own);
    assert!(matches!(
        refusal(published),
        SealingError::Published(key) if key == own
    ));
    assert_eq!(joiner.key(), None);
    server.abort();
}

#[tokio::test]
async fn share_refuses_a_recipient_the_cluster_does_not_publish_that_way() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-share", &replicated).await;
    let joiner = Peer::needing_access("ployzd-backfill-share-joiner", &replicated).await;
    let unknown = MachineId::random();

    let mismatch = holder
        .local
        .share_sealing_key(ShareSealingKeyRequest {
            machine: joiner.machine.id,
            public_key: WireGuardPublicKey([7; 32]),
        })
        .await
        .unwrap_err();
    let unlisted = holder
        .local
        .share_sealing_key(ShareSealingKeyRequest {
            machine: unknown,
            public_key: joiner.machine.public_key,
        })
        .await
        .unwrap_err();
    let not_held = joiner
        .local
        .share_sealing_key(ShareSealingKeyRequest {
            machine: holder.machine.id,
            public_key: holder.machine.public_key,
        })
        .await
        .unwrap_err();

    assert!(matches!(
        refusal(mismatch),
        SealingError::WireGuardKeyMismatch(id) if id == joiner.machine.id
    ));
    assert!(matches!(
        refusal(unlisted),
        SealingError::UnknownMachine(id) if id == unknown
    ));
    assert!(matches!(refusal(not_held), SealingError::NotHeld));
    server.abort();
}
