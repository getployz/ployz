//! What the daemon's planes share: when they may start, and what they do without a Cluster.

use std::{future::Future, io, sync::Arc};

use ployz_core::LocalMachinePhase;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    corrosion::ClusterLink,
    docker::ContainerRuntime,
    machine::{LocalMachine, LocalMachineRecord, RecordOwner},
};

/// Whether this Machine has caught up to the Cluster: planes that act on Cluster rows wait
/// for it, and give up when the daemon shuts down.
#[derive(Clone)]
pub(super) struct Participation {
    records: watch::Receiver<Arc<LocalMachineRecord>>,
    shutdown: CancellationToken,
}

impl Participation {
    pub(super) fn new(
        records: watch::Receiver<Arc<LocalMachineRecord>>,
        shutdown: CancellationToken,
    ) -> Self {
        Self { records, shutdown }
    }

    /// Wait until the published record is participating.
    ///
    /// `Ok(false)` when shutdown wins or the record owner stopped; the daemon shuts
    /// down on the latter, so the gated plane simply never starts.
    pub(super) async fn wait(&self) -> io::Result<bool> {
        let mut records = self.records.clone();
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => Ok(false),
            changed = records.wait_for(|record| record.phase() == LocalMachinePhase::Participating) => {
                Ok(changed.is_ok() && !self.shutdown.is_cancelled())
            }
        }
    }
}

/// A plane this Machine may lack: `make` runs it on `input` when there is one, and
/// without one the plane idles until the daemon shuts down. `make`, and whatever it
/// holds, is dropped before that wait.
pub(super) async fn optional_plane<T, Plane>(
    input: Option<T>,
    shutdown: &CancellationToken,
    make: impl FnOnce(T) -> Plane,
) -> io::Result<()>
where
    Plane: Future<Output = io::Result<()>>,
{
    match input {
        Some(input) => make(input).await,
        None => {
            drop(make);
            shutdown.cancelled().await;
            Ok(())
        }
    }
}

/// The Machine every serving plane is cloned from: Docker, the volume plugin and the
/// Cluster link applied once, so its clones share what a Machine keeps in memory, such as
/// the creations that settled.
pub(super) fn serving_local(
    owner: RecordOwner,
    containers: Option<ContainerRuntime>,
    plugin: crate::storage::Plugin,
    link: Option<&ClusterLink>,
) -> LocalMachine {
    LocalMachine::new(owner)
        .with_containers(containers)
        .with_plugin(plugin)
        .with_cluster_context(link.map(|link| link.context.clone()))
}

#[cfg(test)]
mod tests {
    use tokio_util::sync::CancellationToken;

    use super::{Participation, optional_plane, serving_local};
    use crate::{machine_api::MachineApi, test_dir::TestDir};

    fn gate_owner(name: &str) -> (TestDir, crate::machine::RecordOwner) {
        let root = TestDir::new(name);
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(root.0.join("data")).unwrap(),
        )
        .unwrap();
        (root, owner)
    }

    async fn participate(owner: &crate::machine::RecordOwner) {
        owner
            .mutate(|store| {
                store.enroll(ployz_core::EnrollRequest {
                    role: ployz_core::EnrollAs::Founder {
                        initial_policy: Default::default(),
                        name: ployz_core::MachineName::parse("gate").unwrap(),
                        cluster_network: "10.210.0.0/16".parse().unwrap(),
                        public_ip: None,
                        advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                            "192.0.2.1:51820".parse().unwrap(),
                        )],
                        grant: None,
                    },
                    wireguard_mtu: None,
                    pinned: Default::default(),
                })
            })
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn participation_gate_waits_for_catch_up_and_obeys_shutdown() {
        let (_root, owner) = gate_owner("ployzd-participation-gate");
        let shutdown = CancellationToken::new();
        let participation = Participation::new(owner.watch(), shutdown);
        let waiting = participation.wait();
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        participate(&owner).await;
        assert!(waiting.await.unwrap());

        let records = owner.watch();
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        drop(owner);
        assert!(!Participation::new(records, shutdown).wait().await.unwrap());
    }

    #[tokio::test]
    async fn participation_gate_prefers_shutdown_over_a_participating_record() {
        let (_root, owner) = gate_owner("ployzd-participation-gate-shutdown");
        participate(&owner).await;
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        assert!(
            !Participation::new(owner.watch(), shutdown)
                .wait()
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn participation_gate_answers_at_once_for_a_participating_record() {
        let (_root, owner) = gate_owner("ployzd-participation-gate-now");
        participate(&owner).await;
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Participation::new(owner.watch(), CancellationToken::new()).wait(),
        )
        .await
        .expect("a participating record needs no wait");
        assert!(answer.unwrap());
    }

    #[tokio::test]
    async fn participation_gate_ends_false_when_shutdown_arrives_while_waiting() {
        let (_root, owner) = gate_owner("ployzd-participation-gate-cancel");
        let shutdown = CancellationToken::new();
        let waiting = tokio::spawn({
            let participation = Participation::new(owner.watch(), shutdown.clone());
            async move { participation.wait().await }
        });
        tokio::task::yield_now().await;
        shutdown.cancel();
        let answer = tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
            .await
            .expect("shutdown ends the wait")
            .unwrap();
        assert!(!answer.unwrap());
    }

    #[tokio::test]
    async fn participation_gate_ends_false_when_the_record_owner_stops() {
        let (_root, owner) = gate_owner("ployzd-participation-gate-owner");
        let records = owner.watch();
        drop(owner);
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            Participation::new(records, CancellationToken::new()).wait(),
        )
        .await
        .expect("a stopped record owner ends the wait");
        assert!(!answer.unwrap());
    }

    #[tokio::test]
    async fn every_plane_serves_through_one_machine_that_keeps_settled_creations_once() {
        let root = TestDir::new("ployzd-serving-local");
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(root.0.join("data")).unwrap(),
        )
        .unwrap();
        let serving = serving_local(owner.clone(), None, crate::storage::Plugin::default(), None);

        let management = serving.clone();
        let boot_pass = serving.clone();
        let machine_api = MachineApi::builder(owner)
            .with_local(serving.clone())
            .build();

        assert!(management.shares_settled_creations_with(&boot_pass));
        assert!(management.shares_settled_creations_with(&machine_api.local_machine()));
        assert!(serving.shares_settled_creations_with(&machine_api.local_machine()));
    }

    #[tokio::test]
    async fn a_plane_without_a_link_idles_until_shutdown_and_then_ends_well() {
        let shutdown = CancellationToken::new();
        let idle = optional_plane(None::<u8>, &shutdown, |_| async {
            panic!("a plane with no input has nothing to run");
        });
        tokio::pin!(idle);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut idle)
                .await
                .is_err()
        );
        shutdown.cancel();
        idle.await.unwrap();
    }

    #[tokio::test]
    async fn a_plane_with_a_link_runs_on_it_and_answers_with_its_result() {
        let shutdown = CancellationToken::new();
        optional_plane(Some(7_u8), &shutdown, |input| async move {
            assert_eq!(input, 7);
            Ok(())
        })
        .await
        .unwrap();
        let failed = optional_plane(Some(()), &shutdown, |()| async {
            Err(std::io::Error::other("the plane failed"))
        })
        .await
        .unwrap_err();
        assert_eq!(failed.to_string(), "the plane failed");
        assert!(!shutdown.is_cancelled());
    }

    #[tokio::test]
    async fn a_machine_that_cannot_place_what_it_held_warns_of_each_and_carries_on() {
        let root = TestDir::new("ployzd-restore-held");
        let owner = crate::machine::RecordOwner::spawn(
            crate::machine::LocalMachineStore::open(root.0.join("data")).unwrap(),
        )
        .unwrap();
        let (capture, _logging) = crate::diag::test_support::capture();

        serving_local(owner, None, crate::storage::Plugin::default(), None)
            .restore_held_at_boot()
            .await;

        let log = capture.text();
        let router = log
            .find("could not place the Private Access router this Machine held again; it stays as it is until the next `ployz private-access on`")
            .expect("the router warning");
        let doors = log
            .find("could not place the Ingress doors this Machine held again; they stay as they are until the next `ployz ingress use`")
            .expect("the doors warning");
        assert!(
            router < doors,
            "the router is placed before the doors: {log}"
        );
    }
}
