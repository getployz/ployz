//! Management Client slots and the client keys they admit, as admitted mutations.

use ployz_core::{
    DeviceCode, DeviceSet, LocalMachinePhase, ManagementCapability, ManagementScope,
    SetDeviceRequest, SetManagementClientRequest, SetManagementClientResponse,
};

use super::{Error, LocalMachine};

impl LocalMachine {
    /// Set or clear one labelled Management Client slot under mutation admission.
    ///
    /// `Set` stages a fresh client public key in the slot and returns its Management
    /// Capability. The slot's accepted key remains usable through read-only replacement
    /// verification. Its first operational RPC activates the replacement after the caller
    /// saves it. `Clear` turns the slot into a Cleared tombstone of its public keys;
    /// record publication revokes only that slot's live connections. The client secret is
    /// never persisted.
    ///
    /// # Errors
    /// Returns [`Error::DeviceSlot`] for a `device-` label, which only [`Self::set_device`]
    /// writes, [`Error::OnlyCloudManagesSlots`] when a management client other than
    /// Cloud's calls it, [`Error::NotParticipating`] when this Machine is not participating,
    /// admission failures, [`Error::RecordOwner`] when the record owner has stopped,
    /// and [`Error::Store`] when the record cannot be written.
    pub async fn set_management_client(
        &self,
        request: SetManagementClientRequest,
    ) -> Result<SetManagementClientResponse, Error> {
        let (SetManagementClientRequest::Set { label }
        | SetManagementClientRequest::Clear { label }) = &request;
        if DeviceCode::reserves(label) {
            return Err(Error::DeviceSlot);
        }
        let local = self.clone();
        let client = self.management_client;
        self.finish_mutation(async move {
            local
                .owner
                .mutate(move |store| {
                    if store.record().phase() != LocalMachinePhase::Participating {
                        return Err(Error::NotParticipating);
                    }
                    LocalMachine::require_slot_authority(client, store.record())?;
                    let capability = match request {
                        SetManagementClientRequest::Set { label } => {
                            let client = iroh::SecretKey::generate();
                            store.stage_management_client(label, *client.public().as_bytes())?;
                            Some(ManagementCapability::new(
                                store.record().management_secret().public_key(),
                                client.to_bytes(),
                            ))
                        }
                        SetManagementClientRequest::Clear { label } => {
                            store.clear_management_client(&label)?;
                            None
                        }
                    };
                    Ok(SetManagementClientResponse { capability })
                })
                .await?
        })
        .await
    }

    /// Approve or remove one Device on this Machine under mutation admission.
    ///
    /// The caller must still be Full when the change commits, so a Device removed while
    /// its own request waited for admission changes nothing.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] when the caller holds no Device slot,
    /// [`Error::NotParticipating`] when this Machine is not participating, admission
    /// failures, [`Error::RecordOwner`] when the record owner has stopped, and
    /// [`Error::Store`] when the record cannot be written.
    pub async fn set_device(&self, request: SetDeviceRequest) -> Result<DeviceSet, Error> {
        let local = self.clone();
        let client = self.management_client;
        self.finish_mutation(async move {
            local
                .owner
                .mutate(move |store| {
                    if store.record().phase() != LocalMachinePhase::Participating {
                        return Err(Error::NotParticipating);
                    }
                    if client.is_some_and(|remote| {
                        store.record().management_scope(&remote) != Some(ManagementScope::Full)
                    }) {
                        return Err(Error::NeedsFullScope);
                    }
                    Ok(match request {
                        SetDeviceRequest::Approve { key } => {
                            let code = key.code();
                            if store.approve_device(key)? {
                                DeviceSet::Approved { code }
                            } else {
                                DeviceSet::AlreadyApproved { code }
                            }
                        }
                        SetDeviceRequest::Remove { code } => {
                            if store.remove_device(code)? {
                                DeviceSet::Removed { code }
                            } else {
                                DeviceSet::NotPresent { code }
                            }
                        }
                    })
                })
                .await?
        })
        .await
    }

    /// Activate a staged management key for an authenticated operational RPC.
    ///
    /// # Errors
    /// Returns mutation admission, record owner or persistence errors.
    pub async fn activate_management_client(&self, remote: [u8; 32]) -> Result<(), Error> {
        if self.record().pending_label(&remote).is_none() {
            return Ok(());
        }
        let local = self.clone();
        self.finish_mutation(async move {
            local
                .owner
                .mutate(move |store| {
                    store
                        .activate_management_client(remote)
                        .map_err(Error::from)
                })
                .await?
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{LocalMachineStore, RecordOwner};

    fn cloud() -> ployz_core::ManagementClientLabel {
        ployz_core::ManagementClientLabel::parse("cloud").unwrap()
    }

    fn set() -> SetManagementClientRequest {
        SetManagementClientRequest::Set { label: cloud() }
    }

    fn clear() -> SetManagementClientRequest {
        SetManagementClientRequest::Clear { label: cloud() }
    }

    async fn participating(dir: &std::path::Path) -> LocalMachine {
        let store = LocalMachineStore::open(dir).unwrap();
        let local = LocalMachine::new(RecordOwner::spawn(store).unwrap());
        local
            .enroll(ployz_core::EnrollRequest {
                role: ployz_core::EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: ployz_core::MachineName::parse("first").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                        "192.0.2.1:51820".parse().unwrap(),
                    )],
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .await
            .unwrap();
        local
    }

    #[tokio::test]
    async fn clear_rejects_management_mutations_already_waiting_for_admission() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let capability = local
            .set_management_client(set())
            .await
            .unwrap()
            .capability
            .unwrap();
        let key = *iroh::SecretKey::from_bytes(capability.client_secret())
            .public()
            .as_bytes();
        local.activate_management_client(key).await.unwrap();
        let remote = local.clone().with_management_client(key);
        let lock = local.owner.admission_lock().write_owned().await;
        let clear = local.set_management_client(clear());
        let queued = remote.set_management_client(set());
        tokio::pin!(clear, queued);
        assert!(futures_util::poll!(&mut clear).is_pending());
        assert!(futures_util::poll!(&mut queued).is_pending());
        drop(lock);
        clear.await.unwrap();
        assert!(
            queued.await.is_err(),
            "a revoked client must not mutate after Clear"
        );
        assert!(!local.record().has_management_clients());
    }

    async fn accepted(local: &LocalMachine, label: &str) -> [u8; 32] {
        let label = ployz_core::ManagementClientLabel::parse(label).unwrap();
        let capability = local
            .set_management_client(SetManagementClientRequest::Set { label })
            .await
            .unwrap()
            .capability
            .unwrap();
        let key = *iroh::SecretKey::from_bytes(capability.client_secret())
            .public()
            .as_bytes();
        local.activate_management_client(key).await.unwrap();
        key
    }

    #[tokio::test]
    async fn only_the_cloud_slot_may_set_or_clear_slots() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let cloud_key = accepted(&local, "cloud").await;
        let device_key = accepted(&local, "cli-device").await;
        let device = local.clone().with_management_client(device_key);
        let before = local
            .record()
            .management_clients()
            .cloned()
            .collect::<Vec<_>>();

        for request in [
            set(),
            clear(),
            SetManagementClientRequest::Set {
                label: ployz_core::ManagementClientLabel::parse("cli-other").unwrap(),
            },
        ] {
            assert!(
                matches!(
                    device.set_management_client(request).await,
                    Err(Error::OnlyCloudManagesSlots)
                ),
                "a cli- slot changed slots"
            );
        }
        let record = local.record();
        assert_eq!(
            record.management_clients().cloned().collect::<Vec<_>>(),
            before
        );
        assert_eq!(record.accepted_client(&cloud()), Some(cloud_key));
        assert_eq!(record.pending_client(&cloud()), None);

        let manager = local.clone().with_management_client(cloud_key);
        let rotated = manager.set_management_client(set()).await.unwrap();
        assert!(rotated.capability.is_some());
        assert!(local.record().pending_client(&cloud()).is_some());
        manager
            .set_management_client(SetManagementClientRequest::Clear {
                label: ployz_core::ManagementClientLabel::parse("cli-device").unwrap(),
            })
            .await
            .unwrap();
        assert!(!local.record().accepts_management_client(&device_key));
    }

    #[tokio::test]
    async fn set_returns_a_capability_and_persists_only_the_public_key() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let response = local.set_management_client(set()).await.unwrap();
        let capability = response.capability.unwrap();
        let record = local.record();
        assert_eq!(
            *capability.machine(),
            record.management_secret().public_key()
        );
        let client_public = iroh::SecretKey::from_bytes(capability.client_secret()).public();
        assert_eq!(
            record.pending_client(&cloud()),
            Some(*client_public.as_bytes())
        );
        assert_eq!(record.accepted_client(&cloud()), None);
        local
            .activate_management_client(*client_public.as_bytes())
            .await
            .unwrap();
        let persisted = std::fs::read_to_string(dir.path().join("machine.json")).unwrap();
        assert!(!persisted.contains(&capability.to_secret_string()));
        assert!(!persisted.contains(&serde_json::to_string(capability.client_secret()).unwrap()));

        // A lost response leaves the accepted key usable until the replacement proves possession.
        let again = local.set_management_client(set()).await.unwrap();
        let replacement = again.capability.unwrap();
        assert_ne!(replacement, capability);
        assert_eq!(
            local.record().accepted_client(&cloud()),
            Some(*client_public.as_bytes())
        );
        let replacement_public = iroh::SecretKey::from_bytes(replacement.client_secret()).public();
        // Restart between issuance and handover: both public keys survive, no secret does.
        drop(local);
        let local = LocalMachine::new(
            RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap(),
        );
        local
            .activate_management_client(*replacement_public.as_bytes())
            .await
            .unwrap();
        assert_eq!(
            local.record().accepted_client(&cloud()),
            Some(*replacement_public.as_bytes())
        );
        assert_eq!(local.record().pending_client(&cloud()), None);
    }

    #[tokio::test]
    async fn failed_activation_preserves_the_accepted_and_pending_keys() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let first = local
            .set_management_client(set())
            .await
            .unwrap()
            .capability
            .unwrap();
        let first_key = *iroh::SecretKey::from_bytes(first.client_secret())
            .public()
            .as_bytes();
        local.activate_management_client(first_key).await.unwrap();
        let next = local
            .set_management_client(set())
            .await
            .unwrap()
            .capability
            .unwrap();
        let next_key = *iroh::SecretKey::from_bytes(next.client_secret())
            .public()
            .as_bytes();
        let before = local.record();
        // An occupied destination makes the atomic rename fail, even when tests run as root.
        let path = dir.path().join("machine.json");
        let backup = dir.path().join("saved-machine.json");
        std::fs::rename(&path, &backup).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(matches!(
            local.activate_management_client(next_key).await,
            Err(Error::Store(_))
        ));
        assert_eq!(local.record(), before);
        assert_eq!(before.accepted_client(&cloud()), Some(first_key));
        assert_eq!(before.pending_client(&cloud()), Some(next_key));
        std::fs::remove_dir(&path).unwrap();
        std::fs::rename(backup, path).unwrap();
        local.activate_management_client(next_key).await.unwrap();
        assert_eq!(local.record().accepted_client(&cloud()), Some(next_key));
        assert_eq!(local.record().pending_client(&cloud()), None);
    }

    #[tokio::test]
    async fn clear_tombstones_accepted_and_pending_keys_in_one_write() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        local.set_management_client(set()).await.unwrap();
        let mut records = local.owner().watch();
        records.mark_unchanged();
        let response = local.set_management_client(clear()).await.unwrap();
        assert_eq!(response, SetManagementClientResponse { capability: None });
        assert!(records.has_changed().unwrap());
        let record = records.borrow_and_update().clone();
        assert!(!record.has_management_clients());
        assert_eq!(record.accepted_client(&cloud()), None);
        assert_eq!(record.pending_client(&cloud()), None);
        // One publication means one write: the keys were not cleared separately.
        assert!(!records.has_changed().unwrap());
    }

    fn device() -> (iroh::SecretKey, ployz_core::DeviceKey) {
        let secret = iroh::SecretKey::generate();
        let key = ployz_core::DeviceKey::from_bytes(*secret.public().as_bytes());
        (secret, key)
    }

    #[tokio::test]
    async fn a_device_slot_is_full_only_when_set_device_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let (_, key) = device();
        assert_eq!(
            local
                .set_device(SetDeviceRequest::Approve { key })
                .await
                .unwrap(),
            DeviceSet::Approved { code: key.code() }
        );
        assert_eq!(
            local
                .set_device(SetDeviceRequest::Approve { key })
                .await
                .unwrap(),
            DeviceSet::AlreadyApproved { code: key.code() }
        );
        let record = local.record();
        assert_eq!(record.accepted_client(&key.label()), Some(*key.as_bytes()));
        assert_eq!(
            record.management_scope(key.as_bytes()),
            Some(ManagementScope::Full)
        );

        let cloud_key = accepted(&local, "cloud").await;
        let cli_key = accepted(&local, "cli-laptop").await;
        for client in [cloud_key, cli_key] {
            assert_eq!(
                local.record().management_scope(&client),
                Some(ManagementScope::Operator)
            );
            let caller = local.clone().with_management_client(client);
            assert!(matches!(
                caller
                    .set_device(SetDeviceRequest::Remove { code: key.code() })
                    .await,
                Err(Error::NeedsFullScope)
            ));
        }
        assert_eq!(
            local.record().accepted_client(&key.label()),
            Some(*key.as_bytes())
        );
    }

    #[tokio::test]
    async fn no_caller_writes_a_device_slot_through_set_management_client() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let (_, key) = device();
        local
            .set_device(SetDeviceRequest::Approve { key })
            .await
            .unwrap();
        let cloud = local
            .clone()
            .with_management_client(accepted(&local, "cloud").await);
        let before = local.record();
        for caller in [&local, &cloud] {
            for request in [
                SetManagementClientRequest::Set { label: key.label() },
                SetManagementClientRequest::Clear { label: key.label() },
                SetManagementClientRequest::Set {
                    label: ployz_core::ManagementClientLabel::parse("device-new").unwrap(),
                },
            ] {
                assert!(matches!(
                    caller.set_management_client(request).await,
                    Err(Error::DeviceSlot)
                ));
            }
        }
        assert_eq!(local.record(), before);
    }

    #[tokio::test]
    async fn remove_tombstones_the_slot_and_reports_a_rerun_as_not_present() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let (_, key) = device();
        local
            .set_device(SetDeviceRequest::Approve { key })
            .await
            .unwrap();
        let remove = SetDeviceRequest::Remove { code: key.code() };
        assert_eq!(
            local.set_device(remove.clone()).await.unwrap(),
            DeviceSet::Removed { code: key.code() }
        );
        assert!(!local.record().accepts_management_client(key.as_bytes()));
        assert_eq!(
            local.set_device(remove).await.unwrap(),
            DeviceSet::NotPresent { code: key.code() }
        );
        assert_eq!(
            local
                .set_device(SetDeviceRequest::Approve { key })
                .await
                .unwrap(),
            DeviceSet::Approved { code: key.code() }
        );
    }

    #[tokio::test]
    async fn a_device_removed_while_its_request_waits_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let (_, first) = device();
        let (_, second) = device();
        local
            .set_device(SetDeviceRequest::Approve { key: first })
            .await
            .unwrap();
        let caller = local.clone().with_management_client(*first.as_bytes());
        let lock = local.owner.admission_lock().write_owned().await;
        let remove = local.set_device(SetDeviceRequest::Remove { code: first.code() });
        let queued = caller.set_device(SetDeviceRequest::Approve { key: second });
        tokio::pin!(remove, queued);
        assert!(futures_util::poll!(&mut remove).is_pending());
        assert!(futures_util::poll!(&mut queued).is_pending());
        drop(lock);
        remove.await.unwrap();
        assert!(matches!(queued.await, Err(Error::ManagementRevoked)));
        assert!(!local.record().accepts_management_client(second.as_bytes()));
    }
}
