//! Durable ZFS Volume storage ownership and mutation admission.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::Output,
    sync::Arc,
    time::Duration,
};

use ployzd::machine_pool::MachinePool;
use tokio::{
    process::Command,
    sync::{Mutex, OwnedMutexGuard, watch},
};

use super::{DockerVolumeName, Result, VolumeError, pool::PoolStorage, transfer::RECEIVE_STALL};

pub(super) use ployz_core::{DATASET_ROOT, MIRROR_ROOT};
pub(super) const MOUNT_ROOT: &str = "/var/lib/ployz-volumes";

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Place<'name> {
    Root(&'name str),
    Slot(&'name str),
    Outside,
}

impl<'name> Place<'name> {
    pub(super) fn of(dataset: &'name str, pool: &str) -> Self {
        let Some(rest) = dataset
            .strip_prefix(pool)
            .and_then(|rest| rest.strip_prefix('/'))
        else {
            return Self::Outside;
        };
        if let Some(name) = rest.strip_prefix(&format!("{DATASET_ROOT}/")) {
            return Self::Root(name);
        }
        match rest.strip_prefix(&format!("{MIRROR_ROOT}/")) {
            Some(slot) => Self::Slot(slot.split('/').next().unwrap_or(slot)),
            None => Self::Outside,
        }
    }

    pub(super) fn commits(&self) -> bool {
        matches!(self, Self::Root(_) | Self::Slot(_))
    }
}

#[derive(Clone)]
pub(super) struct VolumeStorage {
    pub(super) pool: PoolStorage,
    pub(super) zfs: PathBuf,
    pub(super) docker: PathBuf,
    pub(super) mount_grant: Arc<std::sync::Mutex<Option<super::switch_source::MountGrant>>>,
    pub(super) steps: super::container_step::ContainerSteps,
    pub(super) mutation: MutationLock,
    pub(super) installation: ployzd::mutation::MutationGate,
    pub(super) receives: super::transfer::Receives,
    /// Where a writer Machine serves send streams; tests point it at a local server.
    pub(super) send_port: u16,
    pub(super) receive_stall: Duration,
}

/// Bytes a dataset commits the Pool to: a root's `refquota`, or a slot parent's. The
/// slot's `fs` copy carries the same bound and is not counted again.
pub(super) fn committed_bytes(dataset: &Dataset, pool: &str) -> u64 {
    match Place::of(&dataset.name, pool) {
        Place::Root(_) => dataset.refquota,
        Place::Slot(name) if dataset.name == format!("{pool}/{MIRROR_ROOT}/{name}") => {
            dataset.refquota
        }
        Place::Slot(_) | Place::Outside => 0,
    }
}

/// A lock whose waiters can refuse instead of queueing while the holder calls Docker.
#[derive(Clone)]
pub(super) struct MutationLock {
    lock: Arc<Mutex<()>>,
    docker_bound: Arc<watch::Sender<bool>>,
}

impl Default for MutationLock {
    fn default() -> Self {
        Self {
            lock: Arc::default(),
            docker_bound: Arc::new(watch::Sender::new(false)),
        }
    }
}

pub(super) struct HeldMutation {
    _guard: OwnedMutexGuard<()>,
    docker_bound: Arc<watch::Sender<bool>>,
}

impl Drop for HeldMutation {
    fn drop(&mut self) {
        self.docker_bound.send_replace(false);
    }
}

impl MutationLock {
    pub(super) async fn lock(&self) -> HeldMutation {
        self.held(Arc::clone(&self.lock).lock_owned().await)
    }

    pub(super) async fn lock_unless_docker_bound(&self) -> Option<HeldMutation> {
        if let Ok(guard) = Arc::clone(&self.lock).try_lock_owned() {
            return Some(self.held(guard));
        }
        let mut docker_bound = self.docker_bound.subscribe();
        tokio::select! {
            biased;
            guard = Arc::clone(&self.lock).lock_owned() => Some(self.held(guard)),
            _ = docker_bound.wait_for(|bound| *bound) => None,
        }
    }

    pub(super) fn raise_docker_bound(&self) {
        self.docker_bound.send_replace(true);
    }

    fn held(&self, guard: OwnedMutexGuard<()>) -> HeldMutation {
        self.docker_bound.send_replace(false);
        HeldMutation {
            _guard: guard,
            docker_bound: Arc::clone(&self.docker_bound),
        }
    }
}

pub(super) enum CapacityAdmission {
    Required,
    Ensured,
}

impl VolumeStorage {
    pub(super) fn new(data_dir: impl Into<PathBuf>, run_dir: impl Into<PathBuf>) -> Self {
        Self {
            pool: PoolStorage::new("zpool"),
            zfs: "zfs".into(),
            docker: "docker".into(),
            mount_grant: Arc::default(),
            steps: Default::default(),
            mutation: MutationLock::default(),
            installation: ployzd::mutation::MutationGate::new(run_dir, data_dir),
            receives: super::transfer::Receives::default(),
            send_port: ployz_core::VOLUME_SEND_PORT,
            receive_stall: RECEIVE_STALL,
        }
    }

    #[cfg(test)]
    pub(super) fn with_programs(zpool: impl Into<PathBuf>, zfs: impl Into<PathBuf>) -> Self {
        let zpool = zpool.into();
        let backing = zpool.with_file_name("machine-pool");
        let fixture = zpool
            .parent()
            .expect("test command has a fixture directory")
            .to_owned();
        Self {
            pool: PoolStorage::new(zpool).with_backing(backing),
            zfs: zfs.into(),
            docker: fixture.join("docker"),
            mount_grant: Arc::default(),
            steps: Default::default(),
            mutation: MutationLock::default(),
            installation: ployzd::mutation::MutationGate::new(
                fixture.join("admission-run"),
                fixture.join("admission-data"),
            ),
            receives: super::transfer::Receives::default(),
            send_port: ployz_core::VOLUME_SEND_PORT,
            receive_stall: RECEIVE_STALL,
        }
    }

    pub(super) async fn admit_mutation(
        &self,
    ) -> Result<(HeldMutation, ployzd::mutation::MutationGuard)> {
        let local = self.mutation.lock().await;
        let installation = self
            .installation
            .try_mutation()
            .map_err(|error| VolumeError::from(ployz_core::error_chain::inline(&error)))?;
        Ok((local, installation))
    }

    /// Docker holds its per-Volume lock across a plugin Mount or Remove, so those refuse
    /// rather than wait behind a holder that is calling Docker for the same Volume.
    pub(super) async fn admit_docker_request(
        &self,
        name: &DockerVolumeName,
    ) -> Result<(HeldMutation, ployzd::mutation::MutationGuard)> {
        let local = self
            .mutation
            .lock_unless_docker_bound()
            .await
            .ok_or_else(|| {
                VolumeError::from(format!(
                    "VolumeSwitching: Volume {name} has an active storage mutation"
                ))
            })?;
        let installation = self
            .installation
            .try_mutation()
            .map_err(|error| VolumeError::from(ployz_core::error_chain::inline(&error)))?;
        Ok((local, installation))
    }

    pub(super) async fn create_volume(
        &self,
        pool: &MachinePool,
        name: &DockerVolumeName,
        requested: u64,
        origin: CapacityAdmission,
    ) -> Result<()> {
        let datasets = self.datasets(pool).await?;
        let root = format!("{}/{DATASET_ROOT}", pool.name());
        let volume = format!("{root}/{name}");

        if let Some(existing) = Self::dataset(&datasets, pool, name)? {
            existing.require_requested(name, requested)?;
            if self.unregistered(&existing.name).await? {
                self.zfs(&["inherit", super::lease::PROMOTE_PROPERTY, &existing.name])
                    .await?;
                ployzd::faults::kill_inside("Create");
            }
            return Ok(());
        }

        if matches!(origin, CapacityAdmission::Required) {
            self.ensure_commitment(pool, &datasets, requested).await?;
        }

        if !datasets.iter().any(|dataset| dataset.name == root) {
            self.create_root(&root).await?;
        }
        self.zfs(&["create", "-o", &format!("refquota={requested}"), &volume])
            .await?;
        Ok(())
    }

    /// Grows the Pool, when it can, for `requested` more committed bytes.
    pub(super) async fn ensure_commitment(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        requested: u64,
    ) -> Result<()> {
        let committed = datasets
            .iter()
            .filter(|dataset| Place::of(&dataset.name, pool.name()).commits());
        let commitment = committed
            .clone()
            .map(|dataset| committed_bytes(dataset, pool.name()))
            .try_fold(requested, u64::checked_add)
            .ok_or_else(|| VolumeError::from("Provisioned Volume commitments overflowed u64"))?;
        let managed_used = committed
            .map(|dataset| dataset.active_used_bytes)
            .try_fold(0u64, u64::checked_add)
            .ok_or("Dataset occupancy overflows u64")?;
        self.pool
            .ensure_capacity(
                pool,
                commitment,
                pool.used_bytes().saturating_sub(managed_used),
            )
            .await?;
        Ok(())
    }

    pub(super) async fn create_root(&self, root: &str) -> Result<()> {
        self.zfs(&[
            "create",
            "-o",
            "canmount=off",
            "-o",
            &format!("mountpoint={MOUNT_ROOT}"),
            root,
        ])
        .await?;
        Ok(())
    }

    pub(super) async fn mountpoint(&self, name: &DockerVolumeName) -> Result<String> {
        if let Some(granted) = self.granted_mountpoint(name).await? {
            return Ok(granted);
        }
        let _guard = self.admit_docker_request(name).await?;
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} does not exist"))?;
        dataset.require_provisioned(name)?;
        dataset.require_not_switching(name)?;
        self.require_writer_idle(&pool, &datasets, dataset, name)
            .await?;
        if dataset.mounted {
            return Ok(dataset.mountpoint.clone());
        }
        self.zfs(&["mount", &dataset.name]).await?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} disappeared while mounting"))?;
        dataset.require_provisioned(name)?;
        dataset.require_not_switching(name)?;
        if !dataset.mounted {
            return Err(format!("Provisioned Volume {name} did not mount").into());
        }
        Ok(dataset.mountpoint.clone())
    }

    /// A root mounts for a Container only while no run holds it: idle marker and no
    /// open cycle in this Machine's record.
    async fn require_writer_idle(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        root: &Dataset,
        name: &DockerVolumeName,
    ) -> Result<()> {
        let writer = self.writer_marker(root).await?;
        if writer != ployz_core::WriterMarker::Idle {
            return Err(format!(
                "VolumeSwitching: Volume {name} is mid-run on this Machine (writer {writer}); wait for the run to finish"
            )
            .into());
        }
        if let Some(record) = self.lease_record(datasets, pool, name).await?
            && record.cycle == ployz_core::Cycle::Open
        {
            return Err(format!(
                "VolumeSwitching: Volume {name} is mid-run on this Machine (record {record} is open); wait for the run to finish"
            )
            .into());
        }
        Ok(())
    }

    pub(super) async fn one_pool(&self) -> Result<MachinePool> {
        self.pool
            .one_usable()
            .await?
            .ok_or_else(|| "no usable existing Machine Pool".into())
    }

    pub(super) async fn datasets(&self, pool: &MachinePool) -> Result<Vec<Dataset>> {
        let output = self
            .zfs(&[
                "list",
                "-Hp",
                "-o",
                "name,refquota,used,usedbydataset,mountpoint,mounted,readonly",
                "-r",
                pool.name(),
            ])
            .await?;
        output
            .lines()
            .map(|line| Dataset::parse(line, pool.name()))
            .collect()
    }

    pub(super) fn dataset<'datasets>(
        datasets: &'datasets [Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
    ) -> Result<Option<&'datasets Dataset>> {
        let root_name = format!("{}/{DATASET_ROOT}", pool.name());
        let root = datasets.iter().find(|dataset| dataset.name == root_name);
        if let Some(root) = root {
            root.require_mountpoint(MOUNT_ROOT)?;
            root.require_writable()?;
        }

        let requested = format!("{}/{DATASET_ROOT}/{name}", pool.name());
        let descendant_prefix = format!("{requested}/");
        if let Some(dataset) = datasets
            .iter()
            .find(|dataset| dataset.name.starts_with(&descendant_prefix))
        {
            return Err(format!(
                "ZFS dataset {} is a descendant of Provisioned Volume {name}; remove it before retrying",
                dataset.name
            )
            .into());
        }
        let dataset = datasets.iter().find(|dataset| dataset.name == requested);
        if dataset.is_some() && root.is_none() {
            return Err(format!("ZFS did not report managed root dataset {root_name}").into());
        }
        Ok(dataset)
    }

    pub(super) async fn zfs(&self, args: &[&str]) -> Result<String> {
        checked_command(&self.zfs, args).await
    }
}

pub(super) struct Dataset {
    pub(super) name: String,
    pub(super) refquota: u64,
    pub(super) used_bytes: u64,
    pub(super) active_used_bytes: u64,
    pub(super) mountpoint: String,
    pub(super) mounted: bool,
    pub(super) readonly: bool,
}

impl Dataset {
    pub(super) fn parse(line: &str, pool: &str) -> Result<Self> {
        let mut fields = line.split('\t');
        let invalid = || format!("invalid ZFS dataset output for Pool {pool}: {line}");
        let (
            Some(name),
            Some(refquota),
            Some(used_bytes),
            Some(active_used_bytes),
            Some(mountpoint),
            Some(mounted),
            Some(readonly),
            None,
        ) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        )
        else {
            return Err(invalid().into());
        };
        let used_bytes = used_bytes.parse::<u64>().map_err(|_| invalid())?;
        let active_used_bytes = active_used_bytes.parse::<u64>().map_err(|_| invalid())?;
        if active_used_bytes > used_bytes {
            return Err(invalid().into());
        }
        Ok(Self {
            name: name.to_owned(),
            refquota: parse_zfs_bytes(refquota)?,
            used_bytes,
            active_used_bytes,
            mountpoint: mountpoint.to_owned(),
            mounted: mounted == "yes",
            readonly: match readonly {
                "off" => false,
                "on" => true,
                _ => return Err(invalid().into()),
            },
        })
    }

    pub(super) fn require_mountpoint(&self, expected: &str) -> Result<()> {
        if self.mountpoint == expected {
            return Ok(());
        }
        Err(format!(
            "ZFS dataset {} has incompatible mountpoint {}; set it to {expected} before retrying",
            self.name, self.mountpoint
        )
        .into())
    }

    pub(super) fn require_writable(&self) -> Result<()> {
        if !self.readonly {
            return Ok(());
        }
        Err(format!(
            "ZFS dataset {} is read-only; make it writable before retrying",
            self.name
        )
        .into())
    }

    pub(super) fn require_not_switching(&self, name: &DockerVolumeName) -> Result<()> {
        if !self.readonly {
            return Ok(());
        }
        Err(format!(
            "VolumeSwitching: Volume {name} is mid-run on this Machine (ZFS dataset {} is read-only); wait for the run to finish",
            self.name
        )
        .into())
    }

    pub(super) fn require_provisioned(&self, name: &DockerVolumeName) -> Result<()> {
        if self.refquota == 0 {
            return Err(format!(
                "ZFS dataset {} has no Provisioned Volume bound; refusing to use it",
                self.name
            )
            .into());
        }
        self.require_mountpoint(&name.mountpoint())
    }

    pub(super) fn require_requested(&self, name: &DockerVolumeName, requested: u64) -> Result<()> {
        self.require_mountpoint(&name.mountpoint())?;
        self.require_writable()?;
        if self.refquota == requested {
            return Ok(());
        }
        Err(format!(
            "Volume {name} already has a {}-byte bound; a Volume's size is fixed once deployed, so it can't become {requested} bytes",
            self.refquota
        )
        .into())
    }
}

fn parse_zfs_bytes(value: &str) -> Result<u64> {
    match value {
        "none" | "-" => Ok(0),
        _ => value
            .parse()
            .map_err(|_| VolumeError::from(format!("invalid byte count from ZFS: {value}"))),
    }
}

pub(super) fn parse_size(options: &BTreeMap<String, String>) -> Result<u64> {
    if options.len() != 1 || !options.contains_key("size") {
        return Err("Volume option size is required and is the only supported option".into());
    }
    let value = options
        .get("size")
        .expect("the only accepted option is size");
    let (amount, suffix) = value.split_at(value.len().saturating_sub(1));
    let multiplier = match suffix {
        "b" => 1,
        "k" => 1024_u64,
        "m" => 1024_u64.pow(2),
        "g" => 1024_u64.pow(3),
        "t" => 1024_u64.pow(4),
        _ => {
            return Err(format!(
                "invalid Volume size {value:?}; use a positive integer followed by b, k, m, g, or t"
            )
            .into());
        }
    };
    let amount = amount.parse::<u64>().map_err(|_| {
        format!(
            "invalid Volume size {value:?}; use a positive integer followed by b, k, m, g, or t"
        )
    })?;
    if amount == 0 {
        return Err("Volume size must be greater than zero".into());
    }
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("Volume size {value:?} overflows bytes").into())
}

pub(super) async fn run_command(program: &PathBuf, args: &[&str]) -> io::Result<Output> {
    let mut attempt = 0;
    loop {
        match Command::new(program).args(args).output().await {
            // ETXTBSY means exec never started; only that launch error is safe to retry.
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy && attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(10 << attempt)).await;
                attempt += 1;
            }
            output => return output,
        }
    }
}

pub(super) async fn checked_command(program: &PathBuf, args: &[&str]) -> Result<String> {
    checked_output(program, args, run_command(program, args).await)
}

pub(super) fn checked_output(
    program: &Path,
    args: &[&str],
    output: io::Result<Output>,
) -> Result<String> {
    let output = output.map_err(|error| {
        format!(
            "could not run {}: {error}",
            program.display(),
            error = ployz_core::error_chain::inline(&error),
        )
    })?;
    if !output.status.success() {
        return Err(format!(
            "{} {} failed: {}",
            program.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    String::from_utf8(output.stdout)
        .map_err(|_| format!("{} returned non-UTF-8 output", program.display()).into())
}
