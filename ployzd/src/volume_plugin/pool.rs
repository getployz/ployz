//! Root-backed Machine Pool creation and cleanup.

use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};

#[cfg(test)]
use ployzd::machine::DEFAULT_DATA_DIR;
use ployzd::machine_pool::{self, MachinePool};

use super::{
    DockerVolumeName, PoolOrigin, Result, VolumeError, VolumeStorage, checked_command, parse_size,
};

const GIBIBYTE: u64 = 1024_u64.pow(3);
const POOL_BACKING_PATH: &str = "/var/lib/ployz-machine-pool";
/// Filename of the root-backed Machine Pool vdev.
#[cfg(test)]
pub(super) const POOL_BACKING_FILE: &str = "machine-pool";
const POOL_NAME: &str = "ployz";

/// Observes and creates the host's single Machine Pool.
#[derive(Clone)]
pub(super) struct PoolStorage {
    zpool: PathBuf,
    fallocate: PathBuf,
    stat: PathBuf,
    df: PathBuf,
    backing: PathBuf,
    host_root: PathBuf,
    sys_dev_block: PathBuf,
}

/// A Pool created by the current request and therefore safe to clean up.
pub(super) struct CreatedPool<'storage> {
    pool: MachinePool,
    storage: &'storage PoolStorage,
}

impl CreatedPool<'_> {
    /// Returns the newly created Pool's usable identity.
    pub(super) fn machine_pool(&self) -> &MachinePool {
        &self.pool
    }

    /// Destroys this newly created Pool and returns the original failure, with cleanup evidence.
    pub(super) async fn cleanup(self, failure: VolumeError) -> VolumeError {
        self.storage.cleanup(failure).await
    }
}

impl VolumeStorage {
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
        let _guard = self.mutation.lock().await;
        let _pool_guard = self.pool.lock_mutation().await?;
        let existing = match self.pool.one_usable().await? {
            Some(pool) => Some(pool),
            None => self.pool.recover().await?,
        };
        if let Some(pool) = existing {
            return self
                .create_volume(&pool, name, requested, PoolOrigin::Existing)
                .await;
        }

        let pool = self.pool.create(requested).await?;
        match self
            .create_volume(
                pool.machine_pool(),
                name,
                requested,
                PoolOrigin::CreatedForRequest,
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
            backing: POOL_BACKING_PATH.into(),
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
            backing,
            host_root,
            sys_dev_block,
        }
    }

    #[cfg(test)]
    /// Replaces the production backing path for adapters at the test seam.
    pub(super) fn with_backing(mut self, backing: PathBuf) -> Self {
        self.backing = backing;
        self
    }

    async fn lock_mutation(&self) -> Result<fs::File> {
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
                    lock_path.display()
                )
            })?;
        tokio::task::spawn_blocking(move || fs2::FileExt::lock_exclusive(&lock).map(|()| lock))
            .await
            .map_err(|error| format!("could not wait for Machine Pool mutation lock: {error}"))?
            .map_err(|error| {
                format!(
                    "could not lock Machine Pool mutations through {}: {error}",
                    lock_path.display()
                )
                .into()
            })
    }

    /// Selects the sole usable imported Machine Pool, or `None` when none is imported.
    ///
    /// # Errors
    ///
    /// Returns an error when Pool inspection fails or imported Pool evidence is unusable.
    pub(super) async fn one_usable(&self) -> Result<Option<MachinePool>> {
        let output = checked_command(
            &self.zpool,
            &[
                "list",
                "-Hp",
                "-o",
                "name,size,allocated,free,health,readonly",
            ],
        )
        .await?;
        Ok(machine_pool::one_usable(&output).map_err(|error| error.to_string())?)
    }

    /// Imports this host's valid unimported backing Pool, or removes an unlabeled stale file.
    async fn recover(&self) -> Result<Option<MachinePool>> {
        match self.backing.try_exists() {
            Ok(false) => return Ok(None),
            Ok(true) => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect Machine Pool backing file {}: {error}",
                    self.backing.display()
                )
                .into());
            }
        }
        let backing = self.backing_text()?;
        let output = checked_command(&self.zpool, &["import", "-d", backing]).await?;
        let pools = machine_pool::importable_names(&output).map_err(|error| error.to_string())?;
        if pools.is_empty() {
            let output = checked_command(&self.zpool, &["import", "-D", "-d", backing]).await?;
            if !machine_pool::importable_names(&output)
                .map_err(|error| error.to_string())?
                .is_empty()
            {
                return Err(format!(
                    "Machine Pool backing file {} contains a destroyed Pool label; refusing to replace it",
                    self.backing.display()
                )
                .into());
            }
            self.remove_backing().map_err(|error| {
                format!(
                    "could not remove unlabeled Machine Pool backing file {}: {error}",
                    self.backing.display()
                )
            })?;
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
        let capacity = capacity_with_headroom(requested)?;
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
                    backing
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
                "-O",
                "canmount=off",
                POOL_NAME,
                backing,
            ],
        )
        .await
        {
            return Err(self.cleanup(error).await);
        }
        match self.one_usable().await {
            Ok(Some(pool)) if pool.name() == POOL_NAME => Ok(CreatedPool {
                pool,
                storage: self,
            }),
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
    pub(super) async fn ensure_capacity(&self, pool: &MachinePool, commitment: u64) -> Result<()> {
        let minimum = capacity_with_headroom(commitment)?;
        if pool.size_bytes().get() >= minimum {
            return Ok(());
        }
        if pool.name() != POOL_NAME {
            return Err(format!(
                "Machine Pool {} is not the managed root-backed Pool {POOL_NAME}; automatic growth cannot use {}",
                pool.name(),
                self.backing.display()
            )
            .into());
        }

        let backing = self.backing_text()?;
        let (length, allocated) = self.backing_allocation(backing).await?;
        let needs_allocation = length < minimum || allocated < length;
        let target = if needs_allocation {
            let extension = minimum
                .saturating_sub(length)
                .checked_next_multiple_of(GIBIBYTE)
                .ok_or_else(|| {
                    VolumeError::from("Machine Pool capacity rounding overflowed u64")
                })?;
            length
                .checked_add(extension)
                .ok_or_else(|| VolumeError::from("Machine Pool backing capacity overflowed u64"))?
        } else {
            length
        };
        if needs_allocation {
            self.check_host_root(target.saturating_sub(allocated))
                .await?;
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
        if expanded.size_bytes().get() < minimum {
            return Err(format!(
                "Machine Pool {} has {} bytes after growth, below the required {minimum} bytes including headroom",
                pool.name(),
                expanded.size_bytes()
            )
            .into());
        }
        Ok(())
    }

    async fn cleanup(&self, failure: VolumeError) -> VolumeError {
        let pools = match checked_command(&self.zpool, &["list", "-H", "-o", "name"]).await {
            Ok(pools) => pools,
            Err(error) => {
                return format!(
                    "{failure}; cleanup could not inspect Machine Pools: {error}; backing file retained at {}",
                    self.backing.display()
                )
                .into();
            }
        };
        if pools.lines().any(|pool| pool == POOL_NAME)
            && let Err(error) = checked_command(&self.zpool, &["destroy", "-f", POOL_NAME]).await
        {
            return format!(
                "{failure}; cleanup could not destroy Machine Pool {POOL_NAME}: {error}; backing file retained at {}",
                self.backing.display()
            )
            .into();
        }
        self.cleanup_backing(failure)
    }

    async fn check_host_root(&self, allocation: u64) -> Result<fs::Metadata> {
        let host = fs::metadata(&self.host_root).map_err(|error| {
            format!(
                "could not inspect host root {}: {error}",
                self.host_root.display()
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
                backing_directory.display()
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
        let reserve = (capacity / 4).max(10 * GIBIBYTE);
        let required = reserve
            .checked_add(allocation)
            .ok_or_else(|| VolumeError::from("host-root reserve calculation overflowed u64"))?;
        if available < required {
            let shortfall = required - available;
            return Err(format!(
                "Host root is {shortfall} bytes short: {available} bytes are available, Machine Pool growth needs {allocation} bytes, and the host-root reserve is {reserve} bytes"
            )
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
                device.display()
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
                        physical_block_size.display()
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
                self.backing.display()
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

fn capacity_with_headroom(commitment: u64) -> Result<u64> {
    commitment
        .checked_add((commitment / 10).max(GIBIBYTE))
        .ok_or_else(|| "Machine Pool capacity calculation overflowed u64".into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_backing_file_survives_machine_state_reset() {
        let storage = PoolStorage::new("zpool");

        assert!(!storage.backing.starts_with(DEFAULT_DATA_DIR));
    }
}
