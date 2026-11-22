//! Management Client slots and the client keys they admit, as admitted mutations.

use ployz_core::{
    ClientKey, DeviceCode, DeviceKey, DeviceSet, KeyStepRequest, ManagementCapability,
    SetDeviceRequest, SetManagementClientRequest, SetManagementClientResponse,
    authority::{
        Approver, KeyFacts, KeyKind, KeyMember, KeyVerified, NewKey, PasskeyApprover, Timestamp,
        TrustedGrant, TrustedPasskey, key,
    },
};

use super::{Caller, Error, FullCaller, LocalMachine};
use crate::machine::LocalMachineRecord;

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
        let caller = self.call.caller();
        self.finish_mutation(move |local| async move {
            local
                .owner
                .mutate(move |store| {
                    store.record().require_participating()?;
                    LocalMachine::require_slot_authority(caller, store.record())?;
                    let capability = match request {
                        SetManagementClientRequest::Set { label } => {
                            let client = iroh::SecretKey::generate();
                            store.stage_management_client(
                                label,
                                ClientKey::from_bytes(*client.public().as_bytes()),
                            )?;
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

    /// Approve one Device on this Machine, or remove the key a code names, Device or passkey,
    /// under mutation admission. A removal goes through [`LocalMachineStore::remove_key`],
    /// so the key stays removed and the last key stays.
    ///
    /// The scope recheck runs on the Machine the caller dialed: a Device removed there
    /// while its own request waited for admission changes nothing. A routed call reaches
    /// its target with no management client, since forwarding carries scope rather than
    /// identity, so the target trusts the entry Machine's check.
    ///
    /// # Errors
    /// Returns [`Error::NeedsFullScope`] for an Operator or a caller that holds no Device
    /// slot, [`Error::DeviceIsPasskey`] when a trusted passkey has the Device's key,
    /// [`Error::NotParticipating`] when this Machine is not participating, admission
    /// failures, [`Error::RecordOwner`] when the record owner has stopped, and
    /// [`Error::Store`] for a removed key, the last key, or a record that cannot be written.
    ///
    /// [`LocalMachineStore::remove_key`]: crate::machine::LocalMachineStore::remove_key
    pub async fn set_device(&self, request: SetDeviceRequest) -> Result<DeviceSet, Error> {
        let full = self.full_caller()?;
        self.finish_mutation(move |local| async move {
            local
                .owner
                .mutate(move |store| {
                    store.record().require_participating()?;
                    let by = full.approver(store.record())?;
                    match request {
                        SetDeviceRequest::Approve { key } => {
                            trust(store, NewKey::Device { key }, by, Timestamp::now())
                        }
                        SetDeviceRequest::Remove { code } => {
                            let removed = store.remove_by_code(code)?;
                            Ok(removal(code, removed, by))
                        }
                        SetDeviceRequest::RemoveKey { key } => {
                            let removed = store.remove_key(key)?;
                            Ok(removal(key.code(), removed, by))
                        }
                    }
                })
                .await?
        })
        .await
    }

    /// Apply a person's signed change to the keys this Machine trusts, under mutation
    /// admission: a trust as a step of a bundle whose window holds on this Machine's floored
    /// clock and that names this Machine, a removal alone or in a bundle. Signature, window
    /// and removed keys are checked inside the record write, so a removal that lands first
    /// wins.
    ///
    /// # Errors
    /// Returns [`Error::KeyRefused`] for a step this Machine does not accept,
    /// [`Error::PasskeyIsDevice`], [`Error::DeviceIsPasskey`], [`Error::KeyHeld`],
    /// [`Error::KeyIsClient`] and [`Error::CredentialHeld`] for a key that collides with one
    /// held, [`Error::NotParticipating`], admission
    /// failures, [`Error::ApprovalRecord`] when the clock floor cannot be kept,
    /// [`Error::RecordOwner`], and [`Error::Store`] for a removed key, the last key, or a
    /// record that cannot be written.
    pub async fn key_step(&self, request: KeyStepRequest) -> Result<DeviceSet, Error> {
        self.finish_mutation(move |local| async move {
            let cluster = crate::sealing_key::SealingKey::load(local.owner.data_dir())
                .map_err(Error::JoinRecord)?
                .map(|key| key.public_key());
            let approvals = local.approvals.clone();
            local
                .owner
                .mutate(move |store| {
                    let record = store.record();
                    record.require_participating()?;
                    let now = approvals.observed_now(Timestamp::now())?;
                    let trusted = record.trusted_keys();
                    let facts = KeyFacts {
                        trusted: &trusted,
                        cluster,
                        member: KeyMember {
                            id: record.id(),
                            public_key: record.private_key().public_key(),
                        },
                        now,
                    };
                    let (change, by) =
                        key::verify(&request.step, &facts).map_err(Error::KeyRefused)?;
                    let by = match by {
                        Approver::Signing(key) => PasskeyApprover::Device { code: key.code() },
                        Approver::Passkey(_) => PasskeyApprover::Unknown,
                    };
                    match change {
                        KeyVerified::Trust { key, issued_at } => trust(store, key, by, issued_at),
                        KeyVerified::Remove { key } => {
                            let removed = store.remove_key(key)?;
                            Ok(removal(key.code(), removed, by))
                        }
                    }
                })
                .await?
        })
        .await
    }
}

impl FullCaller {
    /// This Machine's own view of a Full caller: the Device whose connection asked, a
    /// caller on this Server's local socket, or a call routed here over the mesh.
    fn approver(self, record: &LocalMachineRecord) -> Result<PasskeyApprover, Error> {
        match self.caller() {
            Caller::Management(remote) => record
                .devices()
                .find(|device| ClientKey::from(*device) == remote)
                .map(|device| PasskeyApprover::Device {
                    code: device.code(),
                })
                .ok_or(Error::NeedsFullScope),
            Caller::LocalSocket => Ok(PasskeyApprover::Server),
            Caller::Routed => Ok(PasskeyApprover::Routed),
        }
    }
}

impl LocalMachine {
    /// Activate a staged management key for an authenticated operational RPC.
    ///
    /// # Errors
    /// Returns mutation admission, record owner or persistence errors.
    pub async fn activate_management_client(&self, remote: ClientKey) -> Result<(), Error> {
        if self.record().pending_label(&remote).is_none() {
            return Ok(());
        }
        self.finish_mutation(move |local| async move {
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

/// The answer to a removal of the key `code` names, which `by` asked for.
fn removal(code: DeviceCode, removed: bool, by: PasskeyApprover) -> DeviceSet {
    if removed {
        tracing::info!("trusted key removed code={code} by={by}");
        DeviceSet::Removed { code }
    } else {
        DeviceSet::NotPresent { code }
    }
}

/// Trust `key` here, approved by `by` under a bundle issued at `issued_at`, refusing a key
/// that collides with one held by another kind or another passkey's credential, so a CI or
/// agent key never shares its key with a Device or passkey.
fn trust(
    store: &mut crate::machine::LocalMachineStore,
    key: NewKey,
    by: PasskeyApprover,
    issued_at: Timestamp,
) -> Result<DeviceSet, Error> {
    let record = store.record();
    let code = key.key().code();
    let granted = record
        .grants()
        .iter()
        .find(|held| held.grant.key.code() == code);
    let approved = |added: bool, kind: &str| {
        if added {
            tracing::info!("trusted key added kind={kind} code={code} by={by}");
            DeviceSet::Approved { code }
        } else {
            DeviceSet::AlreadyApproved { code }
        }
    };
    match key {
        NewKey::Device { key } => {
            if record
                .passkeys()
                .iter()
                .any(|held| held.passkey.code() == code)
            {
                return Err(Error::DeviceIsPasskey(code));
            }
            if let Some(held) = granted {
                return Err(Error::KeyHeld {
                    code,
                    held: held.grant.kind.into(),
                });
            }
            Ok(approved(store.approve_device(key)?, "device"))
        }
        NewKey::Passkey { passkey } => {
            if record.devices().any(|device| device.code() == code) {
                return Err(Error::PasskeyIsDevice(code));
            }
            if let Some(held) = granted {
                return Err(Error::KeyHeld {
                    code,
                    held: held.grant.kind.into(),
                });
            }
            if let Some(held) = record.passkeys().iter().find(|held| {
                held.passkey.credential_id == passkey.credential_id && held.passkey.code() != code
            }) {
                return Err(Error::CredentialHeld {
                    held: held.passkey.code(),
                });
            }
            Ok(approved(
                store.approve_passkey(TrustedPasskey {
                    passkey,
                    approved_by: by,
                })?,
                "passkey",
            ))
        }
        NewKey::Ci { key: signing, .. } | NewKey::Agent { key: signing, .. } => {
            // A Device slot the grant would shadow still counts: `devices()` already hides
            // it, so look at the slot itself.
            let client = ClientKey::from(DeviceKey::from(signing));
            if record.accepted_client(&code.label()) == Some(client) {
                return Err(Error::KeyHeld {
                    code,
                    held: KeyKind::FullCli,
                });
            }
            // A key any other slot holds keeps its connection, so it never becomes a grant
            // that removing it later would leave connected.
            if record.admits_management_client(&client) {
                return Err(Error::KeyIsClient(code));
            }
            if record
                .passkeys()
                .iter()
                .any(|held| held.passkey.code() == code)
            {
                return Err(Error::KeyHeld {
                    code,
                    held: KeyKind::Person,
                });
            }
            let grant = key
                .grant(issued_at)
                .expect("a CI or agent key carries a grant");
            let kind = KeyKind::from(grant.kind);
            Ok(approved(
                store.approve_grant(TrustedGrant {
                    grant,
                    approved_by: by,
                })?,
                &kind.to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{LocalMachineStore, RecordOwner, StoreError};
    use ployz_core::authority::{
        KeyChange, KeyRef, KeyStep, ProposalApproval, ProposalRef, ProposalStep, Signed, Signer,
        StepDigest, StepKind,
    };
    use ployz_core::{DeviceKey, ManagementScope};

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
                    grant: None,
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
        let key = ClientKey::from_bytes(
            *iroh::SecretKey::from_bytes(capability.client_secret())
                .public()
                .as_bytes(),
        );
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

    async fn accepted(local: &LocalMachine, label: &str) -> ClientKey {
        let label = ployz_core::ManagementClientLabel::parse(label).unwrap();
        let capability = local
            .set_management_client(SetManagementClientRequest::Set { label })
            .await
            .unwrap()
            .capability
            .unwrap();
        let key = ClientKey::from_bytes(
            *iroh::SecretKey::from_bytes(capability.client_secret())
                .public()
                .as_bytes(),
        );
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
            Some(ClientKey::from_bytes(*client_public.as_bytes()))
        );
        assert_eq!(record.accepted_client(&cloud()), None);
        local
            .activate_management_client(ClientKey::from_bytes(*client_public.as_bytes()))
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
            Some(ClientKey::from_bytes(*client_public.as_bytes()))
        );
        let replacement_public = iroh::SecretKey::from_bytes(replacement.client_secret()).public();
        // Restart between issuance and handover: both public keys survive, no secret does.
        drop(local);
        let local = LocalMachine::new(
            RecordOwner::spawn(LocalMachineStore::open(dir.path()).unwrap()).unwrap(),
        );
        local
            .activate_management_client(ClientKey::from_bytes(*replacement_public.as_bytes()))
            .await
            .unwrap();
        assert_eq!(
            local.record().accepted_client(&cloud()),
            Some(ClientKey::from_bytes(*replacement_public.as_bytes()))
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
        let first_key = ClientKey::from_bytes(
            *iroh::SecretKey::from_bytes(first.client_secret())
                .public()
                .as_bytes(),
        );
        local.activate_management_client(first_key).await.unwrap();
        let next = local
            .set_management_client(set())
            .await
            .unwrap()
            .capability
            .unwrap();
        let next_key = ClientKey::from_bytes(
            *iroh::SecretKey::from_bytes(next.client_secret())
                .public()
                .as_bytes(),
        );
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

    fn device() -> ployz_core::DeviceKey {
        ployz_core::DeviceKey::from_bytes(*iroh::SecretKey::generate().public().as_bytes())
    }

    #[tokio::test]
    async fn a_device_slot_is_full_only_when_set_device_writes_it() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let key = device();
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
        assert_eq!(record.accepted_client(&key.label()), Some(key.into()));
        assert_eq!(
            record.management_scope(&key.into()),
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
            Some(key.into())
        );
    }

    #[tokio::test]
    async fn no_caller_writes_a_device_slot_through_set_management_client() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let key = device();
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
        let key = device();
        for key in [device(), key] {
            local
                .set_device(SetDeviceRequest::Approve { key })
                .await
                .unwrap();
        }
        let remove = SetDeviceRequest::Remove { code: key.code() };
        assert_eq!(
            local.set_device(remove.clone()).await.unwrap(),
            DeviceSet::Removed { code: key.code() }
        );
        assert!(!local.record().accepts_management_client(&key.into()));
        assert_eq!(
            local.set_device(remove).await.unwrap(),
            DeviceSet::NotPresent { code: key.code() }
        );
    }

    #[tokio::test]
    async fn a_device_removed_while_its_request_waits_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let local = participating(dir.path()).await;
        let first = device();
        let second = device();
        for key in [device(), first] {
            local
                .set_device(SetDeviceRequest::Approve { key })
                .await
                .unwrap();
        }
        let caller = local.clone().with_management_client(first.into());
        let lock = local.owner.admission_lock().write_owned().await;
        let remove = local.set_device(SetDeviceRequest::Remove { code: first.code() });
        let queued = caller.set_device(SetDeviceRequest::Approve { key: second });
        tokio::pin!(remove, queued);
        assert!(futures_util::poll!(&mut remove).is_pending());
        assert!(futures_util::poll!(&mut queued).is_pending());
        drop(lock);
        remove.await.unwrap();
        assert!(matches!(queued.await, Err(Error::ManagementRevoked)));
        assert!(!local.record().accepts_management_client(&second.into()));
    }

    fn passkey(key: [u8; 32]) -> ployz_core::authority::Passkey {
        passkey_with_credential(key, "AQID")
    }

    fn passkey_with_credential(key: [u8; 32], credential: &str) -> ployz_core::authority::Passkey {
        use ployz_core::authority::trusted_key::{PasskeyAlgorithm, RelyingParty};
        ployz_core::authority::Passkey {
            credential_id: serde_json::from_value(serde_json::json!(credential)).unwrap(),
            public_key: ployz_core::authority::PasskeyPublicKey::parse(
                PasskeyAlgorithm::Eddsa,
                &key,
            )
            .unwrap(),
            account: ployz_core::authority::PasskeyAccount::parse("ada@example.com").unwrap(),
            relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap()).unwrap(),
        }
    }

    fn signer() -> Signer {
        Signer::from_seed(&[11; 32])
    }

    /// `participating`, holding a Sealing Key and trusting [`signer`] as its Device.
    async fn keyed(dir: &std::path::Path) -> LocalMachine {
        let local = participating(dir).await;
        if crate::sealing_key::SealingKey::load(dir).unwrap().is_none() {
            let key = crate::sealing_key::SealingKey::generate();
            crate::sealing_key::SealingKey::set(dir, Some(&key)).unwrap();
        }
        local
            .set_device(SetDeviceRequest::Approve {
                key: signer().public().into(),
            })
            .await
            .unwrap();
        local
    }

    fn cluster_of(local: &LocalMachine) -> ployz_core::SealingPublicKey {
        crate::sealing_key::SealingKey::load(local.owner.data_dir())
            .unwrap()
            .unwrap()
            .public_key()
    }

    /// A trust of `key` on `local` alone, as the only step of a bundle [`signer`] signed now.
    fn trust_step(local: &LocalMachine, key: NewKey) -> KeyStepRequest {
        trust_step_at(local, key, Timestamp::now())
    }

    /// [`trust_step`] under a bundle issued at `issued_at`.
    fn trust_step_at(local: &LocalMachine, key: NewKey, issued_at: Timestamp) -> KeyStepRequest {
        let record = local.record();
        let operation = ployz_core::OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap();
        let step = KeyStep {
            cluster: cluster_of(local),
            change: KeyChange::Trust {
                key,
                members: vec![KeyMember {
                    id: record.id(),
                    public_key: record.private_key().public_key(),
                }],
            },
            signer: signer().public().into(),
            proposal: Some(ProposalRef {
                operation,
                index: 0,
            }),
        };
        let plan = ProposalApproval {
            cluster: step.cluster,
            environment: ployz_core::Namespace::parse("app").unwrap(),
            operation,
            steps: vec![ProposalStep {
                index: 0,
                kind: StepKind::Key,
                digest: StepDigest::of(&Signed::unsigned(&step).unwrap().body),
            }],
            issued_at,
            expires_at: issued_at.plus(10 * 60 * 1000),
            signer: signer().public().into(),
        };
        KeyStepRequest {
            step: Signed::proposed(&step, Signed::sign(&plan, &signer()).unwrap()).unwrap(),
        }
    }

    /// A removal of `key`, signed alone by `by`.
    fn removal_step(local: &LocalMachine, key: KeyRef, by: &Signer) -> KeyStepRequest {
        let step = KeyStep {
            cluster: cluster_of(local),
            change: KeyChange::Remove { key },
            signer: by.public().into(),
            proposal: None,
        };
        KeyStepRequest {
            step: Signed::sign(&step, by).unwrap(),
        }
    }

    fn trust_passkey(
        local: &LocalMachine,
        passkey: ployz_core::authority::Passkey,
    ) -> KeyStepRequest {
        trust_step(local, NewKey::Passkey { passkey })
    }

    fn passkey_ref(key: [u8; 32]) -> KeyRef {
        KeyRef::Passkey {
            key: passkey(key).public_key,
        }
    }

    fn ci_key(key: ployz_core::authority::SigningPublicKey, namespaces: &[&str]) -> NewKey {
        use ployz_core::authority::{Cap, CapSet, KeyPlace, KeyPlaces, KeyScope};
        NewKey::Ci {
            key,
            scope: KeyScope {
                caps: CapSet::of(&[Cap::Spec]),
                places: KeyPlaces::Only(
                    namespaces
                        .iter()
                        .map(|namespace| KeyPlace {
                            environment: uuid::Uuid::from_u128(1),
                            namespace: ployz_core::Namespace::parse(*namespace).unwrap(),
                            service: None,
                        })
                        .collect(),
                ),
            },
            volumes: Vec::new(),
        }
    }

    fn ci_signer() -> Signer {
        Signer::from_seed(&[12; 32])
    }

    #[tokio::test]
    async fn only_a_newer_bundle_edits_a_ci_keys_scope_so_an_older_wider_one_cant_be_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let now = Timestamp::now();
        let earlier = Timestamp::from_millis(now.as_millis() - 120_000).unwrap();
        let first = Timestamp::from_millis(now.as_millis() - 60_000).unwrap();
        let ci = ci_signer().public();
        let code = ci.code();
        let scope_of = |local: &LocalMachine| {
            let record = local.record();
            let [held] = record.grants() else {
                panic!("one grant");
            };
            (held.grant.scope.clone(), held.grant.issued_at)
        };

        let staging = ci_key(ci, &["staging"]);
        assert_eq!(
            local
                .key_step(trust_step_at(&local, staging.clone(), first))
                .await
                .unwrap(),
            DeviceSet::Approved { code }
        );
        let wider = ci_key(ci, &["staging", "prod"]);
        assert_eq!(
            local
                .key_step(trust_step_at(&local, wider, earlier))
                .await
                .unwrap(),
            DeviceSet::AlreadyApproved { code }
        );
        let NewKey::Ci {
            scope: staging_scope,
            ..
        } = staging
        else {
            unreachable!()
        };
        assert_eq!(scope_of(&local), (staging_scope, first));

        let narrowed = ci_key(ci, &["web-staging"]);
        local
            .key_step(trust_step_at(&local, narrowed.clone(), now))
            .await
            .unwrap();
        let NewKey::Ci {
            scope: narrowed_scope,
            ..
        } = narrowed
        else {
            unreachable!()
        };
        assert_eq!(scope_of(&local), (narrowed_scope, now));
        assert_eq!(
            local
                .record()
                .management_scope(&ClientKey::from(DeviceKey::from(ci))),
            None,
            "a CI key holds no slot"
        );
    }

    #[tokio::test]
    async fn a_key_waiting_in_a_management_client_slot_never_becomes_a_grant() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let capability = local
            .set_management_client(set())
            .await
            .unwrap()
            .capability
            .unwrap();
        let operator = Signer::from_seed(capability.client_secret()).public();
        assert!(
            local
                .record()
                .pending_label(&ClientKey::from(DeviceKey::from(operator)))
                .is_some()
        );

        assert!(matches!(
            local
                .key_step(trust_step(&local, ci_key(operator, &["app"])))
                .await,
            Err(Error::KeyIsClient(_))
        ));
        assert!(local.record().grants().is_empty());
    }

    #[tokio::test]
    async fn a_ci_key_never_shares_its_key_with_a_device() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let device = signer().public();
        assert!(matches!(
            local
                .key_step(trust_step(&local, ci_key(device, &["app"])))
                .await,
            Err(Error::KeyHeld {
                held: ployz_core::authority::KeyKind::FullCli,
                ..
            })
        ));

        let ci = ci_signer().public();
        local
            .key_step(trust_step(&local, ci_key(ci, &["app"])))
            .await
            .unwrap();
        assert!(matches!(
            local
                .set_device(SetDeviceRequest::Approve { key: ci.into() })
                .await,
            Err(Error::KeyHeld {
                held: ployz_core::authority::KeyKind::Ci,
                ..
            })
        ));
        assert_eq!(
            local
                .record()
                .management_scope(&ClientKey::from(DeviceKey::from(ci))),
            None
        );
    }

    #[tokio::test]
    async fn a_ci_key_never_stands_in_for_the_last_device_and_its_removal_is_permanent() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let ci = ci_signer().public();
        local
            .key_step(trust_step(&local, ci_key(ci, &["app"])))
            .await
            .unwrap();

        assert!(matches!(
            local
                .set_device(SetDeviceRequest::RemoveKey {
                    key: KeyRef::Signing {
                        key: signer().public().into()
                    }
                })
                .await,
            Err(Error::Store(StoreError::LastKey(_)))
        ));
        let ci_ref = KeyRef::Signing { key: ci.into() };
        assert_eq!(
            local
                .key_step(removal_step(&local, ci_ref, &signer()))
                .await
                .unwrap(),
            DeviceSet::Removed { code: ci.code() }
        );
        assert!(local.record().grants().is_empty());
        assert!(matches!(
            local
                .key_step(trust_step(&local, ci_key(ci, &["app"])))
                .await,
            Err(Error::Store(StoreError::KeyRemoved(_)))
        ));
    }

    #[tokio::test]
    async fn a_device_signed_bundle_trusts_a_passkey_that_never_connects_or_signs() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let key = *iroh::SecretKey::generate().public().as_bytes();
        let code = passkey(key).code();
        assert_eq!(
            local
                .key_step(trust_passkey(&local, passkey(key)))
                .await
                .unwrap(),
            DeviceSet::Approved { code }
        );
        assert_eq!(
            local
                .key_step(trust_passkey(&local, passkey(key)))
                .await
                .unwrap(),
            DeviceSet::AlreadyApproved { code }
        );
        let record = local.record();
        assert_eq!(
            record.passkeys(),
            [TrustedPasskey {
                passkey: passkey(key),
                approved_by: PasskeyApprover::Device {
                    code: DeviceKey::from(signer().public()).code()
                },
            }]
        );
        assert!(!record.admits_management_client(&ClientKey::from_bytes(key)));
        assert_eq!(record.management_scope(&ClientKey::from_bytes(key)), None);
        assert_eq!(record.trusted_keys().signing_keys().count(), 1);
    }

    #[tokio::test]
    async fn an_operator_caller_such_as_cloud_relays_a_signed_step_and_nothing_else() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let cloud = local
            .clone()
            .with_management_client(accepted(&local, "cloud").await);
        let key = *iroh::SecretKey::generate().public().as_bytes();
        let stranger = Signer::from_seed(&[42; 32]);
        cloud
            .key_step(trust_passkey(&local, passkey(key)))
            .await
            .unwrap();
        assert!(matches!(
            cloud
                .key_step(removal_step(&local, passkey_ref(key), &stranger))
                .await,
            Err(Error::KeyRefused(_))
        ));
        assert_eq!(local.record().passkeys().len(), 1);
        assert_eq!(
            cloud
                .key_step(removal_step(&local, passkey_ref(key), &signer()))
                .await
                .unwrap(),
            DeviceSet::Removed {
                code: passkey(key).code()
            }
        );
    }

    #[tokio::test]
    async fn a_removed_key_is_never_trusted_again_on_any_route_even_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let removed = device();
        local
            .set_device(SetDeviceRequest::Approve { key: removed })
            .await
            .unwrap();
        let code = removed.code();
        assert_eq!(
            local
                .set_device(SetDeviceRequest::Remove { code })
                .await
                .unwrap(),
            DeviceSet::Removed { code }
        );
        let reread: crate::machine::LocalMachineRecord =
            serde_json::from_str(&serde_json::to_string(&*local.record()).unwrap()).unwrap();
        assert!(reread.removes_key(&KeyRef::Signing { key: removed }));
        assert!(matches!(
            local
                .set_device(SetDeviceRequest::Approve { key: removed })
                .await,
            Err(Error::Store(StoreError::KeyRemoved(_)))
        ));
        assert!(matches!(
            local
                .key_step(trust_step(&local, NewKey::Device { key: removed }))
                .await,
            Err(Error::Store(StoreError::KeyRemoved(_)))
        ));
        assert_eq!(
            local.record().devices().collect::<Vec<_>>(),
            [DeviceKey::from(signer().public())]
        );

        let never = *iroh::SecretKey::generate().public().as_bytes();
        assert_eq!(
            local
                .key_step(removal_step(&local, passkey_ref(never), &signer()))
                .await
                .unwrap(),
            DeviceSet::NotPresent {
                code: passkey(never).code()
            }
        );
        assert!(
            matches!(
                local.key_step(trust_passkey(&local, passkey(never))).await,
                Err(Error::Store(StoreError::KeyRemoved(_)))
            ),
            "a removal reaches a Server before the trust it outlives"
        );
    }

    /// `device rm` hands a Server that never held the key its full key. The Server marks it
    /// removed, so a grant of that key that reaches it later is refused.
    #[tokio::test]
    async fn a_full_key_removal_on_a_server_that_never_held_it_refuses_a_later_grant() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let never = device();
        assert_eq!(
            local
                .set_device(SetDeviceRequest::RemoveKey {
                    key: KeyRef::Signing { key: never }
                })
                .await
                .unwrap(),
            DeviceSet::NotPresent { code: never.code() }
        );
        assert!(matches!(
            local
                .key_step(trust_step(&local, NewKey::Device { key: never }))
                .await,
            Err(Error::Store(StoreError::KeyRemoved(_)))
        ));
        assert!(!local.record().devices().any(|device| device == never));
    }

    #[tokio::test]
    async fn the_last_key_a_server_trusts_is_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let only = DeviceKey::from(signer().public());
        assert!(matches!(
            local
                .set_device(SetDeviceRequest::Remove { code: only.code() })
                .await,
            Err(Error::Store(StoreError::LastKey(_)))
        ));
        assert!(matches!(
            local
                .key_step(removal_step(
                    &local,
                    KeyRef::Signing { key: only },
                    &signer()
                ))
                .await,
            Err(Error::Store(StoreError::LastKey(_)))
        ));
        assert_eq!(local.record().devices().collect::<Vec<_>>(), [only]);
        assert!(!local.record().removes_key(&KeyRef::Signing { key: only }));
    }

    #[tokio::test]
    async fn set_device_removes_a_passkey_by_its_code_for_good() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let key = *iroh::SecretKey::generate().public().as_bytes();
        let code = passkey(key).code();
        local
            .key_step(trust_passkey(&local, passkey(key)))
            .await
            .unwrap();
        assert_eq!(
            local
                .set_device(SetDeviceRequest::Remove { code })
                .await
                .unwrap(),
            DeviceSet::Removed { code }
        );
        assert!(local.record().passkeys().is_empty());
        assert!(local.record().removes_key(&passkey_ref(key)));
    }

    #[tokio::test]
    async fn a_device_with_a_trusted_passkeys_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let shared = device();
        local
            .key_step(trust_passkey(&local, passkey(*shared.as_bytes())))
            .await
            .unwrap();
        assert!(matches!(
            local.set_device(SetDeviceRequest::Approve { key: shared }).await,
            Err(Error::DeviceIsPasskey(code)) if code == shared.code()
        ));
        assert_eq!(local.record().devices().count(), 1);
    }

    #[tokio::test]
    async fn a_passkey_with_an_approved_devices_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let shared = device();
        local
            .set_device(SetDeviceRequest::Approve { key: shared })
            .await
            .unwrap();
        assert!(matches!(
            local
                .key_step(trust_passkey(&local, passkey(*shared.as_bytes())))
                .await,
            Err(Error::PasskeyIsDevice(code)) if code == shared.code()
        ));
        assert!(local.record().passkeys().is_empty());
    }

    #[tokio::test]
    async fn a_credential_id_another_passkey_holds_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let first = passkey_with_credential(*device().as_bytes(), "AQID");
        let second = passkey_with_credential(*device().as_bytes(), "AQID");
        local
            .key_step(trust_passkey(&local, first.clone()))
            .await
            .unwrap();
        assert!(matches!(
            local.key_step(trust_passkey(&local, second)).await,
            Err(Error::CredentialHeld { held }) if held == first.code()
        ));
        assert_eq!(local.record().passkeys().len(), 1);
    }

    #[tokio::test]
    async fn a_record_with_passkey_fields_from_a_newer_daemon_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let local = keyed(dir.path()).await;
        let key = *iroh::SecretKey::generate().public().as_bytes();
        local
            .key_step(trust_passkey(&local, passkey(key)))
            .await
            .unwrap();
        let mut record = serde_json::to_value(&*local.record()).unwrap();
        let passkeys = record
            .get_mut("passkeys")
            .and_then(serde_json::Value::as_array_mut)
            .unwrap();
        let mut newer = passkeys.first().unwrap().clone();
        set_field(
            &mut newer,
            &["approved_by"],
            serde_json::json!({ "by": "passkey", "code": "aaaaaaaaaa" }),
        );
        set_field(
            &mut newer,
            &["passkey", "transports"],
            serde_json::json!(["usb"]),
        );
        set_field(
            &mut newer,
            &["passkey", "public_key", "attestation"],
            serde_json::json!("none"),
        );
        set_field(
            &mut newer,
            &["passkey", "relying_party", "top_origin"],
            serde_json::json!("https://a.test"),
        );
        set_field(
            &mut newer,
            &["passkey", "credential_id"],
            serde_json::json!("BAUG"),
        );
        let mut unreadable = passkeys.first().unwrap().clone();
        set_field(
            &mut unreadable,
            &["passkey", "public_key", "algorithm"],
            serde_json::json!("rs256"),
        );
        passkeys.extend([newer, unreadable]);

        let reread: crate::machine::LocalMachineRecord = serde_json::from_value(record).unwrap();
        assert_eq!(
            reread
                .passkeys()
                .iter()
                .map(|trusted| (trusted.passkey.code(), trusted.approved_by))
                .collect::<Vec<_>>(),
            [
                (
                    passkey(key).code(),
                    PasskeyApprover::Device {
                        code: DeviceKey::from(signer().public()).code()
                    }
                ),
                (passkey(key).code(), PasskeyApprover::Unknown),
            ]
        );
    }

    /// Set the field at `path` in `value`, adding it when absent.
    fn set_field(value: &mut serde_json::Value, path: &[&str], new: serde_json::Value) {
        let (last, parents) = path.split_last().unwrap();
        let parent = parents
            .iter()
            .fold(value, |value, key| value.get_mut(*key).unwrap());
        parent
            .as_object_mut()
            .unwrap()
            .insert((*last).to_owned(), new);
    }
}
