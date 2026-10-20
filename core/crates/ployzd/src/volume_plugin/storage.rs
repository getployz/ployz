//! Durable ZFS Volume storage ownership and mutation admission.

use std::{
    collections::BTreeMap,
    io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::Output,
    sync::Arc,
    time::Duration,
};

use ployzd::machine_pool::MachinePool;
use tokio::{
    process::Command,
    sync::{Mutex, OwnedMutexGuard},
};

use super::{
    DockerVolumeName, Result, VolumeError,
    pool::PoolStorage,
    transfer::{RECEIVE_STALL, STOP_DEADLINE},
};

pub(super) use ployz_core::{DATASET_ROOT, MIRROR_ROOT};
pub(super) const MOUNT_ROOT: &str = "/var/lib/ployz-volumes";
/// Set on a root renamed into place until Docker's Create registers it with its labels.
/// Get and List hide a marked root, so Docker never records it without them.
pub(super) const PROMOTE_PROPERTY: &str = "ployz:promote";
/// Set by ZFS on a dataset whose `receive -s` was interrupted; `-` otherwise.
pub(super) const RESUME_TOKEN_PROPERTY: &str = "receive_resume_token";
/// On a root: the SetVolumeCopy run of writes it belongs to. Missing reads as 0.
pub(super) const EPOCH_PROPERTY: &str = "ployz:epoch";
/// Set on a root the cut renames into its slot, so the rename carries it to the slot's
/// `fs`. It means Docker on this Machine still holds a record for the Volume, and plugin
/// Get keeps answering for it until a Writer want makes the root writable again.
pub(super) const CUT_PROPERTY: &str = "ployz:cut";
/// On a slot parent: the target of the SetVolumeCopy pull in flight, from its start until
/// its prune is done.
pub(super) const PULL_PROPERTY: &str = "ployz:pull";
/// On a slot's `fs`: the guid of the snapshot a mirror pull received, set once the
/// receive exits having made a snapshot newer than the slot held. That snapshot came from
/// a writer elsewhere, so while it is still the newest the departure may destroy the slot.
/// Read only where set locally: a value inherited from a parent proves nothing.
pub(super) const PULLED_PROPERTY: &str = "ployz:pulled";
/// On a slot parent: what this Machine holds the slot for, `mirror` once a pull starts
/// and `old_copy` once a cut or a departure parks a root in it. Unset reads as a Mirror.
pub(super) const ROLE_PROPERTY: &str = "ployz:role";

pub(super) fn slot_parent(pool: &MachinePool, name: &DockerVolumeName) -> String {
    format!("{}/{MIRROR_ROOT}/{name}", pool.name())
}

pub(super) fn slot_fs(pool: &MachinePool, name: &DockerVolumeName) -> String {
    format!("{}/fs", slot_parent(pool, name))
}

pub(super) fn root_dataset(pool: &MachinePool, name: &DockerVolumeName) -> String {
    format!("{}/{DATASET_ROOT}/{name}", pool.name())
}

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
    pub(super) mutation: MutationLock,
    pub(super) installation: ployzd::mutation::MutationGate,
    pub(super) receives: super::transfer::Receives,
    /// Where a writer Machine serves send streams; tests point it at a local server.
    pub(super) send_port: u16,
    pub(super) receive_stall: Duration,
    /// How long a departure waits for every receive to stop. Demote answers within its
    /// 120 s timeout even when ZFS hangs one of them.
    pub(super) stop_deadline: Duration,
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

/// The plugin's one mutation lock: every storage change runs under it.
#[derive(Clone, Default)]
pub(super) struct MutationLock {
    lock: Arc<Mutex<()>>,
}

pub(super) struct HeldMutation {
    _guard: OwnedMutexGuard<()>,
}

impl MutationLock {
    pub(super) async fn lock(&self) -> HeldMutation {
        HeldMutation {
            _guard: Arc::clone(&self.lock).lock_owned().await,
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
            mutation: MutationLock::default(),
            installation: ployzd::mutation::MutationGate::new(run_dir, data_dir),
            receives: super::transfer::Receives::default(),
            send_port: ployz_core::VOLUME_SEND_PORT,
            receive_stall: RECEIVE_STALL,
            stop_deadline: STOP_DEADLINE,
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
            mutation: MutationLock::default(),
            installation: ployzd::mutation::MutationGate::new(
                fixture.join("admission-run"),
                fixture.join("admission-data"),
            ),
            receives: super::transfer::Receives::default(),
            send_port: ployz_core::VOLUME_SEND_PORT,
            receive_stall: RECEIVE_STALL,
            stop_deadline: STOP_DEADLINE,
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
                self.zfs(&["inherit", PROMOTE_PROPERTY, &existing.name])
                    .await?;
                ployzd::faults::kill_inside("Create", None);
            }
            return Ok(());
        }

        let slot = slot_parent(pool, name);
        if datasets.iter().any(|dataset| dataset.name == slot) {
            return Err(VolumeError::from(format!(
                "Volume {name} is held in slot {slot}; promote or restore it before creating it again"
            )));
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
        self.ensure_pool_capacity(
            pool,
            commitment,
            pool.used_bytes().saturating_sub(managed_used),
        )
        .await?;
        Ok(())
    }

    /// Raises the bound `dataset` carries from `held` to `wanted`: the difference is
    /// committed to the Pool first, then the one property is written and read back.
    pub(super) async fn grow_bound(
        &self,
        pool: &MachinePool,
        datasets: &[Dataset],
        dataset: &str,
        held: u64,
        wanted: u64,
    ) -> Result<()> {
        self.ensure_commitment(pool, datasets, wanted - held)
            .await?;
        self.set_verified(dataset, "refquota", &wanted.to_string())
            .await
    }

    /// Lowers the bound `held` carries to `wanted`, which releases the difference from the
    /// Pool's commitment. A slot's copy comes down before its parent, so a write that
    /// fails between them leaves the committed bound at or above the copy's.
    pub(super) async fn shrink_bound(&self, held: &Held<'_>, wanted: u64) -> Result<()> {
        let wanted = wanted.to_string();
        if let Held::Slot { fs: Some(fs), .. } = held {
            self.set_verified(&fs.name, "refquota", &wanted).await?;
        }
        self.set_verified(&held.dataset().name, "refquota", &wanted)
            .await
    }

    /// Bytes the dataset holding `held`'s data references; a slot with no copy holds none.
    pub(super) async fn referenced(&self, held: &Held<'_>) -> Result<u64> {
        let dataset = match held {
            Held::Root(root) => root,
            Held::Slot { fs: Some(fs), .. } => fs,
            Held::Slot { fs: None, .. } => return Ok(0),
        };
        let value = self
            .zfs(&["get", "-Hp", "-o", "value", "referenced", &dataset.name])
            .await?;
        let value = value.trim_end_matches('\n');
        value.parse().map_err(|_| {
            format!(
                "ZFS reported referenced={value} for dataset {}",
                dataset.name
            )
            .into()
        })
    }

    /// Sets `property` on `dataset` and reads it back in bytes (`-p`): the value ZFS
    /// reports is the one that counts, whatever the write accepted.
    pub(super) async fn set_verified(
        &self,
        dataset: &str,
        property: &str,
        value: &str,
    ) -> Result<()> {
        self.zfs(&["set", &format!("{property}={value}"), dataset])
            .await?;
        let actual = self
            .zfs(&["get", "-Hp", "-o", "value", property, dataset])
            .await?;
        let actual = actual.trim_end_matches('\n');
        if actual == value {
            return Ok(());
        }
        Err(format!("{dataset} has {property}={actual} after setting it to {value}").into())
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
        let _guard = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let pool = self
            .pool
            .admit(super::pool::UnlabeledBacking::Preserve)
            .await?
            .ok_or("no usable existing Machine Pool")?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} does not exist"))?;
        dataset.require_provisioned(name)?;
        dataset.require_not_switching(name)?;
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
                "name,refquota,used,usedbydataset,referenced,mountpoint,mounted,readonly",
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

    pub(super) async fn exists(&self, dataset: &str) -> Result<bool> {
        let names = self
            .zfs(&["list", "-H", "-o", "name", "-t", "filesystem"])
            .await?;
        Ok(names.lines().any(|name| name == dataset))
    }

    pub(super) async fn zfs(&self, args: &[&str]) -> Result<String> {
        checked_command(&self.zfs, args).await
    }

    /// What this Machine holds for `name` in `datasets`, if anything, with `receiving`
    /// as sampled after that listing.
    pub(super) fn held<'datasets>(
        &self,
        datasets: &'datasets [Dataset],
        pool: &MachinePool,
        name: &DockerVolumeName,
        receiving: bool,
    ) -> Result<Option<Held<'datasets>>> {
        if let Some(root) = Self::dataset(datasets, pool, name)? {
            return Ok(Some(Held::Root(root)));
        }
        let Some(parent) = Self::slot(datasets, pool, name) else {
            return Ok(None);
        };
        Ok(Some(Held::Slot {
            parent,
            fs: Self::slot_fs(datasets, pool, name),
            receiving,
        }))
    }
}

/// The datasets this Machine holds for a Volume: its writer root, or its mirror slot's
/// parent with the copy once a receive or a cut landed one. The bound is the root's
/// `refquota` or the parent's; the copy carries the same bound without committing it.
pub(super) enum Held<'datasets> {
    Root(&'datasets Dataset),
    Slot {
        parent: &'datasets Dataset,
        fs: Option<&'datasets Dataset>,
        receiving: bool,
    },
}

impl Held<'_> {
    /// The dataset carrying the bound.
    pub(super) fn dataset(&self) -> &Dataset {
        match self {
            Self::Root(root) => root,
            Self::Slot { parent, .. } => parent,
        }
    }

    /// The Provisioned Volume bound; a `refquota` of 0 is none.
    pub(super) fn bound(&self) -> Option<NonZeroU64> {
        self.dataset().bound()
    }

    /// Whether a Volume Run has the Volume mid-switch on this Machine. A root switches
    /// while [`Dataset::switching`]. A slot switches only while a receive runs into it:
    /// its pull mark stays until a later pull settles, and its copy's cut mark until a
    /// pull or a promote clears it, so a mark alone says nothing about a step in progress.
    pub(super) fn switching(&self) -> bool {
        match self {
            Self::Root(root) => root.switching(),
            Self::Slot { receiving, .. } => *receiving,
        }
    }
}

pub(super) struct Dataset {
    pub(super) name: String,
    pub(super) refquota: u64,
    /// ZFS `referenced`: the copy's data as `refquota` bounds it, shared snapshot
    /// blocks included.
    pub(super) referenced_bytes: u64,
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
            Some(referenced_bytes),
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
            referenced_bytes: referenced_bytes.parse::<u64>().map_err(|_| invalid())?,
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

    /// Whether a Volume Run holds this root read-only, from its cut until a Writer want
    /// makes it writable again.
    pub(super) fn switching(&self) -> bool {
        self.readonly
    }

    pub(super) fn require_not_switching(&self, name: &DockerVolumeName) -> Result<()> {
        if !self.switching() {
            return Ok(());
        }
        Err(format!(
            "VolumeSwitching: Volume {name} is mid-run on this Machine (ZFS dataset {} is read-only); wait for the run to finish",
            self.name
        )
        .into())
    }

    /// The Provisioned Volume bound the dataset carries; a `refquota` of 0 is none.
    pub(super) fn bound(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.refquota)
    }

    pub(super) fn require_provisioned(&self, name: &DockerVolumeName) -> Result<()> {
        if self.bound().is_none() {
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
        if self.refquota >= requested {
            return Ok(());
        }
        Err(format!(
            "Volume {name} already has a {}-byte bound, below the {requested} bytes requested; creating a Volume never grows its bound",
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
