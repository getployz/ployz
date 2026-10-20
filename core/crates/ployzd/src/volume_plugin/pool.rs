//! Machine Pool recovery and root-backed creation and cleanup.

use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[cfg(test)]
use ployzd::machine::DEFAULT_DATA_DIR;
use ployzd::machine_pool::{self, MachinePool};

use super::{
    CapacityAdmission, DockerVolumeName, Result, VolumeError, VolumeStorage, checked_command,
    checked_output, parse_size, run_command,
};

/// Filename of the root-backed Machine Pool vdev.
#[cfg(test)]
pub(super) const POOL_BACKING_FILE: &str = "machine-pool";
const POOL_NAME: &str = machine_pool::ROOT_BACKED_POOL_NAME;
/// Debian's monthly zfsutils cron trims Pools it misreads as NVMe-only, which
/// punches holes in a file vdev. A trimmed backing file can no longer hold the
/// Pool's size when the host root fills up.
const PERIODIC_TRIM: &str = "org.debian:periodic-trim";
const PERIODIC_TRIM_DISABLED: &str = "org.debian:periodic-trim=disable";

/// Whether recovery may reclaim a verified unlabeled backing file.
pub(super) enum UnlabeledBacking {
    Preserve,
    Remove,
}

/// Observes and creates the host's single Machine Pool.
#[derive(Clone)]
pub(super) struct PoolStorage {
    zpool: PathBuf,
    fallocate: PathBuf,
    stat: PathBuf,
    df: PathBuf,
    backing: PathBuf,
    volume_root: PathBuf,
    host_root: PathBuf,
    sys_dev_block: PathBuf,
}

/// A Pool created by the current request and therefore safe to clean up.
pub(super) struct CreatedPool<'storage> {
    pool: MachinePool,
    storage: &'storage PoolStorage,
}

impl CreatedPool<'_> {
    pub(super) fn machine_pool(&self) -> &MachinePool {
        &self.pool
    }

    /// Destroys this newly created Pool and returns the original failure, with cleanup evidence.
    pub(super) async fn cleanup(self, failure: VolumeError) -> VolumeError {
        self.storage.cleanup(failure).await
    }
}

impl VolumeStorage {
    /// Grows the Pool for `commitment` after opting a managed Pool out of periodic trim.
    ///
    /// # Errors
    ///
    /// Returns an error when the trim opt-out or growth fails.
    pub(super) async fn ensure_pool_capacity(
        &self,
        pool: &MachinePool,
        commitment: u64,
        unmanaged_used_bytes: u64,
    ) -> Result<()> {
        if pool.name() == POOL_NAME
            && self
                .zfs(&["get", "-H", "-o", "value", PERIODIC_TRIM, POOL_NAME])
                .await?
                .trim()
                != "disable"
        {
            self.zfs(&["set", PERIODIC_TRIM_DISABLED, POOL_NAME])
                .await?;
        }
        self.pool
            .ensure_capacity(pool, commitment, unmanaged_used_bytes)
            .await
    }

    /// Creates a bounded Volume, creating its Machine Pool first when absent.
    ///
    /// # Errors
    ///
    /// Returns an error when request validation, Pool creation, or dataset creation fails.
    pub(super) async fn create(
        &self,
        name: &DockerVolumeName,
        options: &BTreeMap<String, String>,
    ) -> Result<()> {
        let requested = parse_size(options)?;
        let _guard = self.admit_mutation().await?;
        let _pool_guard = self.pool.lock_mutation().await?;
        let existing = self.pool.admit(UnlabeledBacking::Remove).await?;
        if let Some(pool) = existing {
            return self
                .create_volume(&pool, name, requested, CapacityAdmission::Required)
                .await;
        }

        let pool = self.pool.create(requested).await?;
        match self
            .create_volume(
                pool.machine_pool(),
                name,
                requested,
                CapacityAdmission::Ensured,
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => Err(pool.cleanup(error).await),
        }
    }
}

impl PoolStorage {
    /// Uses production storage programs and host paths.
    pub(super) fn new(zpool: impl Into<PathBuf>) -> Self {
        Self {
            zpool: zpool.into(),
            fallocate: "fallocate".into(),
            stat: "stat".into(),
            df: "df".into(),
            backing: machine_pool::BACKING_FILE.into(),
            volume_root: super::MOUNT_ROOT.into(),
            host_root: "/".into(),
            sys_dev_block: "/sys/dev/block".into(),
        }
    }

    #[cfg(test)]
    /// Replaces external programs and host paths for seam tests.
    pub(super) fn with_environment(
        zpool: PathBuf,
        fallocate: PathBuf,
        stat: PathBuf,
        df: PathBuf,
        backing: PathBuf,
        host_root: PathBuf,
        sys_dev_block: PathBuf,
    ) -> Self {
        Self {
            zpool,
            fallocate,
            stat,
            df,
            volume_root: backing.with_file_name("ployz-volumes"),
            backing,
            host_root,
            sys_dev_block,
        }
    }

    #[cfg(test)]
    /// Replaces the production backing path for adapters at the test seam.
    pub(super) fn with_backing(mut self, backing: PathBuf) -> Self {
        self.volume_root = backing.with_file_name("ployz-volumes");
        self.backing = backing;
        self
    }

    pub(super) async fn lock_mutation(&self) -> Result<fs::File> {
        let mut lock_path = self.backing.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .map_err(|error| {
                format!(
                    "could not open Machine Pool mutation lock {}: {error}",
                    lock_path.display(),
                    error = ployz_core::error_chain::inline(&error),
                )
            })?;
        tokio::task::spawn_blocking(move || fs2::FileExt::lock_exclusive(&lock).map(|()| lock))
            .await
            .map_err(|error| {
                format!(
                    "could not wait for Machine Pool mutation lock: {error}",
                    error = ployz_core::error_chain::inline(&error),
                )
            })?
            .map_err(|error| {
                format!(
                    "could not lock Machine Pool mutations through {}: {error}",
                    lock_path.display(),
                    error = ployz_core::error_chain::inline(&error),
                )
                .into()
            })
    }

    /// Selects the sole usable imported Machine Pool and records verified device ownership.
    ///
    /// # Errors
    ///
    /// Returns an error for failed inspection, conflicting backing, or failed ownership recording.
    pub(super) async fn one_usable(&self) -> Result<Option<MachinePool>> {
        let args = [
            "list",
            "-Hp",
            "-o",
            "name,size,allocated,free,health,readonly",
        ];
        let output = match run_command(&self.zpool, &args).await {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            output => checked_output(&self.zpool, &args, output)?,
        };
        let pool = machine_pool::one_usable(&output)
            .map_err(|error| ployz_core::error_chain::inline(&error))?;
        if let Some(pool) = &pool {
            let device = pool.name() == machine_pool::DEVICE_BACKED_POOL_NAME;
            if path_exists(&self.backing, "Machine Pool backing file")?
                && (device || self.has_device_pool()?)
            {
                return Err(
                    "conflicting device and file-backed Machine Pool ownership; refusing to choose one"
                        .into(),
                );
            }
            if device {
                let marker = self.backing.with_extension("device");
                if !matches!(
                    fs::read_to_string(&marker).as_deref(),
                    Ok(machine_pool::CONFIRMED_DEVICE_POOL_MARKER)
                ) {
                    fs::write(marker, machine_pool::CONFIRMED_DEVICE_POOL_MARKER).map_err(
                        |error| format!("could not remember device Machine Pool: {error}"),
                    )?;
                }
            }
        }
        Ok(pool)
    }

    /// Selects or recovers a Machine Pool while the caller holds the Pool mutation lock.
    ///
    /// # Errors
    /// Returns an error when selection, ownership recording, or recovery fails.
    pub(super) async fn admit(&self, unlabeled: UnlabeledBacking) -> Result<Option<MachinePool>> {
        match self.one_usable().await? {
            Some(pool) => Ok(Some(pool)),
            None => self.recover(unlabeled).await,
        }
    }

    /// Whether a device Pool marker, backing file, or retained Volume mount root exists.
    ///
    /// # Errors
    /// Returns an error when any path cannot be inspected.
    pub(super) fn has_held_a_pool(&self) -> Result<bool> {
        Ok(self.has_device_pool()?
            || path_exists(&self.backing, "Machine Pool backing file")?
            || path_exists(&self.volume_root, "Volume mount root")?)
    }

    /// Imports the device Pool or valid backing Pool, preserving device ownership when absent.
    ///
    /// # Errors
    /// Returns an error for ambiguous or destroyed labels, failed import, or failed cleanup.
    pub(super) async fn recover(&self, unlabeled: UnlabeledBacking) -> Result<Option<MachinePool>> {
        let has_backing = path_exists(&self.backing, "Machine Pool backing file")?;
        if self.has_device_pool()? && has_backing {
            return Err(
                "conflicting device and file-backed Machine Pool ownership; refusing to choose one"
                    .into(),
            );
        }
        if !has_backing {
            let device_pool = machine_pool::DEVICE_BACKED_POOL_NAME;
            let output = match run_command(&self.zpool, &["import"]).await {
                Err(error)
                    if error.kind() == io::ErrorKind::NotFound && !self.has_device_pool()? =>
                {
                    return Ok(None);
                }
                output => checked_output(&self.zpool, &["import"], output)?,
            };
            let pools = machine_pool::importable_names(&output)
                .map_err(|error| ployz_core::error_chain::inline(&error))?;
            let devices = pools.iter().filter(|name| **name == device_pool).count();
            if devices > 1 {
                return Err("ambiguous device Machine Pool labels; refusing to choose one".into());
            }
            if devices == 1 {
                checked_command(&self.zpool, &["import", "-N", device_pool]).await?;
                return match self.one_usable().await? {
                    Some(pool) if pool.name() == device_pool => Ok(Some(pool)),
                    _ => Err("imported device Machine Pool, but ZFS did not report it as the sole usable Pool".into()),
                };
            }
            if self.has_device_pool()? {
                return Err(
                    "device Machine Pool is unavailable; refusing to create a root-backed Pool"
                        .into(),
                );
            }
            return Ok(None);
        }
        let backing = self.backing_text()?;
        let output = checked_command(&self.zpool, &["import", "-d", backing]).await?;
        let pools = machine_pool::importable_names(&output)
            .map_err(|error| ployz_core::error_chain::inline(&error))?;
        if pools.is_empty() {
            let output = checked_command(&self.zpool, &["import", "-D", "-d", backing]).await?;
            if !machine_pool::importable_names(&output)
                .map_err(|error| ployz_core::error_chain::inline(&error))?
                .is_empty()
            {
                return Err(format!(
                    "Machine Pool backing file {} contains a destroyed Pool label; refusing to replace it",
                    self.backing.display()
                )
                .into());
            }
            if matches!(unlabeled, UnlabeledBacking::Remove) {
                self.remove_backing().map_err(|error| {
                    format!(
                        "could not remove unlabeled Machine Pool backing file {}: {error}",
                        self.backing.display(),
                        error = ployz_core::error_chain::inline(&error),
                    )
                })?;
            }
            return Ok(None);
        }
        if pools.as_slice() != [POOL_NAME] {
            return Err(format!(
                "Machine Pool backing file {} has ambiguous Pool labels ({}); refusing to replace it",
                self.backing.display(),
                pools.join(", ")
            )
            .into());
        }
        checked_command(
            &self.zpool,
            &["import", "-d", backing, "-f", "-N", POOL_NAME],
        )
        .await?;
        match self.one_usable().await? {
            Some(pool) if pool.name() == POOL_NAME => Ok(Some(pool)),
            Some(pool) => Err(format!(
                "imported Machine Pool {POOL_NAME}, but ZFS selected {}",
                pool.name()
            )
            .into()),
            None => {
                Err(format!("imported Machine Pool {POOL_NAME}, but ZFS did not report it").into())
            }
        }
    }

    fn has_device_pool(&self) -> Result<bool> {
        path_exists(
            &self.backing.with_extension("device"),
            "device Machine Pool marker",
        )
    }

    fn backing_text(&self) -> Result<&str> {
        self.backing.to_str().ok_or_else(|| {
            format!(
                "Machine Pool backing path is not valid UTF-8: {}",
                self.backing.display()
            )
            .into()
        })
    }

    /// Creates one root-backed Machine Pool sized for the requested Volume.
    ///
    /// # Errors
    ///
    /// Returns an error when reserve, allocation, Pool creation, or verification fails.
    pub(super) async fn create(&self, requested: u64) -> Result<CreatedPool<'_>> {
        if self.has_device_pool()? {
            return Err(
                "device Machine Pool is unavailable; refusing to create a root-backed Pool".into(),
            );
        }
        let capacity = ployz_core::storage_with_headroom(requested)?;
        let host = self.check_host_root(capacity).await?;
        let ashift = self.host_root_ashift(&host)?;
        let backing = self.backing_text()?;
        fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(backing)
            .map_err(|error| {
                format!(
                    "could not create Machine Pool backing file {}: {error}",
                    backing,
                    error = ployz_core::error_chain::inline(&error),
                )
            })?;
        let capacity_text = capacity.to_string();
        if let Err(error) = checked_command(&self.fallocate, &["-l", &capacity_text, backing]).await
        {
            return Err(self.cleanup_backing(error));
        }
        if let Err(error) = self.verify_preallocation(backing, capacity).await {
            return Err(self.cleanup_backing(error));
        }
        let ashift = format!("ashift={ashift}");
        if let Err(error) = checked_command(
            &self.zpool,
            &[
                "create",
                "-f",
                "-m",
                "none",
                "-o",
                &ashift,
                "-o",
                "autotrim=off",
                "-O",
                "canmount=off",
                "-O",
                PERIODIC_TRIM_DISABLED,
                POOL_NAME,
                backing,
            ],
        )
        .await
        {
            return Err(self.cleanup(error).await);
        }
        match self.one_usable().await {
            Ok(Some(pool)) if pool.name() == POOL_NAME => {
                if let Err(error) = self.ensure_capacity(&pool, requested, 0).await {
                    return Err(self.cleanup(error).await);
                }
                Ok(CreatedPool {
                    pool,
                    storage: self,
                })
            }
            Ok(Some(pool)) => Err(self
                .cleanup(
                    format!(
                        "created Machine Pool {POOL_NAME}, but ZFS selected {}",
                        pool.name()
                    )
                    .into(),
                )
                .await),
            Ok(None) => Err(self
                .cleanup(
                    format!("created Machine Pool {POOL_NAME}, but ZFS did not report it").into(),
                )
                .await),
            Err(error) => Err(self.cleanup(error).await),
        }
    }

    /// Makes the managed backing Pool large enough for all requested bounds.
    ///
    /// # Errors
    ///
    /// Returns an error when reserve, backing allocation, Pool growth, or verification fails.
    pub(super) async fn ensure_capacity(
        &self,
        pool: &MachinePool,
        commitment: u64,
        unmanaged_used_bytes: u64,
    ) -> Result<()> {
        let minimum = ployz_core::storage_with_headroom(commitment)?
            .checked_add(unmanaged_used_bytes)
            .ok_or("Pool occupancy overflows u64")?;
        if pool.name() == POOL_NAME {
            let backing = self.backing_text()?;
            let (length, allocated) = self.backing_allocation(backing).await?;
            if allocated < length {
                self.check_host_root(length - allocated).await?;
                checked_command(&self.fallocate, &["-l", &length.to_string(), backing]).await?;
                self.verify_preallocation(backing, length).await?;
            }
        }
        if pool.size_bytes().get() >= minimum {
            return Ok(());
        }
        if pool.name() != POOL_NAME {
            return Err(ployz_core::StorageCapacityError::PoolCannotGrow {
                required_bytes: minimum,
                capacity_bytes: pool.size_bytes().get(),
            }
            .into());
        }

        let backing = self.backing_text()?;
        let mut observed = pool.size_bytes().get();
        let (length, _) = self.backing_allocation(backing).await?;
        if length < minimum {
            ployz_core::storage_growth_target(length, observed, minimum)?;
        }
        let mut retry_backing = length > observed;
        loop {
            let (length, allocated) = self.backing_allocation(backing).await?;
            let newly_allocated = !retry_backing;
            let target = if retry_backing {
                length
            } else {
                ployz_core::storage_growth_target(length, observed, minimum)?
            };
            if allocated < target {
                self.check_host_root(target - allocated).await?;
                let target_text = target.to_string();
                checked_command(&self.fallocate, &["-l", &target_text, backing]).await?;
                self.verify_preallocation(backing, target).await?;
            }
            checked_command(&self.zpool, &["online", "-e", pool.name(), backing]).await?;

            let expanded = self.one_usable().await?.ok_or_else(|| {
                VolumeError::from(format!(
                    "expanded Machine Pool {}, but ZFS did not report it",
                    pool.name()
                ))
            })?;
            if expanded.name() != pool.name() {
                return Err(format!(
                    "expanded Machine Pool {}, but ZFS selected {}",
                    pool.name(),
                    expanded.name()
                )
                .into());
            }
            let expanded_size = expanded.size_bytes().get();
            if expanded_size >= minimum {
                return Ok(());
            }
            if expanded_size < observed || (expanded_size == observed && newly_allocated) {
                return Err(format!(
                    "Machine Pool {} made no usable-capacity progress after growth; ZFS still reports {expanded_size} bytes, below the required {minimum} bytes including headroom",
                    pool.name()
                )
                .into());
            }
            retry_backing = expanded_size > observed;
            observed = expanded_size;
        }
    }

    async fn cleanup(&self, failure: VolumeError) -> VolumeError {
        let pools = match checked_command(&self.zpool, &["list", "-H", "-o", "name"]).await {
            Ok(pools) => pools,
            Err(error) => {
                return format!(
                    "{failure}; cleanup could not inspect Machine Pools: {error}; backing file retained at {}",
                    self.backing.display(), error = ployz_core::error_chain::inline(&error),)
                .into();
            }
        };
        if pools.lines().any(|pool| pool == POOL_NAME)
            && let Err(error) = checked_command(&self.zpool, &["destroy", "-f", POOL_NAME]).await
        {
            return format!(
                "{failure}; cleanup could not destroy Machine Pool {POOL_NAME}: {error}; backing file retained at {}",
                self.backing.display(), error = ployz_core::error_chain::inline(&error),)
            .into();
        }
        self.cleanup_backing(failure)
    }

    pub(super) async fn capacity_backing(
        &self,
        pool: Option<&MachinePool>,
    ) -> Result<ployz_core::StorageBacking> {
        use ployz_core::StorageBacking;
        if let Some(pool) = pool.filter(|pool| pool.name() != POOL_NAME) {
            return Ok(StorageBacking::Fixed {
                pool_size_bytes: pool.size_bytes().get(),
            });
        }
        let (_, host_total_bytes, host_available_bytes) = self.host_root_space().await?;
        match pool {
            Some(pool) => {
                let (backing_length_bytes, backing_allocated_bytes) =
                    self.backing_allocation(self.backing_text()?).await?;
                Ok(StorageBacking::RootBacked {
                    pool_size_bytes: pool.size_bytes().get(),
                    backing_length_bytes,
                    backing_allocated_bytes,
                    host_total_bytes,
                    host_available_bytes,
                })
            }
            None => {
                if self.backing.exists() {
                    return Err("Machine Pool backing exists without an imported Pool; recover it before planning".into());
                }
                Ok(StorageBacking::Unallocated {
                    host_total_bytes,
                    host_available_bytes,
                })
            }
        }
    }

    async fn host_root_space(&self) -> Result<(fs::Metadata, u64, u64)> {
        let host = fs::metadata(&self.host_root).map_err(|error| {
            format!(
                "could not inspect host root {}: {error}",
                self.host_root.display(),
                error = ployz_core::error_chain::inline(&error),
            )
        })?;
        let backing_directory = self.backing.parent().ok_or_else(|| {
            VolumeError::from(format!(
                "Machine Pool backing path has no parent: {}",
                self.backing.display()
            ))
        })?;
        let data = fs::metadata(backing_directory).map_err(|error| {
            format!(
                "could not inspect Machine Pool backing directory {}: {error}",
                backing_directory.display(),
                error = ployz_core::error_chain::inline(&error),
            )
        })?;
        if host.dev() != data.dev() {
            return Err(format!(
                "Machine Pool backing directory {} is not on the host root filesystem",
                backing_directory.display()
            )
            .into());
        }

        let output = checked_command(
            &self.df,
            &[
                "-B1",
                "--output=size,avail",
                self.host_root.to_str().ok_or_else(|| {
                    VolumeError::from(format!(
                        "host root path is not valid UTF-8: {}",
                        self.host_root.display()
                    ))
                })?,
            ],
        )
        .await?;
        let (capacity, available) = parse_host_root_space(&output)?;
        Ok((host, capacity, available))
    }

    async fn check_host_root(&self, allocation: u64) -> Result<fs::Metadata> {
        let (host, capacity, available) = self.host_root_space().await?;
        let reserve = ployz_core::storage_host_reserve(capacity);
        let required = reserve
            .checked_add(allocation)
            .ok_or_else(|| VolumeError::from("host-root reserve calculation overflowed u64"))?;
        if available < required {
            return Err(ployz_core::StorageCapacityError::InsufficientStorage {
                required_growth_bytes: allocation,
                available_bytes: available,
                reserve_bytes: reserve,
            }
            .into());
        }

        Ok(host)
    }

    fn host_root_ashift(&self, host: &fs::Metadata) -> Result<u32> {
        let device = format!(
            "{}:{}",
            nix::sys::stat::major(host.dev()),
            nix::sys::stat::minor(host.dev())
        );
        safe_ashift(self.physical_block_size(&device)?)
    }

    fn physical_block_size(&self, device: &str) -> Result<u64> {
        let device = self.sys_dev_block.join(device);
        let mut path = fs::canonicalize(&device).map_err(|error| {
            format!(
                "could not resolve host-root block device {}: {error}",
                device.display(),
                error = ployz_core::error_chain::inline(&error),
            )
        })?;
        loop {
            let physical_block_size = path.join("queue/physical_block_size");
            match fs::read_to_string(&physical_block_size) {
                Ok(value) => {
                    return value.trim().parse::<u64>().map_err(|_| {
                        "host-root physical block size is not a positive integer".into()
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound && path.pop() => {}
                Err(error) => {
                    return Err(format!(
                        "could not detect host-root physical block size from {}: {error}",
                        physical_block_size.display(),
                        error = ployz_core::error_chain::inline(&error),
                    )
                    .into());
                }
            }
        }
    }

    async fn backing_allocation(&self, backing: &str) -> Result<(u64, u64)> {
        let output = checked_command(&self.stat, &["-c", "%s %b %B", backing]).await?;
        let mut values = output.split_whitespace();
        let (Some(length), Some(blocks), Some(block_size), None) =
            (values.next(), values.next(), values.next(), values.next())
        else {
            return Err(format!("invalid allocation evidence for {backing}: {output}").into());
        };
        let length = length.parse::<u64>().map_err(|_| {
            VolumeError::from(format!(
                "invalid allocation evidence for {backing}: {output}"
            ))
        })?;
        let allocated = blocks
            .parse::<u64>()
            .ok()
            .and_then(|blocks| {
                block_size
                    .parse::<u64>()
                    .ok()
                    .and_then(|block_size| blocks.checked_mul(block_size))
            })
            .ok_or_else(|| {
                VolumeError::from(format!(
                    "invalid allocation evidence for {backing}: {output}"
                ))
            })?;
        Ok((length, allocated))
    }

    async fn verify_preallocation(&self, backing: &str, expected: u64) -> Result<()> {
        let (length, allocated) = self.backing_allocation(backing).await?;
        if length < expected {
            return Err(format!(
                "Machine Pool backing file {backing} is only {length} of {expected} bytes long"
            )
            .into());
        }
        if allocated < expected {
            return Err(format!(
                "Machine Pool backing file {backing} is sparse: {allocated} of {expected} bytes are allocated"
            )
            .into());
        }
        Ok(())
    }

    fn cleanup_backing(&self, failure: VolumeError) -> VolumeError {
        match self.remove_backing() {
            Ok(()) => failure,
            Err(error) => format!(
                "{failure}; cleanup could not remove Machine Pool backing file {}: {error}",
                self.backing.display(),
                error = ployz_core::error_chain::inline(&error),
            )
            .into(),
        }
    }

    fn remove_backing(&self) -> io::Result<()> {
        match fs::remove_file(&self.backing) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
}

fn parse_host_root_space(output: &str) -> Result<(u64, u64)> {
    let mut values = output.lines().last().unwrap_or_default().split_whitespace();
    let capacity = values
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| VolumeError::from(format!("invalid host-root capacity output: {output}")))?;
    let available = values
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| VolumeError::from(format!("invalid host-root capacity output: {output}")))?;
    if values.next().is_some() {
        return Err(format!("invalid host-root capacity output: {output}").into());
    }
    Ok((capacity, available))
}

fn safe_ashift(physical_block_size: u64) -> Result<u32> {
    if physical_block_size == 0 {
        return Err("host-root physical block size is zero".into());
    }
    let sector = physical_block_size
        .checked_next_power_of_two()
        .ok_or_else(|| VolumeError::from("host-root physical block size is too large"))?;
    let ashift = sector.ilog2().max(12);
    if ashift > 16 {
        return Err(format!(
            "host-root physical block size {physical_block_size} requires unsupported ashift={ashift}"
        )
        .into());
    }
    Ok(ashift)
}

fn path_exists(path: &Path, what: &str) -> Result<bool> {
    path.try_exists().map_err(|error| {
        format!(
            "could not inspect {what} {}: {error}",
            path.display(),
            error = ployz_core::error_chain::inline(&error),
        )
        .into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backing_file_survives_machine_state_reset() {
        let storage = PoolStorage::new("zpool");

        assert!(!storage.backing.starts_with(DEFAULT_DATA_DIR));
    }

    fn device_recovery_storage(root: &Path, scan: &str, import: &str) -> PoolStorage {
        use std::os::unix::fs::PermissionsExt;
        let program = root.join("zpool");
        fs::write(
            &program,
            format!(
                r#"#!/bin/sh
set -eu
case "$*" in
  import) {scan} ;;
  'import -N ployz-device') {import} ;;
  'list -Hp -o name,size,allocated,free,health,readonly')
    [ ! -e '{}/imported' ] || printf 'ployz-device\t8589934592\t0\t8589934592\tONLINE\toff\n' ;;
  *) echo unexpected command >&2; exit 2 ;;
esac
"#,
                root.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        PoolStorage::new(program).with_backing(root.join("machine-pool"))
    }

    #[tokio::test]
    async fn imported_device_admission_preserves_ownership_when_device_later_disappears() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(root.path(), ":", "exit 97");
        fs::write(root.path().join("imported"), "").unwrap();

        let pool = storage.one_usable().await.unwrap().unwrap();
        assert_eq!(pool.name(), machine_pool::DEVICE_BACKED_POOL_NAME);
        assert!(
            storage.has_device_pool().unwrap(),
            "admitted device Pool must retain ownership"
        );
        assert_eq!(
            fs::read_to_string(storage.backing.with_extension("device")).unwrap(),
            machine_pool::CONFIRMED_DEVICE_POOL_MARKER
        );

        fs::remove_file(root.path().join("imported")).unwrap();
        assert!(storage.one_usable().await.unwrap().is_none());
        let failure = storage
            .recover(UnlabeledBacking::Preserve)
            .await
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("device Machine Pool is unavailable"),
            "{failure}"
        );
        let failure = storage.create(1024).await.err().unwrap();
        assert!(
            failure
                .to_string()
                .contains("refusing to create a root-backed Pool"),
            "{failure}"
        );
        assert!(!storage.backing.exists());
    }

    #[tokio::test]
    async fn device_admission_retries_ownership_after_import_marker_write_fails() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(
            root.path(),
            "printf 'pool: ployz-device\\n'",
            &format!("touch '{}/imported'", root.path().display()),
        );
        let marker = storage.backing.with_extension("device");
        fs::create_dir(&marker).unwrap();

        let failure = storage
            .recover(UnlabeledBacking::Preserve)
            .await
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("could not remember device Machine Pool"),
            "{failure}"
        );
        assert!(root.path().join("imported").exists());

        fs::remove_dir(&marker).unwrap();
        let pool = storage.one_usable().await.unwrap().unwrap();
        assert_eq!(pool.name(), machine_pool::DEVICE_BACKED_POOL_NAME);
        assert!(
            marker.is_file(),
            "retry must finish the failed ownership write"
        );
        assert_eq!(
            fs::read_to_string(marker).unwrap(),
            machine_pool::CONFIRMED_DEVICE_POOL_MARKER
        );
        assert!(!storage.backing.exists());
    }

    #[tokio::test]
    async fn imported_device_admission_refuses_retained_file_backing() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(root.path(), "exit 97", "exit 97");
        fs::write(root.path().join("imported"), "").unwrap();
        fs::write(&storage.backing, "retained file Pool").unwrap();

        let failure = storage.one_usable().await.unwrap_err();
        assert!(failure.to_string().contains("conflicting"), "{failure}");
        assert!(!storage.has_device_pool().unwrap());
        assert_eq!(
            fs::read_to_string(&storage.backing).unwrap(),
            "retained file Pool"
        );
    }

    #[tokio::test]
    async fn recovery_imports_a_device_pool_without_a_backing_file() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(
            root.path(),
            "printf '   pool: ployz-device\\n'",
            &format!("touch '{}/imported'", root.path().display()),
        );
        let pool = storage
            .recover(UnlabeledBacking::Remove)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pool.name(), machine_pool::DEVICE_BACKED_POOL_NAME);
        assert!(storage.has_device_pool().unwrap());
        assert!(!storage.backing.exists());
    }

    #[tokio::test]
    async fn recovery_prefers_existing_file_backing_over_foreign_exported_device_pool() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let program = root.path().join("zpool");
        fs::write(
            &program,
            format!(
                r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> '{root}/commands'
case "$*" in
 import) printf 'pool: ployz-device\n' ;;
 'import -d {root}/machine-pool') printf 'pool: ployz\n' ;;
 'import -d {root}/machine-pool -f -N ployz') echo ployz > '{root}/selected' ;;
 'import -N ployz-device') echo ployz-device > '{root}/selected' ;;
 'list -Hp -o name,size,allocated,free,health,readonly')
   printf '%s\t8589934592\t0\t8589934592\tONLINE\toff\n' "$(cat '{root}/selected')" ;;
 *) exit 97 ;;
esac
"#,
                root = root.path().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let storage = PoolStorage::new(program).with_backing(root.path().join("machine-pool"));
        fs::write(&storage.backing, "original pool bytes").unwrap();
        let pool = storage
            .recover(UnlabeledBacking::Preserve)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pool.name(), POOL_NAME);
        let commands = fs::read_to_string(root.path().join("commands")).unwrap();
        assert!(
            commands.contains(&format!("import -d {}", storage.backing.display())),
            "{commands}"
        );
        assert!(!storage.has_device_pool().unwrap());
        assert_eq!(
            fs::read_to_string(&storage.backing).unwrap(),
            "original pool bytes"
        );
    }

    #[tokio::test]
    async fn recovery_refuses_conflicting_file_and_device_ownership() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(root.path(), "exit 97", "exit 97");
        fs::write(&storage.backing, "retained pool").unwrap();
        fs::write(storage.backing.with_extension("device"), "ployz-device\n").unwrap();
        let failure = storage
            .recover(UnlabeledBacking::Preserve)
            .await
            .unwrap_err();
        assert!(failure.to_string().contains("conflicting"), "{failure}");
        assert_eq!(
            fs::read_to_string(&storage.backing).unwrap(),
            "retained pool"
        );
    }

    #[tokio::test]
    async fn recovery_refuses_a_device_pool_that_cannot_import() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(
            root.path(),
            "printf '   pool: ployz-device\\n'",
            "echo device-unavailable >&2; exit 1",
        );
        let failure = storage.recover(UnlabeledBacking::Remove).await.unwrap_err();
        assert!(
            failure.to_string().contains("device-unavailable"),
            "{failure}"
        );
        assert!(!storage.has_device_pool().unwrap());
        assert!(!storage.backing.exists());
    }

    #[tokio::test]
    async fn recovery_refuses_duplicate_device_pool_names() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(
            root.path(),
            "printf '   pool: ployz-device\\n   pool: ployz-device\\n'",
            "exit 97",
        );
        let failure = storage.recover(UnlabeledBacking::Remove).await.unwrap_err();
        assert!(failure.to_string().contains("ambiguous"), "{failure}");
        assert!(!storage.backing.exists());
    }

    #[tokio::test]
    async fn a_missing_device_pool_never_becomes_a_file_backed_pool() {
        let root = tempfile::tempdir().unwrap();
        let storage = device_recovery_storage(root.path(), ":", "exit 97");
        fs::write(storage.backing.with_extension("device"), "ployz-device\n").unwrap();
        assert!(storage.has_held_a_pool().unwrap());
        assert!(storage.recover(UnlabeledBacking::Remove).await.is_err());
        let failure = storage.create(1024).await.err().unwrap();
        assert!(failure.to_string().contains("device"), "{failure}");
        assert!(!storage.backing.exists());
    }
}
