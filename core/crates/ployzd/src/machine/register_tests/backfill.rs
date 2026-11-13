//! Backfilling one Sealing Key: Generate on a Machine, then Share and Install.

use ployz_core::{
    GenerateSealingKeyRequest, InstallSealingKeyRequest, Machine, MachineRpc,
    RecoverSealingKeyRequest, RecoveryCode, RpcResponseBody, Sealing, SealingKeyRecovered,
    SealingPublicKey, SealingRefusal, ShareSealingKeyRequest, WireGuardPublicKey,
    WrapSealingKeyRequest, op,
};
use tonic::Request;

use super::*;
use crate::{
    corrosion::{ReplicatedStore, fake_cluster::FakeCluster},
    machine::{RecoveryError, SealingError},
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

    /// Trust each of `others` as a Member, as a join approval or an activation would.
    async fn trust(&self, others: &[&Peer]) {
        let members = others.iter().map(|peer| (&peer.machine).into()).collect();
        self.local
            .owner()
            .mutate(move |store| store.trust_members(members).unwrap())
            .await
            .unwrap();
    }

    async fn share_to(&self, recipient: &Peer) -> ployz_core::SealedSealingKey {
        self.trust(&[recipient]).await;
        recipient.trust(&[self]).await;
        self.local
            .share_sealing_key(ShareSealingKeyRequest {
                join_approval: Some(approve_recipient(&self.local, &recipient.machine).await),
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
        error.evidence::<SealingRefusal>(),
        Some(SealingRefusal::AwaitsRecovery)
    );
    assert_eq!(SealingKey::load(&dir).unwrap(), None);
    drop(service);
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn inspect_reports_the_awaited_key_until_the_cluster_publishes_one() {
    let cluster = FakeCluster::start();
    let replicated = cluster.store();
    let awaited = SealingKey::generate().public_key();
    cluster.write_sealing_awaits_recovery(&awaited.to_string());
    let peer = Peer::needing_access("ployzd-backfill-inspect-awaits", &replicated).await;
    assert_eq!(
        sealing(&peer.local).await,
        Some(ployz_core::Sealing::AwaitsRecovery {
            public_key: awaited
        })
    );

    replicated
        .publish_sealing_public_key(awaited)
        .await
        .unwrap();

    assert_eq!(
        sealing(&peer.local).await,
        Some(ployz_core::Sealing::NeedsAccess)
    );
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
    joiner.trust(&[&other]).await;
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
    holder.trust(&[&joiner]).await;
    joiner.trust(&[&holder]).await;
    let unknown = MachineId::random();

    let mismatch = holder
        .local
        .share_sealing_key(ShareSealingKeyRequest {
            join_approval: None,
            machine: joiner.machine.id,
            public_key: WireGuardPublicKey([7; 32]),
        })
        .await
        .unwrap_err();
    let unlisted = holder
        .local
        .share_sealing_key(ShareSealingKeyRequest {
            join_approval: None,
            machine: unknown,
            public_key: joiner.machine.public_key,
        })
        .await
        .unwrap_err();
    let not_held = joiner
        .local
        .share_sealing_key(ShareSealingKeyRequest {
            join_approval: None,
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

fn recovery_refusal(error: LocalMachineError) -> RecoveryError {
    let LocalMachineError::Recovery(error) = error else {
        panic!("not a Recovery refusal: {error}")
    };
    error
}

/// Writes a key file that is not 32 bytes, as a hand edit or a damaged disk
/// could leave it. ployzd never writes one.
fn make_unusable(peer: &Peer) {
    std::fs::write(peer.dir.join("sealing.key"), b"not a key").unwrap();
}

fn key_file(peer: &Peer) -> Vec<u8> {
    std::fs::read(peer.dir.join("sealing.key")).unwrap()
}

#[tokio::test]
async fn install_replaces_an_unusable_key_file() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-unusable-install-holder", &replicated).await;
    let peer = Peer::needing_access("ployzd-backfill-unusable-install", &replicated).await;
    replicated
        .publish_sealing_public_key(holder.held())
        .await
        .unwrap();
    make_unusable(&peer);
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::NeedsAccess),
        "a founder whose key file is not 32 bytes reads as needing access"
    );

    let installed = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed: holder.share_to(&peer).await,
            public_key: holder.held(),
        })
        .await;

    assert!(
        matches!(&installed, Ok(held) if held.public_key == holder.held()),
        "Install over a key file that is not 32 bytes answered {installed:?}"
    );
    assert_eq!(peer.key(), holder.key());
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::Held {
            public_key: holder.held()
        })
    );
    server.abort();
}

#[tokio::test]
async fn recover_replaces_an_unusable_key_file() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-unusable-recover-holder", &replicated).await;
    let peer = Peer::needing_access("ployzd-backfill-unusable-recover", &replicated).await;
    let code = RecoveryCode::generate();
    let wrapped = holder
        .local
        .wrap_sealing_key(
            WrapSealingKeyRequest {
                code: code.clone(),
                key: ployz_core::SealingKeyRole::Identity,
            },
            false,
        )
        .unwrap();
    replicated
        .publish_sealing_public_key(wrapped.public_key)
        .await
        .unwrap();
    make_unusable(&peer);
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::NeedsAccess),
        "a founder whose key file is not 32 bytes reads as needing access"
    );

    let recovered = peer
        .local
        .recover_sealing_key(
            RecoverSealingKeyRequest {
                code,
                backup: Some(wrapped.backup),
                rotation: None,
            },
            false,
        )
        .await;

    assert!(
        matches!(
            &recovered,
            Ok(SealingKeyRecovered { public_key }) if *public_key == wrapped.public_key
        ),
        "Recover over a key file that is not 32 bytes answered {recovered:?}"
    );
    assert_eq!(peer.key(), holder.key());
    server.abort();
}

#[tokio::test]
async fn generate_replaces_an_unusable_key_file_while_the_cluster_names_no_key() {
    let (replicated, server) = fake_cluster::store().await;
    let peer = Peer::needing_access("ployzd-backfill-unusable-generate", &replicated).await;
    make_unusable(&peer);
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::NeedsAccess),
        "a founder whose key file is not 32 bytes reads as needing access"
    );

    let generated = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await;

    let Ok(generated) = generated else {
        panic!("Generate over a key file that is not 32 bytes answered {generated:?}");
    };
    assert_eq!(peer.held(), generated.public_key);
    assert_eq!(
        replicated.sealing_public_key().await.unwrap(),
        Some(generated.public_key)
    );
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::Held {
            public_key: generated.public_key
        })
    );
    server.abort();
}

#[tokio::test]
async fn generate_refuses_over_an_unusable_key_file_when_the_cluster_names_another_key() {
    let (replicated, server) = fake_cluster::store().await;
    let peer =
        Peer::needing_access("ployzd-backfill-unusable-generate-published", &replicated).await;
    let elsewhere = SealingKey::generate().public_key();
    replicated
        .publish_sealing_public_key(elsewhere)
        .await
        .unwrap();
    make_unusable(&peer);
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::NeedsAccess),
        "a founder whose key file is not 32 bytes reads as needing access"
    );

    let refused = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await;

    let Err(refused) = refused else {
        panic!("Generate over a published key answered {refused:?}");
    };
    let refused = refusal(refused);
    assert!(
        matches!(refused, SealingError::Published(key) if key == elsewhere),
        "Generate over a key file that is not 32 bytes, with {elsewhere} published, refused {refused:?}"
    );
    assert_eq!(
        key_file(&peer),
        b"not a key",
        "a refused Generate leaves the file"
    );
    server.abort();
}

#[tokio::test]
async fn install_refuses_over_an_unusable_key_file_when_the_cluster_names_another_key() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating(
        "ployzd-backfill-unusable-install-published-holder",
        &replicated,
    )
    .await;
    let peer =
        Peer::needing_access("ployzd-backfill-unusable-install-published", &replicated).await;
    let elsewhere = SealingKey::generate().public_key();
    replicated
        .publish_sealing_public_key(elsewhere)
        .await
        .unwrap();
    make_unusable(&peer);
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::NeedsAccess),
        "a founder whose key file is not 32 bytes reads as needing access"
    );

    let refused = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed: holder.share_to(&peer).await,
            public_key: holder.held(),
        })
        .await;

    let Err(refused) = refused else {
        panic!("Install of a key the Cluster does not publish answered {refused:?}");
    };
    let refused = refusal(refused);
    assert!(
        matches!(refused, SealingError::Published(key) if key == elsewhere),
        "Install over a key file that is not 32 bytes, with {elsewhere} published, refused {refused:?}"
    );
    assert_eq!(
        key_file(&peer),
        b"not a key",
        "a refused Install leaves the file"
    );
    server.abort();
}

#[tokio::test]
async fn routes_refuse_a_key_file_they_cannot_read_and_leave_it() {
    let (replicated, server) = fake_cluster::store().await;
    let holder = Peer::participating("ployzd-backfill-unreadable-holder", &replicated).await;
    let peer = Peer::needing_access("ployzd-backfill-unreadable", &replicated).await;
    let code = RecoveryCode::generate();
    let wrapped = holder
        .local
        .wrap_sealing_key(
            WrapSealingKeyRequest {
                code: code.clone(),
                key: ployz_core::SealingKeyRole::Identity,
            },
            false,
        )
        .unwrap();
    replicated
        .publish_sealing_public_key(wrapped.public_key)
        .await
        .unwrap();
    // Reading a directory fails with neither NotFound nor InvalidData, as root too.
    let file = peer.dir.join("sealing.key");
    std::fs::create_dir(&file).unwrap();

    let inspected = sealing(&peer.local).await;
    let generated = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await;
    let installed = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed: holder.share_to(&peer).await,
            public_key: holder.held(),
        })
        .await;
    let recovered = peer
        .local
        .recover_sealing_key(
            RecoverSealingKeyRequest {
                code,
                backup: Some(wrapped.backup),
                rotation: None,
            },
            false,
        )
        .await;

    assert_eq!(
        inspected, None,
        "Inspect names no Sealing for a key file it cannot read"
    );
    let Err(generated) = generated else {
        panic!("Generate over an unreadable key file answered {generated:?}");
    };
    let generated = refusal(generated);
    assert!(
        matches!(generated, SealingError::Unreadable(_)),
        "Generate over an unreadable key file refused {generated:?}"
    );
    let Err(installed) = installed else {
        panic!("Install over an unreadable key file answered {installed:?}");
    };
    let installed = refusal(installed);
    assert!(
        matches!(installed, SealingError::Unreadable(_)),
        "Install over an unreadable key file refused {installed:?}"
    );
    let Err(recovered) = recovered else {
        panic!("Recover over an unreadable key file answered {recovered:?}");
    };
    let recovered = recovery_refusal(recovered);
    assert!(
        matches!(recovered, RecoveryError::Unreadable(_)),
        "Recover over an unreadable key file refused {recovered:?}"
    );
    assert!(file.is_dir(), "no route replaces a key file it cannot read");
    server.abort();
}

#[tokio::test]
async fn a_machine_holding_its_key_answers_inspect_and_generate_without_reading_the_cluster() {
    let cluster = FakeCluster::start();
    let peer = Peer::participating("ployzd-backfill-held-unread", &cluster.store()).await;
    // With no key published, this marker decides, and it is not a key: any
    // read of the Cluster's Sealing fails.
    cluster.write_sealing_awaits_recovery("not a key");

    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::Held {
            public_key: peer.held()
        }),
        "Inspect on a Machine holding its key reads no Cluster row"
    );
    let generated = peer
        .local
        .generate_sealing_key(GenerateSealingKeyRequest {})
        .await;
    assert!(
        matches!(&generated, Ok(held) if held.public_key == peer.held()),
        "Generate on a Machine holding its key answered {generated:?}"
    );
}

#[tokio::test]
async fn a_confirmation_names_no_key_for_an_unusable_key_file() {
    let (replicated, server) = fake_cluster::store().await;
    let peer = Peer::needing_access("ployzd-backfill-unusable-confirmation", &replicated).await;
    make_unusable(&peer);

    assert_eq!(
        peer.local.held_sealing(LocalMachinePhase::Participating),
        None
    );
    server.abort();
}

#[tokio::test]
async fn install_refuses_to_publish_a_held_key_while_the_cluster_awaits_another() {
    let cluster = FakeCluster::start();
    let awaited = SealingKey::generate().public_key();
    cluster.write_sealing_awaits_recovery(&awaited.to_string());
    let peer = Peer::participating("ployzd-backfill-held-awaits-another", &cluster.store()).await;
    let held = peer.held();
    let sealed = peer.share_to(&peer).await;

    let installed = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: peer.machine.id,
            sealed,
            public_key: held,
        })
        .await;

    assert!(
        matches!(
            &installed,
            Err(LocalMachineError::Sealing(SealingError::AwaitsRecovery(key))) if *key == awaited
        ),
        "Install of held key {held} while this Machine reads its Cluster awaiting {awaited} \
         answered {installed:?}",
    );
    assert_eq!(cluster.store().sealing_public_key().await.unwrap(), None);
    assert_eq!(peer.held(), held);
    assert_eq!(
        sealing(&peer.local).await,
        Some(Sealing::Held { public_key: held })
    );
}

#[tokio::test]
async fn install_publishes_a_held_key_the_cluster_awaits() {
    let cluster = FakeCluster::start();
    let peer = Peer::participating("ployzd-backfill-held-awaited", &cluster.store()).await;
    let held = peer.held();
    cluster.write_sealing_awaits_recovery(&held.to_string());
    let sealed = peer.share_to(&peer).await;

    let installed = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: peer.machine.id,
            sealed,
            public_key: held,
        })
        .await
        .unwrap();

    assert_eq!(installed.public_key, held);
    assert_eq!(
        cluster.store().sealing_public_key().await.unwrap(),
        Some(held)
    );
}

#[tokio::test]
async fn install_publishes_a_held_key_over_a_malformed_marker() {
    let cluster = FakeCluster::start();
    let peer = Peer::participating("ployzd-backfill-held-malformed", &cluster.store()).await;
    let held = peer.held();
    // With no key published, this marker decides, and it is not a key: any
    // read of the Cluster's Sealing fails.
    cluster.write_sealing_awaits_recovery("not a key");
    let sealed = peer.share_to(&peer).await;

    let installed = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: peer.machine.id,
            sealed,
            public_key: held,
        })
        .await
        .unwrap();

    assert_eq!(installed.public_key, held);
    assert_eq!(
        cluster.store().sealing_public_key().await.unwrap(),
        Some(held)
    );
}

#[tokio::test]
async fn install_answers_a_held_key_beside_another_published_key() {
    let (replicated, server) = fake_cluster::store().await;
    let peer = Peer::participating("ployzd-backfill-held-beside-another", &replicated).await;
    let held = peer.held();
    let elsewhere = SealingKey::generate().public_key();
    replicated
        .publish_sealing_public_key(elsewhere)
        .await
        .unwrap();
    let sealed = peer.share_to(&peer).await;

    // This pins main's answer. Whether Install should refuse a held key the
    // Cluster does not publish is a later question.
    let installed = peer
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: peer.machine.id,
            sealed,
            public_key: held,
        })
        .await
        .unwrap();

    assert_eq!(installed.public_key, held);
    assert_eq!(
        replicated.sealing_public_key().await.unwrap(),
        Some(elsewhere)
    );
    server.abort();
}

#[tokio::test]
async fn a_keyless_machine_beside_a_holder_of_another_key_awaits_recovery_and_refuses_that_key() {
    let cluster = FakeCluster::start();
    let awaited = SealingKey::generate().public_key();
    cluster.write_sealing_awaits_recovery(&awaited.to_string());
    let holder = Peer::participating("ployzd-backfill-other-holder", &cluster.store()).await;
    let keyless = Peer::needing_access("ployzd-backfill-keyless-beside", &cluster.store()).await;
    let held = holder.held();
    // The held arm of Install on the holder itself inserts what its
    // publisher's Held publish would.
    let own = holder.share_to(&holder).await;
    let _ = holder
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed: own,
            public_key: held,
        })
        .await;

    assert_eq!(
        sealing(&keyless.local).await,
        Some(Sealing::AwaitsRecovery {
            public_key: awaited
        })
    );
    let sealed = holder.share_to(&keyless).await;
    let installed = keyless
        .local
        .install_sealing_key(InstallSealingKeyRequest {
            from: holder.machine.id,
            sealed,
            public_key: held,
        })
        .await;
    assert!(
        matches!(
            &installed,
            Err(LocalMachineError::Sealing(SealingError::AwaitsRecovery(key))) if *key == awaited
        ),
        "Install of {held} on a Machine reading its Cluster awaiting {awaited} answered {installed:?}",
    );
    assert_eq!(keyless.key(), None);
    assert_eq!(cluster.store().sealing_public_key().await.unwrap(), None);
}
