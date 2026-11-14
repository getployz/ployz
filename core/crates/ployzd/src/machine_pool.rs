//! Imported Machine Pool eligibility and the files that record which Pool a Machine owns.

use std::{
    fs, io,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

/// Ownership written only after the device Pool has been verified.
pub const CONFIRMED_DEVICE_POOL_MARKER: &str = "ployz-device\nconfirmed\n";

/// What an earlier release wrote before it verified the device Pool.
pub const UNCONFIRMED_DEVICE_POOL_MARKER: &str = "ployz-device\n";

/// What a device Pool's marker file holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceMarker {
    Absent,
    Unconfirmed,
    Confirmed,
    Unrecognized(String),
}

/// The Machine Pool's backing file and the marker beside it, which survives a missing device so
/// recovery cannot replace a device Pool with a root-backed one.
#[derive(Clone, Debug)]
pub struct DevicePoolFiles {
    backing: PathBuf,
    marker: PathBuf,
}

impl DevicePoolFiles {
    /// The marker is the backing path with `.device` appended.
    #[must_use]
    pub fn new(backing: PathBuf) -> Self {
        let mut marker = backing.as_os_str().to_owned();
        marker.push(".device");
        Self {
            backing,
            marker: PathBuf::from(marker),
        }
    }

    /// The file a root-backed Pool lives in.
    #[must_use]
    pub fn backing(&self) -> &Path {
        &self.backing
    }

    /// The file that records device ownership.
    #[must_use]
    pub fn marker(&self) -> &Path {
        &self.marker
    }

    /// What the marker holds now.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker exists and cannot be read as text.
    pub fn read(&self) -> io::Result<DeviceMarker> {
        match fs::read_to_string(&self.marker) {
            Ok(text) if text == CONFIRMED_DEVICE_POOL_MARKER => Ok(DeviceMarker::Confirmed),
            Ok(text) if text == UNCONFIRMED_DEVICE_POOL_MARKER => Ok(DeviceMarker::Unconfirmed),
            Ok(text) => Ok(DeviceMarker::Unrecognized(text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DeviceMarker::Absent),
            Err(error) => Err(error),
        }
    }

    /// Records verified device ownership, replacing the marker whole or not at all.
    ///
    /// # Errors
    ///
    /// Returns the write failure.
    pub fn confirm(&self) -> io::Result<()> {
        crate::filesystem::atomic_write(
            &self.marker,
            CONFIRMED_DEVICE_POOL_MARKER.as_bytes(),
            0o644,
        )
    }

    /// Records verified device ownership in place, as the volume plugin has always written it.
    ///
    /// # Errors
    ///
    /// Returns the write failure.
    pub fn confirm_in_place(&self) -> io::Result<()> {
        fs::write(&self.marker, CONFIRMED_DEVICE_POOL_MARKER)
    }

    /// Removes the marker.
    ///
    /// # Errors
    ///
    /// Returns the removal failure.
    pub fn discard(&self) -> io::Result<()> {
        fs::remove_file(&self.marker)
    }
}

/// The Pool whose file backing the volume plugin can grow.
pub const ROOT_BACKED_POOL_NAME: &str = "ployz";

/// The Pool created on a disk explicitly selected during installation.
pub const DEVICE_BACKED_POOL_NAME: &str = "ployz-device";

/// The `zpool` arguments whose output [`one_usable`] reads.
pub const ZPOOL_LIST: [&str; 4] = [
    "list",
    "-Hp",
    "-o",
    "name,size,allocated,free,health,readonly",
];

/// A writable imported Machine Pool with usable health.
#[derive(Debug, Eq, PartialEq)]
pub struct MachinePool {
    name: String,
    size_bytes: NonZeroU64,
    used_bytes: u64,
    free_bytes: u64,
}

impl MachinePool {
    /// The imported ZFS Pool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Current ZFS Pool size in bytes.
    #[must_use]
    pub fn size_bytes(&self) -> NonZeroU64 {
        self.size_bytes
    }

    /// Current allocated ZFS Pool bytes.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// Current free ZFS Pool bytes.
    #[must_use]
    pub fn free_bytes(&self) -> u64 {
        self.free_bytes
    }
}

/// Invalid or unusable imported Machine Pool evidence.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

/// Selects exactly one writable `ONLINE` or `DEGRADED` imported Machine Pool.
///
/// `None` means no Pool is imported. Imported but unusable or ambiguous Pools
/// are errors.
///
/// # Errors
///
/// Returns an error for malformed output, no usable imported Pool, or several
/// usable imported Pools.
pub fn one_usable(output: &str) -> Result<Option<MachinePool>, Error> {
    let mut imported = 0;
    let mut usable = Vec::new();
    for line in output.lines() {
        imported += 1;
        if let Some(pool) = parse(line)? {
            usable.push(pool);
        }
    }
    match usable.len() {
        0 if imported == 0 => Ok(None),
        0 => Err(Error(
            "no usable existing Machine Pool; automatic Pool creation is tracked by #541".into(),
        )),
        1 => Ok(usable.pop()),
        _ => Err(Error(format!(
            "multiple usable Machine Pools ({}) are ambiguous; Ployz will not choose one",
            usable
                .iter()
                .map(MachinePool::name)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Reads the Pool names from one successful `zpool import` label scan.
///
/// # Errors
///
/// Returns an error when non-empty output contains no complete Pool name.
#[expect(
    clippy::needless_lifetimes,
    reason = "the repository standard requires naming the output borrow's source lifetime"
)]
pub fn importable_names<'output>(output: &'output str) -> Result<Vec<&'output str>, Error> {
    let output = output.trim();
    if output.is_empty() || output == "no pools available to import" {
        return Ok(Vec::new());
    }
    let pools: Vec<_> = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pool:").map(str::trim))
        .collect();
    if pools.is_empty() || pools.contains(&"") {
        return Err(Error(format!("invalid ZFS Pool import output: {output}")));
    }
    Ok(pools)
}

fn parse(line: &str) -> Result<Option<MachinePool>, Error> {
    let mut fields = line.split('\t');
    let invalid = || Error(format!("invalid ZFS Pool output: {line}"));
    let (
        Some(name),
        Some(size_bytes),
        Some(used_bytes),
        Some(free_bytes),
        Some(health),
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
    )
    else {
        return Err(invalid());
    };
    let size_bytes = size_bytes.parse::<NonZeroU64>().map_err(|_| invalid())?;
    let used_bytes = used_bytes.parse::<u64>().map_err(|_| invalid())?;
    let free_bytes = free_bytes.parse::<u64>().map_err(|_| invalid())?;
    if name.is_empty()
        || !matches!(
            health,
            "ONLINE" | "DEGRADED" | "FAULTED" | "OFFLINE" | "REMOVED" | "UNAVAIL" | "SUSPENDED"
        )
        || !matches!(readonly, "on" | "off")
    {
        return Err(invalid());
    }
    Ok(
        (matches!(health, "ONLINE" | "DEGRADED") && readonly == "off").then(|| MachinePool {
            name: name.to_owned(),
            size_bytes,
            used_bytes,
            free_bytes,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_marker_sits_beside_the_backing_file_where_machines_keep_it() {
        let layout = ployz_core::MachineLayout::production();
        let files = DevicePoolFiles::new(layout.machine_pool_file());
        assert_eq!(files.backing().as_os_str(), "/var/lib/ployz-machine-pool");
        assert_eq!(
            files.marker().as_os_str(),
            "/var/lib/ployz-machine-pool.device"
        );
        assert_eq!(files.marker(), layout.device_pool_marker());
    }

    #[test]
    fn the_marker_holds_one_of_two_accepted_texts() {
        assert_eq!(CONFIRMED_DEVICE_POOL_MARKER, "ployz-device\nconfirmed\n");
        assert_eq!(UNCONFIRMED_DEVICE_POOL_MARKER, "ployz-device\n");
        let root = tempfile::tempdir().unwrap();
        let files = DevicePoolFiles::new(root.path().join("machine-pool"));
        assert_eq!(files.read().unwrap(), DeviceMarker::Absent);
        for (text, expected) in [
            (CONFIRMED_DEVICE_POOL_MARKER, DeviceMarker::Confirmed),
            (UNCONFIRMED_DEVICE_POOL_MARKER, DeviceMarker::Unconfirmed),
            (
                "ployz-device",
                DeviceMarker::Unrecognized("ployz-device".into()),
            ),
            ("", DeviceMarker::Unrecognized(String::new())),
        ] {
            fs::write(files.marker(), text).unwrap();
            assert_eq!(files.read().unwrap(), expected);
        }
        fs::write(files.marker(), [0xff, 0xfe]).unwrap();
        assert_eq!(files.read().unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn both_confirmations_write_the_same_bytes_and_discard_removes_them() {
        let root = tempfile::tempdir().unwrap();
        let files = DevicePoolFiles::new(root.path().join("machine-pool"));
        files.confirm().unwrap();
        assert_eq!(
            fs::read_to_string(files.marker()).unwrap(),
            "ployz-device\nconfirmed\n"
        );
        fs::write(files.marker(), UNCONFIRMED_DEVICE_POOL_MARKER).unwrap();
        files.confirm_in_place().unwrap();
        assert_eq!(files.read().unwrap(), DeviceMarker::Confirmed);
        files.discard().unwrap();
        assert_eq!(files.read().unwrap(), DeviceMarker::Absent);
        assert_eq!(files.discard().unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn malformed_output_is_an_error() {
        assert_eq!(
            one_usable("broken\tnot-a-size\t1\t1\tONLINE\toff")
                .unwrap_err()
                .to_string(),
            "invalid ZFS Pool output: broken\tnot-a-size\t1\t1\tONLINE\toff"
        );
    }

    #[test]
    fn import_scan_extracts_names_and_distinguishes_no_pool() {
        assert!(
            importable_names("no pools available to import\n")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            importable_names("   pool: ployz\n     id: 1\n   pool: other\n").unwrap(),
            ["ployz", "other"]
        );
        assert!(importable_names("unexpected").is_err());
    }
}
