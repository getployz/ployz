//! Local ownership of Machine upgrade replay and admission.

use ployz_core::{
    MachineUpgradeAttempt, ManagementScope, RequestMachineUpgradeRequest, Sealing,
    authority::{AuthorityRefusal, Purpose, Timestamp, upgrade},
};

use super::{Error, LocalMachine};
use crate::installer::upgrade::ReleaseChoice;

impl LocalMachine {
    /// Resolve, durably accept, and launch one retry-safe Machine upgrade.
    ///
    /// A replay is read before claiming exclusive mutation ownership so a lost reply can be
    /// recovered while the independent worker owns the installation lock.
    ///
    /// `cloud_routed` marks a request that entered the mesh through Ployz Cloud on any
    /// Machine; such a request, like one on this Machine's own tunnel, cannot downgrade.
    ///
    /// Under Device enforcement an Operator's request must carry the Release Channel a
    /// person approved for this Cluster, at a generation no lower than the highest this
    /// Machine has accepted. That generation is recorded, synced, before anything else
    /// decides, and the request may name only that channel or what it resolves to now.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UpgradeChannelRefused`] for an Operator under enforcement whose
    /// channel statement is missing, untrusted, for another Cluster or superseded,
    /// [`Error::ApprovalRecord`] when its generation cannot be recorded, and
    /// [`Error::Upgrade`] for a conflicting retry identity, active mutation, release
    /// failure, a release off the approved channel, receipt failure, or worker launch
    /// uncertainty.
    pub(crate) async fn request_upgrade(
        &self,
        request: RequestMachineUpgradeRequest,
        cloud_routed: bool,
        scope: ManagementScope,
    ) -> Result<MachineUpgradeAttempt, Error> {
        let data_dir = self.owner.data_dir().to_owned();
        let run_dir = self.owner.run_dir().to_owned();
        if let Some(attempt) =
            crate::installer::upgrade::replay(&request, &data_dir, &run_dir).await?
        {
            return Ok(attempt);
        }
        let _local = self
            .owner
            .admission_lock()
            .try_write_owned()
            .map_err(|_| crate::mutation::Error::Busy)?;
        self.require_management_access()?;
        let choice = self.release_choice(&request, scope, Timestamp::now())?;
        let installation = self.owner.mutation_gate().try_installation()?;
        crate::installer::require_standard_machine_paths(&data_dir, &run_dir.join("ployz.sock"))
            .map_err(crate::installer::upgrade::Error::NonstandardPaths)?;
        crate::installer::upgrade::request(
            request,
            self.downgrade(cloud_routed),
            choice,
            data_dir,
            run_dir,
            installation,
        )
        .await
        .map_err(Into::into)
    }

    /// The releases `scope` may name here. An Operator under enforcement gets only the
    /// channel its verified statement approves, whose generation is recorded first.
    fn release_choice(
        &self,
        request: &RequestMachineUpgradeRequest,
        scope: ManagementScope,
        now: Timestamp,
    ) -> Result<ReleaseChoice, Error> {
        let record = self.record();
        match scope {
            ManagementScope::Full => return Ok(ReleaseChoice::AnyRelease),
            ManagementScope::Operator if !record.device_enforcement() => {
                return Ok(ReleaseChoice::ChannelResolved);
            }
            ManagementScope::Operator => {}
        }
        let signed = request
            .channel
            .as_ref()
            .ok_or(Error::UpgradeChannelRefused(AuthorityRefusal::Unsigned {
                purpose: Purpose::Rule,
            }))?;
        let Some(Sealing::Held { public_key }) = self.held_sealing(record.phase()) else {
            return Err(Error::UpgradeChannelRefused(
                AuthorityRefusal::NoClusterIdentity,
            ));
        };
        let approved = upgrade::verify(signed, &record.trusted_keys(), public_key, now)
            .map_err(Error::UpgradeChannelRefused)?;
        self.approvals.mark_upgrade_channel(&approved)?;
        tracing::info!(
            channel = %approved.channel,
            generation = approved.generation,
            "admitting an upgrade along the approved Release Channel"
        );
        Ok(ReleaseChoice::Signed(approved.channel))
    }

    fn downgrade(&self, cloud_routed: bool) -> crate::installer::upgrade::Downgrade {
        if cloud_routed || self.management_client.is_some() {
            crate::installer::upgrade::Downgrade::Refused
        } else {
            crate::installer::upgrade::Downgrade::Allowed
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        DeviceKey, MachineRelease, MachineUpgradeAttemptId, ReleaseChannel,
        authority::{Signed, Signer, SignerKey, UpgradeChannel},
    };

    use super::*;
    use crate::{
        installer::upgrade::Downgrade,
        machine::{LocalMachineStore, RecordOwner},
        sealing_key::SealingKey,
    };

    #[test]
    fn only_a_request_that_never_touched_cloud_may_downgrade() {
        let dir = tempfile::tempdir().unwrap();
        let local = LocalMachine::new(
            RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap(),
        );
        let tunnel = local
            .clone()
            .with_management_client(ployz_core::ClientKey::from_bytes([7; 32]));

        assert_eq!(local.downgrade(false), Downgrade::Allowed);
        assert_eq!(local.downgrade(true), Downgrade::Refused);
        assert_eq!(tunnel.downgrade(false), Downgrade::Refused);
        assert_eq!(tunnel.downgrade(true), Downgrade::Refused);
    }

    fn device() -> Signer {
        Signer::from_seed(&[7; 32])
    }

    /// A founder in `dir` that trusts [`device`], with enforcement as given.
    async fn founder(dir: &std::path::Path, enforced: bool) -> LocalMachine {
        let mut store = LocalMachineStore::open(dir).unwrap();
        store
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: ployz_core::MachineName::parse("local").unwrap(),
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
            .unwrap();
        let owner = RecordOwner::spawn(store).unwrap();
        owner
            .mutate(move |store| {
                store
                    .approve_device(DeviceKey::from(device().public()))
                    .unwrap();
                store.set_device_enforcement(enforced, Vec::new()).unwrap();
            })
            .await
            .unwrap();
        LocalMachine::new(owner)
    }

    fn request(channel: Option<Signed<UpgradeChannel>>) -> RequestMachineUpgradeRequest {
        RequestMachineUpgradeRequest {
            attempt_id: MachineUpgradeAttemptId::random(),
            release: MachineRelease::Stable,
            channel,
        }
    }

    fn approved(dir: &std::path::Path, generation: u64) -> Signed<UpgradeChannel> {
        approved_for(
            SealingKey::load(dir).unwrap().unwrap().public_key(),
            ReleaseChannel::Stable,
            generation,
        )
    }

    fn approved_for(
        cluster: ployz_core::SealingPublicKey,
        channel: ReleaseChannel,
        generation: u64,
    ) -> Signed<UpgradeChannel> {
        let statement = UpgradeChannel {
            cluster,
            channel,
            generation,
            signer: SignerKey::Device(DeviceKey::from(device().public())),
        };
        Signed::sign(&statement, &device()).unwrap()
    }

    fn choose(
        local: &LocalMachine,
        request: &RequestMachineUpgradeRequest,
    ) -> Result<ReleaseChoice, Error> {
        local.release_choice(request, ManagementScope::Operator, Timestamp::now())
    }

    #[tokio::test]
    async fn an_enforcing_machine_refuses_an_operator_upgrade_without_an_approved_channel() {
        let dir = tempfile::tempdir().unwrap();
        let local = founder(dir.path(), true).await;
        let refused = choose(&local, &request(None)).unwrap_err();
        assert!(
            matches!(
                refused,
                Error::UpgradeChannelRefused(AuthorityRefusal::Unsigned { .. })
            ),
            "{refused:?}"
        );
        assert!(
            refused
                .to_string()
                .starts_with(ployz_core::UPGRADE_CHANNEL_CAPABILITY)
        );
        assert_eq!(
            local
                .release_choice(&request(None), ManagementScope::Full, Timestamp::now())
                .unwrap(),
            ReleaseChoice::AnyRelease
        );
    }

    #[tokio::test]
    async fn an_older_generation_is_refused_after_a_newer_one_also_after_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let local = founder(dir.path(), true).await;
        assert_eq!(
            choose(&local, &request(Some(approved(dir.path(), 2)))).unwrap(),
            ReleaseChoice::Signed(ReleaseChannel::Stable)
        );
        assert_eq!(
            choose(&local, &request(Some(approved(dir.path(), 2)))).unwrap(),
            ReleaseChoice::Signed(ReleaseChannel::Stable),
            "a resend of the same generation is admitted"
        );
        let generation = |result: Result<ReleaseChoice, Error>| {
            matches!(
                result,
                Err(Error::UpgradeChannelRefused(AuthorityRefusal::Mismatch {
                    field: "a generation"
                }))
            )
        };
        assert!(generation(choose(
            &local,
            &request(Some(approved(dir.path(), 1)))
        )));
        drop(local);

        let reloaded = LocalMachine::new(
            RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap(),
        );
        assert!(generation(choose(
            &reloaded,
            &request(Some(approved(dir.path(), 1)))
        )));
    }

    #[tokio::test]
    async fn an_equal_generation_naming_another_channel_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let local = founder(dir.path(), true).await;
        let cluster = SealingKey::load(dir.path()).unwrap().unwrap().public_key();
        choose(&local, &request(Some(approved(dir.path(), 2)))).unwrap();
        let refused = choose(
            &local,
            &request(Some(approved_for(cluster, ReleaseChannel::Beta, 2))),
        )
        .unwrap_err();
        assert!(
            matches!(
                refused,
                Error::UpgradeChannelRefused(AuthorityRefusal::Mismatch {
                    field: "a generation"
                })
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn another_clusters_approval_or_no_cluster_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let local = founder(dir.path(), true).await;
        let elsewhere = approved_for(
            ployz_core::SealingPublicKey::from_bytes([8; 32]),
            ReleaseChannel::Stable,
            1,
        );
        let refused = choose(&local, &request(Some(elsewhere))).unwrap_err();
        assert!(
            matches!(
                refused,
                Error::UpgradeChannelRefused(AuthorityRefusal::Mismatch { field: "a Cluster" })
            ),
            "{refused:?}"
        );

        let ours = approved(dir.path(), 1);
        std::fs::remove_file(dir.path().join("sealing.key")).unwrap();
        let refused = choose(&local, &request(Some(ours))).unwrap_err();
        assert!(
            matches!(
                refused,
                Error::UpgradeChannelRefused(AuthorityRefusal::NoClusterIdentity)
            ),
            "{refused:?}"
        );
    }

    #[tokio::test]
    async fn a_machine_not_enforcing_ignores_the_channel() {
        let dir = tempfile::tempdir().unwrap();
        let local = founder(dir.path(), false).await;
        let mut stranger = approved(dir.path(), 1);
        stranger.body = stranger.body.replace("stable", "beta");
        assert_eq!(
            choose(&local, &request(Some(stranger))).unwrap(),
            ReleaseChoice::ChannelResolved
        );
        assert_eq!(
            choose(&local, &request(None)).unwrap(),
            ReleaseChoice::ChannelResolved
        );
    }
}
