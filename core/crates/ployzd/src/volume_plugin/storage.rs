//! Durable ZFS Volume storage ownership and mutation admission.

use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc};

use ployzd::machine_pool::MachinePool;
use tokio::{
    process::Command,
    sync::{Mutex, OwnedMutexGuard},
};

use super::{DockerVolumeName, Result, VolumeError, pool::PoolStorage};

pub(super) const DATASET_ROOT: &str = "ployz";
pub(super) const MOUNT_ROOT: &str = "/var/lib/ployz-volumes";

#[derive(Clone)]
pub(super) struct VolumeStorage {
    pub(super) pool: PoolStorage,
    pub(super) zfs: PathBuf,
    pub(super) mutation: Arc<Mutex<()>>,
    pub(super) installation: ployzd::mutation::MutationGate,
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
            mutation: Arc::new(Mutex::new(())),
            installation: ployzd::mutation::MutationGate::new(run_dir, data_dir),
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
            mutation: Arc::new(Mutex::new(())),
            installation: ployzd::mutation::MutationGate::new(
                fixture.join("admission-run"),
                fixture.join("admission-data"),
            ),
        }
    }

    pub(super) async fn admit_mutation(
        &self,
    ) -> Result<(OwnedMutexGuard<()>, ployzd::mutation::MutationGuard)> {
        let local = Arc::clone(&self.mutation).lock_owned().await;
        let installation = self
            .installation
            .try_mutation()
            .map_err(|error| VolumeError::from(error.to_string()))?;
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
            return existing.require_requested(name, requested);
        }

        if matches!(origin, CapacityAdmission::Required) {
            let commitment = datasets
                .iter()
                .filter(|dataset| dataset.name.starts_with(&format!("{root}/")))
                .map(|dataset| dataset.refquota)
                .try_fold(requested, u64::checked_add)
                .ok_or_else(|| {
                    VolumeError::from("Provisioned Volume commitments overflowed u64")
                })?;
            let managed_used = datasets
                .iter()
                .filter(|dataset| dataset.name.starts_with(&format!("{root}/")))
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
        }

        if !datasets.iter().any(|dataset| dataset.name == root) {
            self.zfs(&[
                "create",
                "-o",
                "canmount=off",
                "-o",
                &format!("mountpoint={MOUNT_ROOT}"),
                &root,
            ])
            .await?;
        }
        self.zfs(&["create", "-o", &format!("refquota={requested}"), &volume])
            .await?;
        Ok(())
    }

    pub(super) async fn mountpoint(&self, name: &DockerVolumeName) -> Result<String> {
        let _guard = self.admit_mutation().await?;
        let pool = self.one_pool().await?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} does not exist"))?;
        dataset.require_provisioned(name)?;
        dataset.require_writable()?;
        if dataset.mounted {
            return Ok(dataset.mountpoint.clone());
        }
        self.zfs(&["mount", &dataset.name]).await?;
        let datasets = self.datasets(&pool).await?;
        let dataset = Self::dataset(&datasets, &pool, name)?
            .ok_or_else(|| format!("Provisioned Volume {name} disappeared while mounting"))?;
        dataset.require_provisioned(name)?;
        dataset.require_writable()?;
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
            "Volume {name} already has a {}-byte bound; changing it to {requested} bytes is a separate update operation",
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

pub(super) async fn checked_command(program: &PathBuf, args: &[&str]) -> Result<String> {
    let mut attempt = 0;
    let output = loop {
        match Command::new(program).args(args).output().await {
            // ETXTBSY means exec never started; only that launch error is safe to retry.
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy && attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(10 << attempt)).await;
                attempt += 1;
            }
            output => {
                break output
                    .map_err(|error| format!("could not run {}: {error}", program.display()))?;
            }
        }
    };
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
