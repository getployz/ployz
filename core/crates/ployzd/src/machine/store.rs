//! Durable Local Machine record ownership and persistence.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use ployz_core::{
    ClientKey, DeviceCode, DeviceKey, EnrollAs, EnrollRequest, LocalMachinePhase, Machine,
    MachineId, MachineNow, MachineUpdate, MachineUpdateError, ManagementClientLabel,
    PREPARE_DIR_NAME, PinnedAddresses, PublicIpUpdate, RecoverySeed, Registered, Retired,
    SelectedEndpoint, apply_machine_update,
    authority::{KeyRef, SigningPublicKey},
};
use reqwest::Url;
use thiserror::Error;

use super::{
    FoundingCluster, LiveSlot, LocalMachineBody, LocalMachineRecord, ManagementClientSlot,
    ParticipationOrigin, RemovedMember,
};
use crate::management::ManagementSecret;
use crate::network::{DiscoveredNetwork, WireGuardPrivateKey, allocate_machine_subnet};
use crate::sealing_key::{Floors, KeyFile, OpenError, SealingKey};

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
    /// The floors sealed beside the Sealing Key, which the joiner takes before the key.
    floors: Floors,
    keys: ployz_core::authority::TrustedKeys,
    wireguard_mtu: Option<u32>,
    pinned: PinnedAddresses,
    enrollment: EnrollRequest,
}

/// What an enrollment installs: the body, the Cluster's key with the floors sealed beside
/// it, and the keys its approval names.
type Enrolled = (
    LocalMachineBody,
    Option<SealingKey>,
    Floors,
    ployz_core::authority::TrustedKeys,
);

impl Installation {
    pub(crate) fn machine(&self) -> Option<&Machine> {
        self.body.machine()
    }

    /// The Cluster whose Sealing Key this installs, and the floors sealed beside it.
    pub(crate) fn floors(&self) -> Option<(ployz_core::SealingPublicKey, &Floors)> {
        self.sealing_key
            .as_ref()
            .map(|key| (key.public_key(), &self.floors))
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
        Ok(Opened::Ready(store))
    }

    /// Run `edit` on a copy of the record and keep what it left: the file is written even
    /// when the copy equals the record, which recreates a file that went missing, and a
    /// refusal from `edit` or a failed write leaves the record, and the file, as they were.
    /// The store holds the copy only once the file does.
    fn commit<R, E: From<StoreError>>(
        &mut self,
        edit: impl FnOnce(&mut LocalMachineRecord) -> Result<R, E>,
    ) -> Result<R, E> {
        let mut updated = self.record.clone();
        let answer = edit(&mut updated)?;
        save(&self.data_dir, &updated)?;
        self.record = updated;
        Ok(answer)
    }

    /// As [`Self::commit`], except a copy equal to the record writes nothing.
    fn commit_if_changed<R, E: From<StoreError>>(
        &mut self,
        edit: impl FnOnce(&mut LocalMachineRecord) -> Result<R, E>,
    ) -> Result<R, E> {
        let mut updated = self.record.clone();
        let answer = edit(&mut updated)?;
        if updated != self.record {
            save(&self.data_dir, &updated)?;
            self.record = updated;
        }
        Ok(answer)
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
        let runtime = crate::host_facts::local_runtime_blocking(&self.data_dir);
        self.commit_if_changed(|record| {
            if let LocalMachineBody::Joining { machine, .. }
            | LocalMachineBody::Participating { machine, .. } = &mut record.body
            {
                machine.runtime = runtime;
            }
            Ok(())
        })
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
        let now = ployz_core::authority::Timestamp::now();
        let (plan, installation) = self.plan_enrollment(request, recovery, now)?;
        if matches!(plan, EnrollPlan::Fresh) {
            self.install(installation, now)?;
        }
        Ok(plan)
    }

    /// The Installation this request would persist, and whether this Machine
    /// takes it fresh, already holds it, or holds another enrollment. `now` is this
    /// Machine's floored clock, which a Member's join approval must be open at.
    pub(crate) fn plan_enrollment(
        &self,
        request: EnrollRequest,
        recovery: Option<RecoverySeed>,
        now: ployz_core::authority::Timestamp,
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
                    floors: Floors::new(),
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
        let (body, sealing_key, floors, keys) = match role {
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
                    Floors::new(),
                    ployz_core::authority::TrustedKeys::default(),
                )
            }
            EnrollAs::Member { .. } if recovery.is_some() => {
                return Err(StoreError::RecoveryWithoutFounding);
            }
            EnrollAs::Member { registration, pin } => self.member(registration, pin, now)?,
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
                floors,
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
            runtime: crate::host_facts::local_runtime_blocking(&self.data_dir),
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
    /// Device the approval lists. A pinned join trusts only the pinned Devices the approval
    /// names, and no passkey or grant. Anything else is refused and the record stays as it was.
    fn member(
        &self,
        registration: Registered,
        pin: Option<ployz_core::JoinPin>,
        now: ployz_core::authority::Timestamp,
    ) -> Result<Enrolled, StoreError> {
        let Registered {
            assigned_machine: mut machine,
            mut visible_peers,
            target_versions,
            sealing_key,
            devices: _,
            grants: _,
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
        let (sealing_key, approval, floors) = match opened {
            Some((key, approval, floors)) => (Some(key), approval, floors),
            None => (None, None, Floors::new()),
        };
        let Some(approval) = approval else {
            return Err(StoreError::JoinRefused(
                ployz_core::authority::AuthorityRefusal::Unsigned {
                    purpose: ployz_core::authority::Purpose::Join,
                },
            ));
        };
        // A pinned join's body may come through Cloud, which can name a Device, passkey or
        // grant of its own beside the person's. The install line already runs Cloud's script.
        let carries_passkeys = pin.is_none();
        let ployz_core::authority::join::JoinerApproval { body, devices } =
            ployz_core::authority::join::verify_as_joiner(
                &approval,
                pin.as_ref().map(|pin| pin.devices.as_slice()),
                &machine,
                sealing_key.as_ref().map(SealingKey::public_key),
                now,
            )
            .map_err(StoreError::JoinRefused)?;
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
        let approved_by = match body.signer {
            ployz_core::authority::SignerKey::Device(signer) => {
                ployz_core::authority::PasskeyApprover::Device {
                    code: signer.code(),
                }
            }
            ployz_core::authority::SignerKey::Passkey(_) => {
                ployz_core::authority::PasskeyApprover::Unknown
            }
        };
        let mut approved_passkeys: Vec<ployz_core::authority::TrustedPasskey> = Vec::new();
        for passkey in body.passkeys.iter().filter(|_| carries_passkeys) {
            let code = passkey.code();
            let held = approved_passkeys
                .iter()
                .any(|held| held.passkey.code() == code)
                || body.devices.iter().any(|device| device.code() == code);
            if !held {
                approved_passkeys.push(ployz_core::authority::TrustedPasskey {
                    passkey: passkey.clone(),
                    approved_by,
                });
            }
        }
        let approved_grants: Vec<ployz_core::authority::TrustedGrant> = body
            .grants
            .iter()
            .filter(|_| carries_passkeys)
            .map(|grant| ployz_core::authority::TrustedGrant {
                grant: grant.clone(),
                approved_by,
            })
            .collect();
        machine.join_approval = Some(approval);
        machine.runtime = crate::host_facts::local_runtime_blocking(&self.data_dir);
        Ok((
            LocalMachineBody::Joining {
                machine,
                bootstrap: visible_peers,
                min_store_version: target_versions,
            },
            sealing_key,
            floors,
            ployz_core::authority::TrustedKeys::new(devices, approved_passkeys, approved_grants),
        ))
    }

    /// Persist a fresh enrollment. A join's approval, and the bundle it rides in, must
    /// still be open at `now`, this Machine's floored clock, so work done since the plan
    /// can't carry an approval past its window.
    pub(crate) fn install(
        &mut self,
        installation: Installation,
        now: ployz_core::authority::Timestamp,
    ) -> Result<(), StoreError> {
        self.require_uninitialized()?;
        if let LocalMachineBody::Joining { machine, .. } = &installation.body {
            let approval = machine
                .join_approval
                .as_ref()
                .ok_or(StoreError::JoinRefused(
                    ployz_core::authority::AuthorityRefusal::Unsigned {
                        purpose: ployz_core::authority::Purpose::Join,
                    },
                ))?;
            ployz_core::authority::join::windows(approval, now).map_err(StoreError::JoinRefused)?;
        }
        self.set_sealing_key(installation.sealing_key.as_ref())?;
        self.commit(|installed| {
            installed.body = installation.body;
            installed.filters_peers = true;
            let joins = installed.phase() == LocalMachinePhase::Joining;
            installed.trusted_machines =
                installed.body.bootstrap().iter().map(Into::into).collect();
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
                installed.passkeys = installation.keys.passkeys().cloned().collect();
                installed.grants = installation.keys.grants().cloned().collect();
            }
            installed
                .management_clients
                .extend(installation.keys.devices().map(|key| {
                    (
                        key.label(),
                        ManagementClientSlot::Live(LiveSlot::Active {
                            accepted: ClientKey::from(key),
                        }),
                    )
                }));
            Ok(())
        })
    }

    pub(crate) fn set_sealing_key(&mut self, key: Option<&SealingKey>) -> io::Result<()> {
        SealingKey::set(&self.data_dir, key)?;
        if self.record.filters_peers && self.record.membership_cluster.is_none() {
            self.commit::<_, StoreError>(|record| {
                record.membership_cluster = key.map(SealingKey::public_key);
                Ok(())
            })
            .map_err(io::Error::other)?;
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
        self.commit(|record| {
            let LocalMachineBody::Participating { machine, .. } = &mut record.body else {
                return Err(StoreError::NotParticipating);
            };
            let updated = apply_machine_update(machine, visible, update)?;
            *machine = updated.clone();
            record.pinned_addresses = pinned;
            Ok(updated)
        })
    }

    fn require_uninitialized(&self) -> Result<(), StoreError> {
        if matches!(self.record.body, LocalMachineBody::Uninitialized { .. }) {
            Ok(())
        } else {
            Err(StoreError::AlreadyInitialized)
        }
    }

    pub fn complete_catch_up(&mut self) -> Result<(), StoreError> {
        self.commit(|record| {
            let LocalMachineBody::Joining {
                machine, bootstrap, ..
            } = record.body.clone()
            else {
                return Err(StoreError::NotJoining);
            };
            record.body = LocalMachineBody::Participating {
                machine,
                origin: ParticipationOrigin::Join { bootstrap },
            };
            Ok(())
        })
    }

    pub fn persist_selected_endpoint(
        &mut self,
        machine_id: MachineId,
        endpoint: SelectedEndpoint,
    ) -> Result<(), StoreError> {
        self.commit(|record| {
            record.selected_endpoints.insert(machine_id, endpoint);
            Ok(())
        })
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
        self.commit(|record| {
            record.management_clients.insert(label, slot);
            Ok(())
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
        self.commit(|record| {
            record
                .management_clients
                .insert(label.clone(), ManagementClientSlot::Cleared { was });
            Ok(())
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
        self.commit(|record| {
            record.management_clients.insert(
                label,
                ManagementClientSlot::Live(LiveSlot::Active { accepted: remote }),
            );
            Ok(())
        })
    }

    /// Admit `key` as an accepted client in the slot its [`DeviceCode`] names, in one
    /// write. Returns `false`, without a write, when the slot already holds exactly it.
    ///
    /// # Errors
    /// Returns [`StoreError::KeyRemoved`] for a key this Machine removed, and a storage
    /// error if the updated record cannot be saved atomically.
    pub fn approve_device(&mut self, key: DeviceKey) -> Result<bool, StoreError> {
        if self.record.removes_key(&KeyRef::Signing { key }) {
            return Err(StoreError::KeyRemoved(KeyRef::Signing { key }));
        }
        let label = key.label();
        let slot = ManagementClientSlot::Live(LiveSlot::Active {
            accepted: ClientKey::from(key),
        });
        let held = self.record.management_clients.get(&label) == Some(&slot);
        self.commit_if_changed::<_, StoreError>(|record| {
            record.management_clients.insert(label, slot);
            Ok(())
        })?;
        Ok(!held)
    }

    /// Remove the key `code` names, as [`Self::remove_key`] does: the Device its slot holds,
    /// or the trusted passkey or grant with that code. Returns whether any held it.
    ///
    /// # Errors
    /// Returns [`StoreError::AmbiguousCode`] when `code` names more than one key here, and
    /// otherwise as [`Self::remove_key`].
    pub fn remove_by_code(&mut self, code: DeviceCode) -> Result<bool, StoreError> {
        let device = self
            .record
            .management_clients
            .get(&code.label())
            .and_then(|slot| slot.accepted())
            .map(|accepted| KeyRef::Signing {
                key: DeviceKey::from_bytes(*accepted.as_bytes()),
            });
        let passkeys = self
            .record
            .passkeys
            .iter()
            .filter(|held| held.passkey.code() == code)
            .map(|held| KeyRef::Passkey {
                key: held.passkey.public_key,
            });
        let grants = self
            .record
            .grants
            .iter()
            .filter(|held| held.grant.key.code() == code)
            .map(|held| KeyRef::Signing {
                key: held.grant.key.into(),
            });
        let mut named: Vec<KeyRef> = device.into_iter().chain(passkeys).chain(grants).collect();
        named.dedup();
        match named.as_slice() {
            [] => Ok(false),
            [key] => self.remove_key(*key),
            _ => Err(StoreError::AmbiguousCode(code)),
        }
    }

    /// Stop trusting `key` for good, in one write: drop it from the trusted keys, clearing a
    /// Device's slot or dropping a grant, and add it to the removed keys, so no later
    /// approval trusts it here again. A key this Machine never trusted is still marked
    /// removed. Returns whether it was trusted.
    ///
    /// # Errors
    /// Returns [`StoreError::LastKey`] when `key` is the last person's or full CLI key this
    /// Machine trusts, which alone change keys, and a storage error if the updated record
    /// cannot be saved atomically.
    pub fn remove_key(&mut self, key: KeyRef) -> Result<bool, StoreError> {
        self.remove(key, false)
    }

    /// Stop trusting the CI or agent key `key` for good, as [`Self::remove_key`] does, and
    /// keep that it was removed as one. With `retired`, only while `key` holds the grant
    /// issued when the rotation read it and the new key holds its grant beside it, checked
    /// in the same write, so a change made after that read never lands only to be lost.
    /// A key a removal from before typed tombstones already removed stays as it is.
    ///
    /// # Errors
    /// Returns [`StoreError::NotGrant`] when `key` is a Device here,
    /// [`StoreError::GrantMoved`] when either grant `retired` names isn't the one held
    /// here, and what [`Self::remove_key`] returns.
    pub fn remove_grant(
        &mut self,
        key: SigningPublicKey,
        retired: Option<Retired>,
    ) -> Result<bool, StoreError> {
        let removed = KeyRef::Signing { key: key.into() };
        if self.record.devices().any(|device| device == key.into()) {
            return Err(StoreError::NotGrant(key.code()));
        }
        let issued = |key: SigningPublicKey| {
            self.record
                .grants
                .iter()
                .find(|held| held.grant.key == key)
                .map(|held| held.grant.issued_at)
        };
        if let Some(retired) = retired {
            if issued(key) != Some(retired.issued_at) {
                return Err(StoreError::GrantMoved(key.code()));
            }
            if retired.by == key || issued(retired.by) != Some(retired.by_issued_at) {
                return Err(StoreError::GrantMoved(retired.by.code()));
            }
        }
        if self.record.removes_key(&removed) && !self.record.removed_grants.contains(&key) {
            return Ok(false);
        }
        self.remove(removed, true)
    }

    fn remove(&mut self, key: KeyRef, grant: bool) -> Result<bool, StoreError> {
        let trusted = self.record.trusted_keys();
        let holds = |held: &ployz_core::authority::TrustedKey| match (key, held) {
            (KeyRef::Signing { key }, _) => held.signing_key() == Some(key.into()),
            (KeyRef::Passkey { key }, ployz_core::authority::TrustedKey::Passkey(passkey)) => {
                passkey.passkey.public_key == key
            }
            (KeyRef::Passkey { .. }, _) => false,
        };
        let held = trusted.keys().any(holds);
        let whole_cluster = |held: &&ployz_core::authority::TrustedKey| held.kind().whole_cluster();
        if trusted.keys().filter(whole_cluster).any(holds)
            && trusted.keys().filter(whole_cluster).count() == 1
        {
            return Err(StoreError::LastKey(key.code()));
        }
        self.commit_if_changed(|record| {
            if !record.removed_keys.contains(&key) {
                record.removed_keys.push(key);
            }
            match key {
                KeyRef::Signing { key } => {
                    let signing = SigningPublicKey::from(key);
                    let granted = record.grants.iter().any(|held| held.grant.key == signing);
                    if (grant || granted) && !record.removed_grants.contains(&signing) {
                        record.removed_grants.push(signing);
                    }
                    record.grants.retain(|held| held.grant.key != signing);
                    let label = key.label();
                    if let Some(was) = record
                        .management_clients
                        .get(&label)
                        .and_then(|slot| slot.live())
                        .filter(|live| live.accepted() == Some(ClientKey::from(key)))
                    {
                        record
                            .management_clients
                            .insert(label, ManagementClientSlot::Cleared { was });
                    }
                }
                KeyRef::Passkey { key } => {
                    record
                        .passkeys
                        .retain(|held| held.passkey.public_key != key);
                }
            }
            Ok(held)
        })
    }

    /// Trust `trusted` to approve workflows. Returns `false` when a passkey with its code
    /// is already trusted, which keeps its first approval.
    ///
    /// # Errors
    /// Returns [`StoreError::KeyRemoved`] for a passkey this Machine removed, and a storage
    /// error if the updated record cannot be saved atomically.
    pub fn approve_passkey(
        &mut self,
        trusted: ployz_core::authority::TrustedPasskey,
    ) -> Result<bool, StoreError> {
        let code = trusted.passkey.code();
        let removed = KeyRef::Passkey {
            key: trusted.passkey.public_key,
        };
        if self.record.removes_key(&removed) {
            return Err(StoreError::KeyRemoved(removed));
        }
        self.commit_if_changed(|record| {
            let held = record
                .passkeys
                .iter()
                .any(|held| held.passkey.code() == code);
            if !held {
                record.passkeys.push(trusted);
            }
            Ok(!held)
        })
    }

    /// Trust `trusted` as a CI or agent key, edit the grant it holds, or trust it in place
    /// of another key's grant, as `prior` says the signer read this Machine's grants. The
    /// check and the write are one record write, so a narrowing or rotation signed from a
    /// read another change has since moved never lands. Returns whether the record changed.
    ///
    /// - No `prior`: trust a key this Machine holds no grant of. The same bundle again
    ///   changes nothing.
    /// - [`Prior::Edit`]: the key holds the grant a bundle issued at that time set, and the
    ///   new one, issued later, takes its scope, Volumes and approver. The same bundle
    ///   again changes nothing.
    /// - [`Prior::Replace`]: the source key holds the grant a bundle issued at that time
    ///   set, and the new key takes the same kind beside it. A new key already held
    ///   changes nothing, so a rotation resumes with it.
    ///
    /// # Errors
    /// Returns [`StoreError::KeyRemoved`] for a key this Machine removed,
    /// [`StoreError::GrantKind`] for a key granted here as the other kind,
    /// [`StoreError::GrantHeld`] for a trust with no `prior` of a key already granted
    /// here, [`StoreError::GrantMoved`] when the grant `prior` names isn't the one held
    /// here, and a storage error if the updated record cannot be saved atomically.
    ///
    /// [`Prior::Edit`]: ployz_core::authority::Prior::Edit
    /// [`Prior::Replace`]: ployz_core::authority::Prior::Replace
    pub fn approve_grant(
        &mut self,
        trusted: ployz_core::authority::TrustedGrant,
        prior: Option<ployz_core::authority::Prior>,
    ) -> Result<bool, StoreError> {
        use ployz_core::authority::Prior;
        let removed = KeyRef::Signing {
            key: trusted.grant.key.into(),
        };
        if self.record.removes_key(&removed) {
            return Err(StoreError::KeyRemoved(removed));
        }
        let grant_of = |key| {
            self.record
                .grants
                .iter()
                .find(move |held| held.grant.key == key)
        };
        let held = grant_of(trusted.grant.key);
        if let Some(held) = held.filter(|held| held.grant.kind != trusted.grant.kind) {
            return Err(StoreError::GrantKind {
                code: held.grant.key.code(),
                held: held.grant.kind.into(),
            });
        }
        let issued_at = trusted.grant.issued_at;
        // Only the same grant again is a replay: another one issued in the same
        // millisecond is a change this Machine never applied.
        let replays = held.is_some_and(|held| held.grant == trusted.grant);
        let edits = match (held, prior) {
            (_, None | Some(Prior::Edit { .. })) if replays => return Ok(false),
            (None, None) => false,
            (Some(_), None) => return Err(StoreError::GrantHeld(trusted.grant.key.code())),
            (Some(held), Some(Prior::Edit { issued_at: read }))
                if held.grant.issued_at == read && read < issued_at =>
            {
                true
            }
            (_, Some(Prior::Edit { .. })) => {
                return Err(StoreError::GrantMoved(trusted.grant.key.code()));
            }
            (
                _,
                Some(Prior::Replace {
                    key,
                    issued_at: read,
                }),
            ) => {
                let source = grant_of(key).filter(|source| {
                    key != trusted.grant.key
                        && source.grant.kind == trusted.grant.kind
                        && source.grant.issued_at == read
                });
                if source.is_none() {
                    return Err(StoreError::GrantMoved(key.code()));
                }
                if held.is_some() {
                    return Ok(false);
                }
                false
            }
        };
        self.commit_if_changed(|record| {
            match edits {
                true => record
                    .grants
                    .iter_mut()
                    .filter(|held| held.grant.key == trusted.grant.key)
                    .for_each(|held| *held = trusted.clone()),
                false => record.grants.push(trusted),
            }
            Ok(true)
        })
    }

    /// Remember `removed` as Members this Machine no longer admits, and stop trusting them,
    /// in one write. A Member's WireGuard key is filled in from its trusted entry when the
    /// caller knows only its ID. Nothing is ever taken back out.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub(crate) fn remove_members(&mut self, removed: &[RemovedMember]) -> Result<(), StoreError> {
        self.commit_if_changed(|record| {
            for member in removed {
                let public_key = member.public_key.or_else(|| {
                    record
                        .trusted_machines
                        .iter()
                        .find(|trusted| trusted.id == member.id)
                        .map(|trusted| trusted.public_key)
                });
                let member = RemovedMember {
                    id: member.id,
                    public_key,
                };
                if !record.removed_members.contains(&member) {
                    record.removed_members.push(member);
                }
                record
                    .trusted_machines
                    .retain(|trusted| trusted.id != member.id);
            }
            Ok(())
        })
    }

    /// Replace the push target in one write. An unchanged target skips the write.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub fn set_push_target(&mut self, url: Option<Url>) -> Result<(), StoreError> {
        self.commit_if_changed(|record| {
            record.push_target = url;
            Ok(())
        })
    }

    /// Pin approved members so removing the Device that signed them keeps them peered.
    ///
    /// # Errors
    /// Returns a storage error if the updated record cannot be saved atomically.
    pub(crate) fn pin_machines(&mut self, machines: &[Machine]) -> Result<(), StoreError> {
        self.commit_if_changed(|record| {
            let pinned: Vec<_> = machines
                .iter()
                .filter(|machine| record.accepts_machine(machine) && !record.pins_machine(machine))
                .map(Into::into)
                .collect();
            record.trusted_machines.extend(pinned);
            Ok(())
        })
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
        let held_cluster = if self.record.filters_peers {
            None
        } else {
            Some(SealingKey::load(&self.data_dir)?.map(|key| key.public_key()))
        };
        self.commit_if_changed(|record| {
            if let Some(cluster) = held_cluster {
                record.trusted_machines.clear();
                record.membership_cluster = cluster;
            }
            record.filters_peers = true;
            for member in starting {
                if !record.trusted_machines.contains(&member) {
                    record.trusted_machines.push(member);
                }
            }
            Ok(())
        })
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
/// Cloud could write a `device-<code>` slot that [`LocalMachineRecord::management_scope`]
/// now reads as Full. The first open by this daemon tombstones every live `device-` slot and marks
/// the record vetted in the same write, so Devices approved afterwards stay approved.
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

fn removed_key_text(key: &KeyRef) -> String {
    match key {
        KeyRef::Signing { key } => format!(
            "Device {} was removed from this Server for good. To trust that computer again, delete its device.key and run: ployz device approve --this-device --connect ssh://root@HOST",
            key.code()
        ),
        KeyRef::Passkey { key } => format!(
            "Passkey {} was removed from this Server for good. Add a new passkey instead.",
            key.code()
        ),
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("join approval refused: {0}")]
    JoinRefused(ployz_core::authority::AuthorityRefusal),
    #[error("{}", removed_key_text(.0))]
    KeyRemoved(KeyRef),
    #[error(
        "Key {0} is the last person's or full CLI key this Server trusts. Approve another first."
    )]
    LastKey(DeviceCode),
    #[error("Key {code} is already a {held} key on this Server. Remove it first.")]
    GrantKind {
        code: DeviceCode,
        held: ployz_core::authority::KeyKind,
    },
    #[error("Code {0} names more than one key on this Server. Remove it by its full key.")]
    AmbiguousCode(DeviceCode),
    #[error("Key {0} is already on this Server. Change it with ployz key scope.")]
    GrantHeld(DeviceCode),
    #[error(
        "Key {0} changed on this Server while this ran. Nothing changed here. Run the command again."
    )]
    GrantMoved(DeviceCode),
    #[error("Key {0} is a device on this Server, not a CI or agent key. Nothing changed here.")]
    NotGrant(DeviceCode),
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
        store.approve_device(key()).unwrap();
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

    /// A Device slot an older daemon approved, after a downgrade, for a key this record
    /// also holds as a CI grant.
    #[test]
    fn a_device_slot_whose_key_a_ci_grant_holds_is_not_full() {
        let (dir, store) = founded();
        drop(store);
        let path = dir.path().join("data").join(STATE_FILE_NAME);
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        json.as_object_mut().unwrap().insert(
            "grants".to_owned(),
            serde_json::json!([{
                "grant": {
                    "key": ployz_core::authority::SigningPublicKey::from(key()),
                    "kind": "ci",
                    "scope": {"caps": ["spec"], "places": "cluster"},
                    "issued_at": 1,
                },
                "approved_by": {"by": "server"},
            }]),
        );
        std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();

        let store = LocalMachineStore::open(dir.path().join("data")).unwrap();

        let record = store.record();
        assert_eq!(record.grants().len(), 1);
        assert_eq!(
            record.management_scope(&key().into()),
            Some(ManagementScope::Operator)
        );
        assert_eq!(record.devices().count(), 0);
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
        assert!(store.approve_device(key()).unwrap());

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

#[cfg(test)]
mod commit_tests {
    use std::fs;

    use super::{LocalMachineRecord, LocalMachineStore, StoreError, TEMPORARY_FILE_NAME, path};

    fn opened() -> (tempfile::TempDir, LocalMachineStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalMachineStore::open(dir.path().join("data")).unwrap();
        (dir, store)
    }

    fn url(text: &str) -> Option<reqwest::Url> {
        Some(reqwest::Url::parse(text).unwrap())
    }

    fn stored(store: &LocalMachineStore) -> LocalMachineRecord {
        serde_json::from_slice(&fs::read(path(store.data_dir())).unwrap()).unwrap()
    }

    #[test]
    fn a_change_is_written_and_then_held() {
        let (_dir, mut store) = opened();

        store
            .set_push_target(url("https://cloud.example/push"))
            .unwrap();

        assert_eq!(
            store.record().push_target(),
            url("https://cloud.example/push").as_ref()
        );
        assert_eq!(stored(&store), *store.record());
    }

    #[test]
    fn an_edit_that_changes_nothing_writes_nothing() {
        let (_dir, mut store) = opened();
        store
            .set_push_target(url("https://cloud.example/push"))
            .unwrap();
        let file = path(store.data_dir());
        fs::remove_file(&file).unwrap();

        store
            .set_push_target(url("https://cloud.example/push"))
            .unwrap();

        assert!(!file.exists());
        store.set_push_target(None).unwrap();
        assert!(file.exists());
    }

    #[test]
    fn an_edit_that_refuses_leaves_the_record_and_the_file() {
        let (_dir, mut store) = opened();
        let before = store.record().clone();
        let file = fs::read(path(store.data_dir())).unwrap();

        let refused = store.complete_catch_up();

        assert!(matches!(refused, Err(StoreError::NotJoining)));
        assert_eq!(*store.record(), before);
        assert_eq!(fs::read(path(store.data_dir())).unwrap(), file);
    }

    #[test]
    fn a_failed_write_leaves_the_record_as_it_was() {
        let (_dir, mut store) = opened();
        let before = store.record().clone();
        let file = fs::read(path(store.data_dir())).unwrap();
        fs::create_dir(store.data_dir().join(TEMPORARY_FILE_NAME)).unwrap();

        let failed = store.set_push_target(url("https://cloud.example/push"));

        assert!(matches!(failed, Err(StoreError::Io(_))));
        assert_eq!(*store.record(), before);
        assert_eq!(fs::read(path(store.data_dir())).unwrap(), file);
    }
}
