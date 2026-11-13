//! Durable Local Machine record ownership and persistence.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use ployz_core::{
    ClientKey, DeviceCode, DeviceKey, EnrollAs, EnrollRequest, LocalMachinePhase, Machine,
    MachineId, MachineNow, MachineUpdate, MachineUpdateError, ManagementClientLabel,
    PREPARE_DIR_NAME, PinnedAddresses, PublicIpUpdate, RecoverySeed, Registered, SelectedEndpoint,
    apply_machine_update,
};
use reqwest::Url;
use thiserror::Error;

use super::{
    FoundingCluster, LiveSlot, LocalMachineBody, LocalMachineRecord, ManagementClientSlot,
    ParticipationOrigin, local_runtime,
};
use crate::management::ManagementSecret;
use crate::network::{DiscoveredNetwork, WireGuardPrivateKey, allocate_machine_subnet};
use crate::sealing_key::{KeyFile, OpenError, SealingKey};

pub(crate) const STATE_FILE_NAME: &str = "machine.json";
const TEMPORARY_FILE_NAME: &str = ".machine.json.tmp";
pub(super) const PENDING_RESET_FILE_NAME: &str = ".machine.reset.pending";
const LOCK_FILE_NAME: &str = ".ployzd.lock";

pub struct LocalMachineStore {
    pub(super) data_dir: PathBuf,
    pub(super) run_dir: PathBuf,
    record: LocalMachineRecord,
    _lock: File,
    // Lock order: admission, then publication/ingress/Docker. Record mutations queue
    // on the record owner and take no lock of their own.
    // Reset waits for admitted operations, including Docker streams with no total deadline.
    // Exclusive for changes to the record or Service Containers; shared for image
    // transfers, which run side by side while all exclusive work waits for them.
    pub(super) admission_lock: Arc<tokio::sync::RwLock<()>>,
    pub(super) mutation_gate: crate::mutation::MutationGate,
    pub(super) sealing: tokio::sync::watch::Sender<Option<KeyFile<ployz_core::SealingPublicKey>>>,
}

pub(crate) struct PreparedReset {
    data_dir: PathBuf,
    original: LocalMachineRecord,
    resetting: LocalMachineRecord,
}

pub(crate) struct Installation {
    body: LocalMachineBody,
    sealing_key: Option<SealingKey>,
    recipient: Option<JoinedRecipient>,
    keys: ployz_core::authority::TrustedKeys,
    wireguard_mtu: Option<u32>,
    pinned: PinnedAddresses,
    enrollment: EnrollRequest,
}

/// The recipient key a joiner after activation opened from its registration, under a
/// rotation it verified against the keys its join approval names.
struct JoinedRecipient {
    key: SealingKey,
    rotation: ployz_core::authority::Signed<ployz_core::authority::SealingRotation>,
    operation: ployz_core::OperationId,
}

/// What an enrollment installs: the body, the Cluster's key, the keys its approval names,
/// and the recipient key after activation.
type Enrolled = (
    LocalMachineBody,
    Option<SealingKey>,
    ployz_core::authority::TrustedKeys,
    Option<JoinedRecipient>,
);

/// The recipient key in `recipient`, when its rotation is for the join's Cluster and signed by
/// a Device the verified join approval `body` names, and its sealed key opens to the key the
/// rotation names. Anything else is refused: the joiner installs no recipient key and says so.
fn joined_recipient(
    recipient: &ployz_core::JoinRecipient,
    body: &ployz_core::authority::JoinApproval,
    wireguard: &crate::network::WireGuardPrivateKey,
) -> Option<JoinedRecipient> {
    let trusted = ployz_core::authority::TrustedKeys {
        devices: body.devices.clone(),
        passkeys: Vec::new(),
    };
    let verified = ployz_core::authority::rotation::verify_for_join(
        &recipient.rotation,
        &trusted,
        body.cluster,
        ployz_core::authority::Timestamp::now(),
    );
    let rotation = match verified {
        Ok((rotation, _)) => rotation,
        Err(refusal) => {
            tracing::warn!(
                %refusal,
                "joined without the Cluster's recipient key: its rotation did not verify"
            );
            return None;
        }
    };
    let opened = ployz_core::RecipientSecret::open(
        wireguard.as_bytes(),
        &ployz_core::authority::rotation::sealed_key_binding(&body.cluster, &rotation.operation),
        &recipient.sealed,
    )
    .map(|secret| SealingKey::from_secret(*secret.secret()))
    .filter(|key| key.public_key() == rotation.recipient);
    if opened.is_none() {
        tracing::warn!(
            "joined without the Cluster's recipient key: it did not open to the key its rotation names"
        );
    }
    opened.map(|key| JoinedRecipient {
        key,
        rotation: recipient.rotation.clone(),
        operation: rotation.operation,
    })
}

impl Installation {
    pub(crate) fn machine(&self) -> Option<&Machine> {
        self.body.machine()
    }

    pub(crate) fn wireguard_mtu(&self) -> Option<u32> {
        self.wireguard_mtu
    }
}

/// What an Enroll does on this Machine.
#[derive(Clone, Debug, PartialEq)]
pub enum EnrollPlan {
    Fresh,
    /// Already enrolled exactly as asked; the stored Machine.
    Replay(Machine),
    /// Enrolled as someone else, or resetting.
    Conflict(MachineNow),
}

/// What daemon-start discovery did to this Machine's own row.
#[derive(Clone, Debug, PartialEq)]
pub enum Rediscovered {
    Unchanged,
    Updated(Box<Machine>),
    PublicIpLookupFailed,
}

/// Replay when the stored Machine and MTU equal what the request would produce.
pub(crate) fn plan_enroll(
    now: &MachineNow,
    stored_mtu: Option<u32>,
    wanted: &Machine,
    wanted_mtu: Option<u32>,
) -> EnrollPlan {
    match now.phase {
        LocalMachinePhase::Uninitialized => EnrollPlan::Fresh,
        LocalMachinePhase::Joining | LocalMachinePhase::Participating
            if now.machine.as_ref() == Some(wanted) && stored_mtu == wanted_mtu =>
        {
            EnrollPlan::Replay(wanted.clone())
        }
        LocalMachinePhase::Joining
        | LocalMachinePhase::Participating
        | LocalMachinePhase::Resetting
        | LocalMachinePhase::Unrecognized(_) => EnrollPlan::Conflict(now.clone()),
    }
}

impl PreparedReset {
    pub(crate) fn commit(self, store: &mut LocalMachineStore) -> Result<(), StoreError> {
        let mut current = store.record.clone();
        current
            .selected_endpoints
            .clone_from(&self.original.selected_endpoints);
        if store.data_dir != self.data_dir || current != self.original {
            return Err(StoreError::ResetPreparationLost(store.data_dir.clone()));
        }
        fs::rename(
            self.data_dir.join(PENDING_RESET_FILE_NAME),
            path(&self.data_dir),
        )?;
        store.record = self.resetting.clone();
        if let Err(error) = File::open(&self.data_dir).and_then(|directory| directory.sync_all()) {
            eprintln!(
                "failed to sync committed local Machine reset: {error}",
                error = ployz_core::error_chain::inline(&error),
            );
        }
        Ok(())
    }
}

impl Drop for PreparedReset {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.data_dir.join(PENDING_RESET_FILE_NAME));
    }
}

impl Drop for LocalMachineStore {
    fn drop(&mut self) {
        // A concurrently forked child can briefly inherit the flock before exec closes the fd.
        let _ = fs2::FileExt::unlock(&self._lock);
    }
}

/// A store opened by the daemon. A reset that stopped before clearing the data
/// directory comes back as [`Opened::Resetting`], so the caller removes what
/// still runs from that directory before it is deleted.
pub(crate) enum Opened {
    Ready(LocalMachineStore),
    Resetting(InterruptedReset),
}

pub(crate) struct InterruptedReset(LocalMachineStore);

impl InterruptedReset {
    pub(crate) fn record(&self) -> &LocalMachineRecord {
        self.0.record()
    }

    pub(crate) fn complete(self) -> Result<LocalMachineStore, StoreError> {
        let store = self.0;
        store.complete_reset()?;
        let data_dir = store.data_dir.clone();
        let run_dir = store.run_dir.clone();
        drop(store);
        match LocalMachineStore::open_with_admission(&data_dir, run_dir)? {
            Opened::Ready(store) => Ok(store),
            Opened::Resetting(_) => Err(StoreError::ResetInterrupted(data_dir)),
        }
    }
}

impl LocalMachineStore {
    /// # Errors
    ///
    /// Returns [`StoreError::ResetInterrupted`] when the record is Resetting:
    /// only the daemon may finish that reset.
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_dir = data_dir.as_ref();
        match Self::open_with_admission(data_dir, data_dir.join(".run"))? {
            Opened::Ready(store) => Ok(store),
            Opened::Resetting(_) => Err(StoreError::ResetInterrupted(data_dir.to_owned())),
        }
    }

    pub(crate) fn open_with_admission(
        data_dir: impl AsRef<Path>,
        run_dir: impl AsRef<Path>,
    ) -> Result<Opened, StoreError> {
        let data_dir = data_dir.as_ref().to_owned();
        let run_dir = run_dir.as_ref().to_owned();
        validate_data_dir(&data_dir)?;
        claim_data_dir(&data_dir)?;
        fs::create_dir_all(&data_dir)?;
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o711))?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(data_dir.join(LOCK_FILE_NAME))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                StoreError::AlreadyRunning(data_dir.clone())
            } else {
                StoreError::Io(error)
            }
        })?;
        let record = match fs::read(path(&data_dir)) {
            Ok(data) => vet_device_slots(&data_dir, serde_json::from_slice(&data)?)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match fs::remove_file(data_dir.join(TEMPORARY_FILE_NAME)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                let record = LocalMachineRecord::fresh(
                    LocalMachineBody::Uninitialized {
                        id: MachineId::random(),
                    },
                    WireGuardPrivateKey::generate(),
                );
                save(&data_dir, &record)?;
                record
            }
            Err(error) => return Err(error.into()),
        };

        let mut store = Self {
            data_dir: data_dir.clone(),
            record,
            _lock: lock,
            admission_lock: Arc::new(tokio::sync::RwLock::new(())),
            run_dir: run_dir.clone(),
            mutation_gate: crate::mutation::MutationGate::new(&run_dir, &data_dir),
            sealing: tokio::sync::watch::Sender::new(SealingKey::observe(&data_dir)),
        };
        if store.record.phase() == LocalMachinePhase::Resetting {
            return Ok(Opened::Resetting(InterruptedReset(store)));
        }
        store.refresh_runtime()?;
        store.ensure_snapshot()?;
        Ok(Opened::Ready(store))
    }

    /// Write the trusted snapshot, empty, on this daemon's first start: nothing a record
    /// learned before a daemon that never gives the mesh Full scope has evidence.
    fn ensure_snapshot(&mut self) -> Result<(), StoreError> {
        if self.record.trusted_snapshot.is_some() {
            return Ok(());
        }
        let mut updated = self.record.clone();
        updated.trusted_snapshot = Some(super::TrustedSnapshot::default());
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    #[must_use]
    pub fn record(&self) -> &LocalMachineRecord {
        &self.record
    }

    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn refresh_runtime(&mut self) -> Result<(), StoreError> {
        let runtime = local_runtime(&self.data_dir);
        let stale = match &self.record.body {
            LocalMachineBody::Joining { machine, .. }
            | LocalMachineBody::Participating { machine, .. } => machine.runtime != runtime,
            LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
                return Ok(());
            }
        };
        if !stale {
            return Ok(());
        }
        let mut refreshed = self.record.clone();
        match &mut refreshed.body {
            LocalMachineBody::Joining { machine, .. }
            | LocalMachineBody::Participating { machine, .. } => machine.runtime = runtime,
            LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
                return Ok(());
            }
        }
        save(&self.data_dir, &refreshed)?;
        self.record = refreshed;
        Ok(())
    }

    pub fn begin_reset(&mut self) -> Result<(), StoreError> {
        let prepared = self.prepare_reset()?;
        prepared.commit(self)
    }

    pub(crate) fn prepare_reset(&self) -> Result<PreparedReset, StoreError> {
        if self.record.phase() == LocalMachinePhase::Resetting {
            return Err(StoreError::AlreadyResetting);
        }
        let resetting = self.record.clone().into_resetting();
        write_record(&self.data_dir.join(PENDING_RESET_FILE_NAME), &resetting)?;
        File::open(&self.data_dir)?.sync_all()?;
        Ok(PreparedReset {
            data_dir: self.data_dir.clone(),
            original: self.record.clone(),
            resetting,
        })
    }

    /// Persist a fresh enrollment. A replay or a conflict changes nothing.
    ///
    /// # Errors
    /// Rejects missing endpoints or peers, an invalid network, an assignment for
    /// another Machine or key, or a failed save.
    pub fn enroll(&mut self, request: EnrollRequest) -> Result<EnrollPlan, StoreError> {
        self.enroll_with(request, None)
    }

    /// Persist a fresh founding that holds no Sealing Key until it recovers
    /// the one `recovery` names.
    ///
    /// # Errors
    /// As [`Self::enroll`], and rejects a request that does not found a Cluster.
    pub fn enroll_for_recovery(
        &mut self,
        request: EnrollRequest,
        recovery: RecoverySeed,
    ) -> Result<EnrollPlan, StoreError> {
        self.enroll_with(request, Some(recovery))
    }

    fn enroll_with(
        &mut self,
        request: EnrollRequest,
        recovery: Option<RecoverySeed>,
    ) -> Result<EnrollPlan, StoreError> {
        let (plan, installation) = self.plan_enrollment(request, recovery)?;
        if matches!(plan, EnrollPlan::Fresh) {
            self.install(installation)?;
        }
        Ok(plan)
    }

    /// The Installation this request would persist, and whether this Machine
    /// takes it fresh, already holds it, or holds another enrollment.
    pub(crate) fn plan_enrollment(
        &self,
        request: EnrollRequest,
        recovery: Option<RecoverySeed>,
    ) -> Result<(EnrollPlan, Installation), StoreError> {
        let enrollment = request.clone();
        if self.record.enrollment.as_ref() == Some(&request) {
            let plan = match self.record.phase() {
                LocalMachinePhase::Joining | LocalMachinePhase::Participating => {
                    EnrollPlan::Replay(
                        self.record
                            .machine()
                            .expect("persisted enrollment owns a Machine")
                            .clone(),
                    )
                }
                LocalMachinePhase::Uninitialized
                | LocalMachinePhase::Resetting
                | LocalMachinePhase::Unrecognized(_) => EnrollPlan::Conflict(self.record.now()),
            };
            return Ok((
                plan,
                Installation {
                    body: self.record.body.clone(),
                    sealing_key: None,
                    recipient: None,
                    keys: ployz_core::authority::TrustedKeys::default(),
                    wireguard_mtu: request.wireguard_mtu,
                    pinned: request.pinned,
                    enrollment: request,
                },
            ));
        }
        let EnrollRequest {
            role,
            wireguard_mtu,
            pinned,
        } = request;
        let (body, sealing_key, keys, recipient) = match role {
            EnrollAs::Founder {
                name,
                cluster_network,
                initial_policy,
                public_ip,
                advertised_endpoints,
                grant,
            } => {
                let (body, sealing_key) = self.founder(
                    name,
                    FoundingCluster {
                        network: cluster_network,
                        recovery,
                        grant,
                    },
                    initial_policy,
                    public_ip,
                    advertised_endpoints,
                )?;
                (
                    body,
                    sealing_key,
                    ployz_core::authority::TrustedKeys::default(),
                    None,
                )
            }
            EnrollAs::Member { .. } if recovery.is_some() => {
                return Err(StoreError::RecoveryWithoutFounding);
            }
            EnrollAs::Member { registration, pin } => self.member(registration, pin)?,
        };
        let wanted = body.machine().expect("an enrollment owns its Machine");
        let plan = plan_enroll(
            &self.record.now(),
            self.record.wireguard_mtu,
            wanted,
            wireguard_mtu,
        );
        Ok((
            plan,
            Installation {
                body,
                sealing_key,
                recipient,
                keys,
                wireguard_mtu,
                pinned,
                enrollment,
            },
        ))
    }

    fn founder(
        &self,
        name: ployz_core::MachineName,
        cluster: FoundingCluster,
        initial_policy: ployz_core::InitialMachinePolicy,
        public_ip: Option<std::net::IpAddr>,
        advertised_endpoints: Vec<ployz_core::AdvertisedEndpoint>,
    ) -> Result<(LocalMachineBody, Option<SealingKey>), StoreError> {
        if advertised_endpoints.is_empty() {
            return Err(StoreError::MissingEndpoints);
        }
        let machine = Machine {
            labels: initial_policy.labels,
            accepts_builds: initial_policy.accepts_builds,
            accepts_services: initial_policy.accepts_services,
            accepts_ingress: initial_policy.accepts_ingress,
            id: self.record.id(),
            name,
            subnet: allocate_machine_subnet(cluster.network, []).map_err(|error| {
                StoreError::InvalidNetwork(ployz_core::error_chain::inline(&error))
            })?,
            public_key: self.record.wireguard_private_key.public_key(),
            public_ip,
            advertised_endpoints,
            runtime: local_runtime(&self.data_dir),
            join_approval: None,
            build_concurrency: None,
        };
        let sealing_key = cluster.recovery.is_none().then(SealingKey::generate);
        Ok((
            LocalMachineBody::Participating {
                machine,
                origin: ParticipationOrigin::Founder { cluster },
            },
            sealing_key,
        ))
    }

    /// A Member's enrollment, admitted only under an approved capsule: one that opens to this
    /// Machine and carries a join approval signed by a key in `pin`, or with no pin, by a
    /// Device the approval lists. Anything else is refused and the record stays as it was.
    fn member(
        &self,
        registration: Registered,
        pin: Option<ployz_core::JoinPin>,
    ) -> Result<Enrolled, StoreError> {
        let Registered {
            assigned_machine: mut machine,
            mut visible_peers,
            target_versions,
            sealing_key,
            devices: _,
            recipient,
        } = registration;
        if self.record.id() != machine.id {
            return Err(StoreError::IdentityMismatch);
        }
        if visible_peers.is_empty() {
            return Err(StoreError::MissingPeers);
        }
        if self.record.wireguard_private_key.public_key() != machine.public_key {
            return Err(StoreError::KeyMismatch);
        }
        if machine.advertised_endpoints.is_empty() {
            return Err(StoreError::MissingEndpoints);
        }
        let opened = sealing_key
            .map(|sealed| SealingKey::open_join(&sealed, &self.record.wireguard_private_key))
            .transpose()?;
        let (sealing_key, approval) = match opened {
            Some((key, approval)) => (Some(key), approval),
            None => (None, None),
        };
        let Some(approval) = approval else {
            return Err(StoreError::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Unsigned {
                    purpose: ployz_core::authority::Purpose::Join,
                },
            ));
        };
        let body = approval.open().map_err(StoreError::JoinRefused)?;
        let trusted = pin.map_or(body.devices, |pin| pin.devices);
        let body = ployz_core::authority::join::verify_signature(
            &approval,
            &trusted,
            ployz_core::authority::Timestamp::now(),
        )
        .map_err(StoreError::JoinRefused)?;
        if body.machine != ployz_core::authority::join::JoinAllocation::from(&machine)
            || body.sealing_recipient != machine.public_key
            || sealing_key.as_ref().map(SealingKey::public_key) != Some(body.cluster)
            || !body.network.contains(&ipnet::Ipv4Net::from(machine.subnet))
            || body.admitting_machines.is_empty()
        {
            return Err(StoreError::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Mismatch {
                    field: "a joining Machine",
                },
            ));
        }
        visible_peers = body
            .bootstrap
            .iter()
            .map(|allocation| Machine {
                join_approval: None,
                id: allocation.id,
                name: allocation.name.clone(),
                subnet: allocation.subnet,
                public_key: allocation.public_key,
                public_ip: allocation.public_ip,
                advertised_endpoints: allocation.advertised_endpoints.clone(),
                labels: allocation.policy.labels.clone(),
                accepts_builds: allocation.policy.accepts_builds,
                accepts_services: allocation.policy.accepts_services,
                accepts_ingress: allocation.policy.accepts_ingress,
                runtime: Default::default(),
                build_concurrency: None,
            })
            .collect();
        if visible_peers.is_empty() {
            return Err(StoreError::MissingPeers);
        }
        let mut approved_passkeys: Vec<ployz_core::authority::TrustedPasskey> = Vec::new();
        for passkey in &body.passkeys {
            let code = passkey.code();
            let held = approved_passkeys
                .iter()
                .any(|held| held.passkey.code() == code)
                || body.devices.iter().any(|device| device.code() == code);
            if !held {
                approved_passkeys.push(ployz_core::authority::TrustedPasskey {
                    passkey: passkey.clone(),
                    approved_by: ployz_core::authority::PasskeyApprover::Device {
                        code: body.signer.code(),
                    },
                });
            }
        }
        machine.join_approval = Some(approval);
        let joined = recipient.and_then(|recipient| {
            joined_recipient(&recipient, &body, &self.record.wireguard_private_key)
        });
        machine.runtime = local_runtime(&self.data_dir);
        Ok((
            LocalMachineBody::Joining {
                machine,
                bootstrap: visible_peers,
                min_store_version: target_versions,
            },
            sealing_key,
            ployz_core::authority::TrustedKeys {
                devices: body.devices,
                passkeys: approved_passkeys,
            },
            joined,
        ))
    }

    pub(crate) fn install(&mut self, installation: Installation) -> Result<(), StoreError> {
        self.require_uninitialized()?;
        let mut installed = self.record.clone();
        installed.body = installation.body;
        installed.filters_peers = true;
        installed.born_enforcing = true;
        let joins = installed.phase() == LocalMachinePhase::Joining;
        installed.trusted_machines = installed.body.bootstrap().iter().map(Into::into).collect();
        installed.membership_cluster = installation
            .sealing_key
            .as_ref()
            .map(SealingKey::public_key);
        installed.enrollment = Some(installation.enrollment);
        installed.wireguard_mtu = installation.wireguard_mtu;
        installed.pinned_addresses = installation.pinned;
        if joins {
            installed
                .management_clients
                .retain(|label, _| !DeviceCode::reserves(label));
            installed.passkeys = installation.keys.passkeys.clone();
        }
        installed
            .management_clients
            .extend(installation.keys.devices.iter().map(|&key| {
                (
                    key.label(),
                    ManagementClientSlot::Live(LiveSlot::Active {
                        accepted: ClientKey::from(key),
                    }),
                )
            }));
        if let Some(joined) = &installation.recipient {
            SealingKey::set_recipient_rotation(&self.data_dir, &joined.rotation)?;
            SealingKey::set_recipient(&self.data_dir, &joined.key)?;
            let snapshot = installed
                .trusted_snapshot
                .get_or_insert_with(super::TrustedSnapshot::default);
            for member in installed
                .trusted_machines
                .iter()
                .chain(installed.body.machine().map(Into::into).as_ref())
            {
                snapshot.add_member(member.clone());
            }
            let codes = installation.keys.devices.iter().map(DeviceKey::code).chain(
                installation
                    .keys
                    .passkeys
                    .iter()
                    .map(|trusted| trusted.passkey.code()),
            );
            for code in codes {
                snapshot.add_key(
                    code,
                    super::Evidence::Activation {
                        operation: joined.operation,
                    },
                );
            }
            installed.activation = Some(joined.operation);
            installed.activated_at = Some(ployz_core::authority::Timestamp::now());
            tracing::info!(
                operation = %joined.operation,
                "installed the Cluster's recipient key from the join"
            );
        }
        self.set_sealing_key(installation.sealing_key.as_ref())?;
        save(&self.data_dir, &installed)?;
        self.record = installed;
        Ok(())
    }

    pub(crate) fn set_sealing_key(&mut self, key: Option<&SealingKey>) -> io::Result<()> {
        SealingKey::set(&self.data_dir, key)?;
        if self.record.filters_peers && self.record.membership_cluster.is_none() {
            let mut updated = self.record.clone();
            updated.membership_cluster = key.map(SealingKey::public_key);
            save(&self.data_dir, &updated).map_err(io::Error::other)?;
            self.record = updated;
        }
        self.sealing
            .send_replace(SealingKey::observe(&self.data_dir));
        Ok(())
    }

    pub fn update(
        &mut self,
        update: MachineUpdate,
        visible: &[Machine],
    ) -> Result<Machine, StoreError> {
        let pinned = self.record.pinned_addresses.with_update(&update);
        self.apply(update, visible, pinned)
    }

    /// Replace the unpinned public IP and Advertised Endpoints with `discovered`.
    ///
    /// # Errors
    /// Rejects a Machine that is not participating, an empty endpoint list, or a failed save.
    pub fn rediscover(
        &mut self,
        discovered: DiscoveredNetwork,
    ) -> Result<Rediscovered, StoreError> {
        let LocalMachineBody::Participating { machine, .. } = &self.record.body else {
            return Err(StoreError::NotParticipating);
        };
        let pinned = self.record.pinned_addresses;
        // Without an IPv4 route, a stored IPv4 public IP was a NAT64 gateway's.
        let foreign = !discovered.ipv4_route && machine.public_ip.is_some_and(|ip| ip.is_ipv4());
        if !pinned.public_ip
            && discovered.public_ip.is_none()
            && machine.public_ip.is_some()
            && !foreign
        {
            return Ok(Rediscovered::PublicIpLookupFailed);
        }
        let update = MachineUpdate {
            public_ip: match discovered.public_ip {
                Some(address) if !pinned.public_ip && machine.public_ip != Some(address) => {
                    PublicIpUpdate::Set(address)
                }
                None if !pinned.public_ip && foreign => PublicIpUpdate::Remove,
                _ => PublicIpUpdate::Keep,
            },
            advertised_endpoints: (!pinned.advertised_endpoints
                && discovered.endpoints != machine.advertised_endpoints)
                .then_some(discovered.endpoints),
            ..MachineUpdate::default()
        };
        if update.is_empty() {
            return Ok(Rediscovered::Unchanged);
        }
        self.apply(update, &[], pinned)
            .map(|machine| Rediscovered::Updated(Box::new(machine)))
    }

    fn apply(
        &mut self,
        update: MachineUpdate,
        visible: &[Machine],
        pinned: PinnedAddresses,
    ) -> Result<Machine, StoreError> {
        let mut record = self.record.clone();
        let LocalMachineBody::Participating { machine, .. } = &mut record.body else {
            return Err(StoreError::NotParticipating);
        };
        let updated = apply_machine_update(machine, visible, update)?;
        *machine = updated.clone();
        record.pinned_addresses = pinned;
        save(&self.data_dir, &record)?;
        self.record = record;
        Ok(updated)
    }

    fn require_uninitialized(&self) -> Result<(), StoreError> {
        if matches!(self.record.body, LocalMachineBody::Uninitialized { .. }) {
            Ok(())
        } else {
            Err(StoreError::AlreadyInitialized)
        }
    }

    pub fn complete_catch_up(&mut self) -> Result<(), StoreError> {
        let mut participating = self.record.clone();
        let LocalMachineBody::Joining {
            machine, bootstrap, ..
        } = participating.body
        else {
            return Err(StoreError::NotJoining);
        };
        participating.body = LocalMachineBody::Participating {
            machine,
            origin: ParticipationOrigin::Join { bootstrap },
        };
        save(&self.data_dir, &participating)?;
        self.record = participating;
        Ok(())
    }

    pub fn persist_selected_endpoint(
        &mut self,
        machine_id: MachineId,
        endpoint: SelectedEndpoint,
    ) -> Result<(), StoreError> {
        let mut updated = self.record.clone();
        updated.selected_endpoints.insert(machine_id, endpoint);
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    /// Stage a fresh client key in `label`'s slot, beside any key it has accepted.
    /// A Cleared tombstone is replaced.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn stage_management_client(
        &mut self,
        label: ManagementClientLabel,
        pending: ClientKey,
    ) -> Result<(), StoreError> {
        let slot = match self.record.accepted_client(&label) {
            Some(accepted) => ManagementClientSlot::Live(LiveSlot::Rotating { accepted, pending }),
            None => ManagementClientSlot::Live(LiveSlot::Pending { pending }),
        };
        self.persist_management_clients(|clients| {
            clients.insert(label, slot);
        })
    }

    /// Turn `label`'s slot into a Cleared tombstone of its public keys, in one write.
    /// An absent label stays absent and a tombstone stays unchanged, without a write.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn clear_management_client(
        &mut self,
        label: &ManagementClientLabel,
    ) -> Result<(), StoreError> {
        let Some(was) = self
            .record
            .management_clients
            .get(label)
            .and_then(|slot| slot.live())
        else {
            return Ok(());
        };
        self.persist_management_clients(|clients| {
            clients.insert(label.clone(), ManagementClientSlot::Cleared { was });
        })
    }

    /// Commit a pending key for its holder's first operational RPC, after identity verification.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn activate_management_client(&mut self, remote: ClientKey) -> Result<(), StoreError> {
        let Some(label) = self.record.pending_label(&remote).cloned() else {
            return Ok(());
        };
        self.persist_management_clients(|clients| {
            clients.insert(
                label,
                ManagementClientSlot::Live(LiveSlot::Active { accepted: remote }),
            );
        })
    }

    /// Admit `key` as an accepted client in the slot its [`DeviceCode`] names, in one
    /// write. Returns `false`, without a write, when the slot already holds exactly it.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn approve_device(
        &mut self,
        key: DeviceKey,
        evidence: Option<super::Evidence>,
    ) -> Result<bool, StoreError> {
        let label = key.label();
        let slot = ManagementClientSlot::Live(LiveSlot::Active {
            accepted: ClientKey::from(key),
        });
        let held = self.record.management_clients.get(&label) == Some(&slot);
        let evidenced = evidence.is_none()
            || self
                .record
                .trusted_snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.holds_key(key.code()));
        if held && evidenced {
            return Ok(false);
        }
        let mut updated = self.record.clone();
        updated.management_clients.insert(label, slot);
        if let (Some(evidence), Some(snapshot)) = (evidence, updated.trusted_snapshot.as_mut()) {
            snapshot.add_key(key.code(), evidence);
        }
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(!held)
    }

    /// Clear the Device slot `code` names. Returns `false` when it holds no live key.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn remove_device(&mut self, code: DeviceCode) -> Result<bool, StoreError> {
        let label = code.label();
        let live = self
            .record
            .management_clients
            .get(&label)
            .is_some_and(|slot| slot.live().is_some());
        self.clear_management_client(&label)?;
        self.forget_evidence(code)?;
        Ok(live)
    }

    /// Stop trusting the passkey `code` names. Returns `false` when none is trusted.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn remove_passkey(&mut self, code: DeviceCode) -> Result<bool, StoreError> {
        let mut updated = self.record.clone();
        updated.passkeys.retain(|held| held.passkey.code() != code);
        if updated.passkeys.len() == self.record.passkeys.len() {
            return Ok(false);
        }
        if let Some(snapshot) = updated.trusted_snapshot.as_mut() {
            snapshot.remove_key(code);
        }
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(true)
    }

    /// Trust `trusted` to approve workflows. Returns `false` when a passkey with its code
    /// is already trusted, which keeps its first approval.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn approve_passkey(
        &mut self,
        trusted: ployz_core::authority::TrustedPasskey,
        evidence: Option<super::Evidence>,
    ) -> Result<bool, StoreError> {
        let code = trusted.passkey.code();
        if self
            .record
            .passkeys
            .iter()
            .any(|held| held.passkey.code() == code)
        {
            return Ok(false);
        }
        let mut updated = self.record.clone();
        updated.passkeys.push(trusted);
        if let (Some(evidence), Some(snapshot)) = (evidence, updated.trusted_snapshot.as_mut()) {
            snapshot.add_key(code, evidence);
        }
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(true)
    }

    /// Replace the push target in one write. An unchanged target skips the write.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn set_push_target(&mut self, url: Option<Url>) -> Result<(), StoreError> {
        if self.record.push_target == url {
            return Ok(());
        }
        let mut updated = self.record.clone();
        updated.push_target = url;
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    /// Pin approved members so removing the Device that signed them keeps them peered.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub(crate) fn pin_machines(&mut self, machines: &[Machine]) -> Result<(), StoreError> {
        let mut updated = self.record.clone();
        updated.trusted_machines.extend(
            machines
                .iter()
                .filter(|machine| {
                    self.record.accepts_machine(machine) && !self.record.pins_machine(machine)
                })
                .map(Into::into),
        );
        if updated.trusted_machines.len() == self.record.trusted_machines.len() {
            return Ok(());
        }
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    /// Trust `starting` as Members beside those already trusted, and filter peers by them.
    /// A record that did not filter peers trusts exactly `starting` from now on.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    #[cfg(test)]
    pub(crate) fn trust_members(
        &mut self,
        starting: Vec<ployz_core::authority::Member>,
    ) -> Result<(), StoreError> {
        let mut updated = self.record.clone();
        if !self.record.filters_peers {
            updated.trusted_machines.clear();
            updated.membership_cluster =
                SealingKey::load(&self.data_dir)?.map(|key| key.public_key());
        }
        updated.filters_peers = true;
        for member in starting {
            if !updated.trusted_machines.contains(&member) {
                updated.trusted_machines.push(member);
            }
        }
        if updated == self.record {
            return Ok(());
        }
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    /// Drop `code`'s evidence, as a person removing the key asked.
    fn forget_evidence(&mut self, code: DeviceCode) -> Result<(), StoreError> {
        if !self
            .record
            .trusted_snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.holds_key(code))
        {
            return Ok(());
        }
        let mut updated = self.record.clone();
        if let Some(snapshot) = updated.trusted_snapshot.as_mut() {
            snapshot.remove_key(code);
        }
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    /// Commit an activation in one write: enforce, trust exactly the set's Members, put
    /// them and the kept keys in the snapshot, and retire every Device slot and passkey
    /// outside it with a Cleared tombstone for each retired Device. Only the boot commit
    /// calls this, in a process that has served nothing yet.
    ///
    /// # Errors
    /// Returns a storage error if the record cannot be saved atomically.
    pub(crate) fn commit_activation(
        &mut self,
        pending: &super::local_machine::PendingActivation,
    ) -> Result<super::local_machine::Retired, StoreError> {
        let mut updated = self.record.clone();
        let snapshot = updated
            .trusted_snapshot
            .get_or_insert_with(super::TrustedSnapshot::default);
        for member in &pending.members {
            snapshot.add_member(member.clone());
        }
        for (code, evidence) in pending.kept() {
            snapshot.add_key(code, evidence);
        }
        let keeps = |code: DeviceCode| snapshot.holds_key(code);
        let mut retired = super::local_machine::Retired::default();
        for (label, slot) in &mut updated.management_clients {
            let Some(code) = DeviceCode::named_by(label) else {
                continue;
            };
            let Some(live) = slot.live() else {
                continue;
            };
            if !keeps(code) {
                *slot = ManagementClientSlot::Cleared { was: live };
                retired.devices.push(code);
            }
        }
        updated.passkeys.retain(|held| {
            let code = held.passkey.code();
            let kept = keeps(code);
            if !kept {
                retired.passkeys.push(code);
            }
            kept
        });
        updated.filters_peers = true;
        updated.trusted_machines = pending.members.clone();
        if updated.membership_cluster.is_none() {
            updated.membership_cluster =
                SealingKey::load(&self.data_dir)?.map(|key| key.public_key());
        }
        updated.activation = Some(pending.operation);
        updated.activated_at = Some(ployz_core::authority::Timestamp::now());
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(retired)
    }

    fn persist_management_clients(
        &mut self,
        change: impl FnOnce(&mut BTreeMap<ManagementClientLabel, ManagementClientSlot>),
    ) -> Result<(), StoreError> {
        let mut updated = self.record.clone();
        change(&mut updated.management_clients);
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(())
    }

    pub fn complete_reset(&self) -> Result<(), StoreError> {
        if self.record.phase() != LocalMachinePhase::Resetting {
            return Err(StoreError::NotResetting);
        }
        let persisted: LocalMachineRecord =
            serde_json::from_slice(&fs::read(path(&self.data_dir))?)?;
        if persisted != self.record {
            return Err(StoreError::OwnershipLost(self.data_dir.clone()));
        }
        // The record goes last. A cleanup that stops partway, as one killed at the
        // unit's stop timeout does, leaves a directory that still opens as
        // Resetting, so the next start finishes the reset instead of refusing an
        // unowned directory.
        for entry in fs::read_dir(&self.data_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == STATE_FILE_NAME || name == LOCK_FILE_NAME || name == PREPARE_DIR_NAME {
                continue;
            }
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        File::open(&self.data_dir)?.sync_all()?;
        fs::remove_file(path(&self.data_dir))?;
        if self.data_dir.join(PREPARE_DIR_NAME).exists() {
            fs::remove_file(self.data_dir.join(LOCK_FILE_NAME))?;
            return Ok(());
        }
        fs::remove_dir_all(&self.data_dir).map_err(StoreError::Io)
    }
}

fn validate_data_dir(path: &Path) -> Result<(), StoreError> {
    if path.file_name().is_none()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(StoreError::UnsafeDataDirectory(path.to_owned()));
    }
    Ok(())
}

fn claim_data_dir(data_dir: &Path) -> Result<(), StoreError> {
    match fs::read_dir(data_dir) {
        Ok(entries) => {
            let names = entries
                .map(|entry| Ok(entry?.file_name()))
                .collect::<io::Result<Vec<_>>>()?;
            let owns_directory = names.iter().any(|name| name == STATE_FILE_NAME)
                || names.iter().all(|name| {
                    name == TEMPORARY_FILE_NAME
                        || name == LOCK_FILE_NAME
                        || name == PREPARE_DIR_NAME
                });
            if owns_directory {
                Ok(())
            } else {
                Err(StoreError::UnownedDataDirectory(data_dir.to_owned()))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Mixed-version handling, deletable once no Machine runs a record from before Devices
/// were Full-only. Until then a daemon accepted any label from any Management Client, so
/// Cloud could write a `device-<code>` slot that [`ManagementScope::of`] now reads as
/// Full. The first open by this daemon tombstones every live `device-` slot and marks
/// the record vetted in the same write, so Devices approved afterwards stay approved.
///
/// [`ManagementScope::of`]: ployz_core::ManagementScope::of
fn vet_device_slots(
    data_dir: &Path,
    mut record: LocalMachineRecord,
) -> Result<LocalMachineRecord, StoreError> {
    if record.device_slots_vetted {
        return Ok(record);
    }
    for (label, slot) in &mut record.management_clients {
        if DeviceCode::reserves(label)
            && let ManagementClientSlot::Live(was) = *slot
        {
            *slot = ManagementClientSlot::Cleared { was };
        }
    }
    record.device_slots_vetted = true;
    save(data_dir, &record)?;
    Ok(record)
}

/// Where the Machine record lives under `data_dir`.
pub(crate) fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE_NAME)
}

/// What the observe process learns about this Machine from its record.
pub(crate) struct MachineIdentity {
    pub(crate) id: MachineId,
    pub(crate) push_target: Option<Url>,
    pub(crate) management_secret: ManagementSecret,
}

/// Read the record fresh, without the store's lock: a reset or Cloud setting a
/// push target changes what the next read returns. None while the record is
/// missing or unreadable.
pub(crate) fn identity(data_dir: &Path) -> Option<MachineIdentity> {
    let bytes = fs::read(path(data_dir)).ok()?;
    let record = serde_json::from_slice::<LocalMachineRecord>(&bytes).ok()?;
    Some(MachineIdentity {
        id: record.id(),
        push_target: record.push_target().cloned(),
        management_secret: record.management_secret().clone(),
    })
}

fn save(data_dir: &Path, record: &LocalMachineRecord) -> Result<(), StoreError> {
    fs::create_dir_all(data_dir)?;
    fs::set_permissions(data_dir, fs::Permissions::from_mode(0o711))?;

    let temporary = data_dir.join(TEMPORARY_FILE_NAME);
    write_record(&temporary, record)?;
    fs::rename(temporary, path(data_dir))?;
    File::open(data_dir)?.sync_all()?;
    Ok(())
}

fn write_record(path: &Path, record: &LocalMachineRecord) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, record)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn display_data_directory(path: &Path) -> String {
    let mut text = String::new();
    for chunk in path.as_os_str().as_encoded_bytes().utf8_chunks() {
        text.extend(chunk.valid().escape_debug());
        text.extend(chunk.invalid().escape_ascii().map(char::from));
    }
    text
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("join approval refused: {0}")]
    JoinRefused(ployz_core::authority::AuthorityRefusal),
    #[error("local Machine record I/O failed")]
    Io(#[from] io::Error),
    #[error("local Machine record JSON is invalid")]
    Json(#[from] serde_json::Error),
    #[error("machine is not participating")]
    NotParticipating,
    #[error("machine is already resetting")]
    AlreadyResetting,
    #[error("machine is not resetting")]
    NotResetting,
    #[error("machine is not joining")]
    NotJoining,
    #[error("machine is already initialized")]
    AlreadyInitialized,
    #[error("only a founding Machine recovers a Sealing Key")]
    RecoveryWithoutFounding,
    #[error("at least one advertised endpoint is required")]
    MissingEndpoints,
    #[error("at least one visible peer is required to join")]
    MissingPeers,
    #[error("assigned public key does not match this Machine")]
    KeyMismatch,
    #[error("assignment does not match the local durable Machine ID")]
    IdentityMismatch,
    #[error(transparent)]
    SealingKey(#[from] OpenError),
    #[error("invalid Cluster network: {0}")]
    InvalidNetwork(String),
    #[error(transparent)]
    MachineUpdate(#[from] MachineUpdateError),
    #[error("another daemon already owns data directory {}", display_data_directory(.0))]
    AlreadyRunning(PathBuf),
    #[error("refusing to clear broad data directory {}", display_data_directory(.0))]
    UnsafeDataDirectory(PathBuf),
    #[error("refusing to claim nonempty data directory {}", display_data_directory(.0))]
    UnownedDataDirectory(PathBuf),
    #[error("local Machine record changed before clearing data directory {}", display_data_directory(.0))]
    OwnershipLost(PathBuf),
    #[error("local Machine record changed before prepared reset was committed in {}", display_data_directory(.0))]
    ResetPreparationLost(PathBuf),
    #[error("a reset of data directory {} did not finish; start the ployz daemon to finish it", display_data_directory(.0))]
    ResetInterrupted(PathBuf),
}

#[cfg(test)]
mod plan_enroll_tests {
    use ployz_core::{
        AdvertisedEndpoint, LocalMachinePhase, Machine, MachineId, MachineName, MachineNow,
        WireGuardPublicKey,
    };

    use super::{EnrollPlan, plan_enroll};

    fn wanted(id: &MachineId) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: *id,
            name: MachineName::parse("edge").unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            runtime: Default::default(),
            join_approval: None,
            build_concurrency: None,
        }
    }

    #[test]
    fn plan_enroll_replays_only_the_same_record_and_mtu() {
        let id = MachineId::random();
        let wanted = wanted(&id);
        let mut renamed = wanted.clone();
        renamed.name = MachineName::parse("other").unwrap();
        let now = |phase, machine: Option<&Machine>| MachineNow {
            id,
            phase,
            machine: machine.cloned(),
        };
        let participating = now(LocalMachinePhase::Participating, Some(&wanted));
        let joining = now(LocalMachinePhase::Joining, Some(&wanted));
        let different = now(LocalMachinePhase::Participating, Some(&renamed));
        let resetting = now(LocalMachinePhase::Resetting, None);
        let cases = [
            (
                "fresh",
                now(LocalMachinePhase::Uninitialized, None),
                None,
                EnrollPlan::Fresh,
            ),
            (
                "same record replays",
                participating.clone(),
                Some(1400),
                EnrollPlan::Replay(wanted.clone()),
            ),
            (
                "same joining record replays",
                joining,
                Some(1400),
                EnrollPlan::Replay(wanted.clone()),
            ),
            (
                "different record refused",
                different.clone(),
                Some(1400),
                EnrollPlan::Conflict(different),
            ),
            (
                "MTU differs",
                participating.clone(),
                None,
                EnrollPlan::Conflict(participating),
            ),
            (
                "resetting refused",
                resetting.clone(),
                Some(1400),
                EnrollPlan::Conflict(resetting),
            ),
        ];
        for (case, now, stored_mtu, expected) in cases {
            assert_eq!(
                plan_enroll(&now, stored_mtu, &wanted, Some(1400)),
                expected,
                "{case}"
            );
        }
    }
}

#[cfg(test)]
mod rediscover_tests {
    use std::net::IpAddr;

    use ployz_core::{
        AdvertisedEndpoint, EnrollAs, EnrollRequest, MachineName, MachineUpdate, PinnedAddresses,
        PublicIpUpdate,
    };

    use super::{LocalMachineStore, Rediscovered, STATE_FILE_NAME};
    use crate::network::DiscoveredNetwork;

    const OLD_IP: &str = "198.51.100.7";
    const NEW_IP: &str = "203.0.113.9";

    fn ip(address: &str) -> IpAddr {
        address.parse().unwrap()
    }

    fn endpoints(addresses: &[&str]) -> Vec<AdvertisedEndpoint> {
        addresses
            .iter()
            .map(|address| AdvertisedEndpoint(std::net::SocketAddr::new(ip(address), 51820)))
            .collect()
    }

    fn founded(pinned: PinnedAddresses) -> (tempfile::TempDir, LocalMachineStore) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = LocalMachineStore::open(dir.path().join("data")).unwrap();
        store
            .enroll(EnrollRequest {
                role: EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("edge").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: Some(ip(OLD_IP)),
                    advertised_endpoints: endpoints(&["10.0.0.4", OLD_IP]),
                    grant: None,
                },
                wireguard_mtu: None,
                pinned,
            })
            .unwrap();
        (dir, store)
    }

    fn moved_host() -> DiscoveredNetwork {
        DiscoveredNetwork {
            public_ip: Some(ip(NEW_IP)),
            endpoints: endpoints(&["10.0.0.5", NEW_IP]),
            ipv4_route: true,
        }
    }

    fn reopened(dir: &tempfile::TempDir, store: LocalMachineStore) -> LocalMachineStore {
        drop(store);
        LocalMachineStore::open(dir.path().join("data")).unwrap()
    }

    #[test]
    fn stale_discovered_addresses_are_replaced_and_persisted() {
        let (dir, mut store) = founded(PinnedAddresses::default());

        let outcome = store.rediscover(moved_host()).unwrap();

        let store = reopened(&dir, store);
        let machine = store.record().machine().unwrap().clone();
        assert_eq!(outcome, Rediscovered::Updated(Box::new(machine.clone())));
        assert_eq!(machine.public_ip, Some(ip(NEW_IP)));
        assert_eq!(machine.advertised_endpoints, moved_host().endpoints);
        assert_eq!(
            store.record().pinned_addresses(),
            PinnedAddresses::default()
        );
    }

    #[test]
    fn unchanged_discovery_writes_nothing() {
        let (_dir, mut store) = founded(PinnedAddresses::default());
        let same = DiscoveredNetwork {
            public_ip: Some(ip(OLD_IP)),
            endpoints: endpoints(&["10.0.0.4", OLD_IP]),
            ipv4_route: true,
        };

        assert_eq!(store.rediscover(same).unwrap(), Rediscovered::Unchanged);
    }

    #[test]
    fn addresses_chosen_at_enrollment_survive_rediscovery() {
        let (_dir, mut store) = founded(PinnedAddresses {
            public_ip: true,
            advertised_endpoints: true,
        });

        assert_eq!(
            store.rediscover(moved_host()).unwrap(),
            Rediscovered::Unchanged
        );
        let machine = store.record().machine().unwrap();
        assert_eq!(machine.public_ip, Some(ip(OLD_IP)));
        assert_eq!(
            machine.advertised_endpoints,
            endpoints(&["10.0.0.4", OLD_IP])
        );
    }

    #[test]
    fn server_set_pins_only_the_address_it_sets() {
        let (dir, mut store) = founded(PinnedAddresses::default());
        store
            .update(
                MachineUpdate {
                    public_ip: PublicIpUpdate::Remove,
                    ..MachineUpdate::default()
                },
                &[],
            )
            .unwrap();
        let mut store = reopened(&dir, store);

        store.rediscover(moved_host()).unwrap();

        let machine = store.record().machine().unwrap();
        assert_eq!(machine.public_ip, None);
        assert_eq!(machine.advertised_endpoints, moved_host().endpoints);
    }

    #[test]
    fn a_failed_public_ip_lookup_keeps_the_stored_row() {
        let (_dir, mut store) = founded(PinnedAddresses::default());
        let before = store.record().machine().unwrap().clone();

        let outcome = store
            .rediscover(DiscoveredNetwork {
                public_ip: None,
                endpoints: endpoints(&["10.0.0.5"]),
                ipv4_route: true,
            })
            .unwrap();

        assert_eq!(outcome, Rediscovered::PublicIpLookupFailed);
        assert_eq!(store.record().machine(), Some(&before));
    }

    #[test]
    fn a_server_without_an_ipv4_route_drops_the_ipv4_public_ip_it_stored() {
        // As a NAT64 gateway's address, stored before the lookup knew better.
        let (dir, mut store) = founded(PinnedAddresses::default());
        let own = endpoints(&["2001:db8::10"]);

        let outcome = store
            .rediscover(DiscoveredNetwork {
                public_ip: None,
                endpoints: own.clone(),
                ipv4_route: false,
            })
            .unwrap();

        let store = reopened(&dir, store);
        let machine = store.record().machine().unwrap().clone();
        assert_eq!(outcome, Rediscovered::Updated(Box::new(machine.clone())));
        assert_eq!(machine.public_ip, None);
        assert_eq!(machine.advertised_endpoints, own);
    }

    /// A record as `daemon_version` left it, with nothing pinned on disk.
    fn rewritten_without_pins(
        dir: &tempfile::TempDir,
        store: LocalMachineStore,
        daemon_version: &str,
    ) -> LocalMachineStore {
        drop(store);
        let path = dir.path().join("data").join(STATE_FILE_NAME);
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            json.as_object_mut()
                .unwrap()
                .remove("pinned_addresses")
                .is_some()
        );
        *json
            .pointer_mut("/body/machine/runtime/daemon_version")
            .unwrap() = daemon_version.into();
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        LocalMachineStore::open(dir.path().join("data")).unwrap()
    }

    #[test]
    fn addresses_from_before_pins_existed_survive_rediscovery() {
        let (dir, store) = founded(PinnedAddresses::default());
        let mut store = rewritten_without_pins(&dir, store, "0.3.0-beta.6");

        assert_eq!(
            store.rediscover(moved_host()).unwrap(),
            Rediscovered::Unchanged
        );
        let store = reopened(&dir, store);
        let machine = store.record().machine().unwrap();
        assert_eq!(machine.public_ip, Some(ip(OLD_IP)));
        assert_eq!(
            machine.advertised_endpoints,
            endpoints(&["10.0.0.4", OLD_IP])
        );
        assert_eq!(
            store.record().pinned_addresses(),
            PinnedAddresses {
                public_ip: true,
                advertised_endpoints: true,
            }
        );
    }

    #[test]
    fn a_pinning_daemons_record_without_pins_is_rediscovered() {
        let (dir, store) = founded(PinnedAddresses::default());
        let mut store = rewritten_without_pins(&dir, store, "0.3.0-beta.7");

        assert!(matches!(
            store.rediscover(moved_host()).unwrap(),
            Rediscovered::Updated(_)
        ));
    }
}

#[cfg(test)]
mod device_slot_tests {
    use ployz_core::{DeviceKey, EnrollAs, EnrollRequest, MachineName, ManagementScope};

    use super::{LocalMachineStore, STATE_FILE_NAME};

    fn key() -> DeviceKey {
        DeviceKey::from_bytes([7; 32])
    }

    fn founded() -> (tempfile::TempDir, LocalMachineStore) {
        let dir = tempfile::tempdir().unwrap();
        let mut store = LocalMachineStore::open(dir.path().join("data")).unwrap();
        store
            .enroll(EnrollRequest {
                role: EnrollAs::Founder {
                    initial_policy: Default::default(),
                    name: MachineName::parse("edge").unwrap(),
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: Some("198.51.100.7".parse().unwrap()),
                    advertised_endpoints: vec![ployz_core::AdvertisedEndpoint(
                        "198.51.100.7:51820".parse().unwrap(),
                    )],
                    grant: None,
                },
                wireguard_mtu: None,
                pinned: Default::default(),
            })
            .unwrap();
        store.approve_device(key(), None).unwrap();
        (dir, store)
    }

    fn reopened(dir: &tempfile::TempDir, store: LocalMachineStore) -> LocalMachineStore {
        drop(store);
        LocalMachineStore::open(dir.path().join("data")).unwrap()
    }

    fn reopened_unvetted(dir: &tempfile::TempDir, store: LocalMachineStore) -> LocalMachineStore {
        drop(store);
        let path = dir.path().join("data").join(STATE_FILE_NAME);
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(
            json.as_object_mut()
                .unwrap()
                .remove("device_slots_vetted")
                .is_some()
        );
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
        LocalMachineStore::open(dir.path().join("data")).unwrap()
    }

    #[test]
    fn a_device_slot_from_before_vetting_stops_granting_access() {
        let (dir, store) = founded();

        let store = reopened_unvetted(&dir, store);

        let record = store.record();
        assert_eq!(record.management_scope(&key().into()), None);
        assert_eq!(record.devices().count(), 0);
        assert!(record.clears_management_client(&key().into()));
    }

    #[test]
    fn vetting_runs_once_so_a_later_approval_survives_reopening() {
        let (dir, store) = founded();
        let mut store = reopened_unvetted(&dir, store);
        assert!(store.approve_device(key(), None).unwrap());

        let store = reopened(&dir, store);

        assert_eq!(
            store.record().management_scope(&key().into()),
            Some(ManagementScope::Full)
        );
        assert_eq!(store.record().devices().collect::<Vec<_>>(), vec![key()]);
    }

    #[test]
    fn an_approved_device_survives_reopening() {
        let (dir, store) = founded();

        let store = reopened(&dir, store);

        assert_eq!(
            store.record().management_scope(&key().into()),
            Some(ManagementScope::Full)
        );
    }
}
