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
    /// Returns an error when the marker is missing or cannot be read as text, as a plain read does.
    pub fn read(&self) -> io::Result<DeviceMarker> {
        match fs::read_to_string(&self.marker) {
            Ok(text) if text == CONFIRMED_DEVICE_POOL_MARKER => Ok(DeviceMarker::Confirmed),
            Ok(text) if text == UNCONFIRMED_DEVICE_POOL_MARKER => Ok(DeviceMarker::Unconfirmed),
            Ok(text) => Ok(DeviceMarker::Unrecognized(text)),
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

    #[must_use]
    pub fn size_bytes(&self) -> NonZeroU64 {
        self.size_bytes
    }

    /// Current allocated ZFS Pool bytes.
    #[must_use]
    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

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

/// What `zpool import` prints on stderr, under `LC_ALL=C`, when it finds nothing to import.
const NOTHING_TO_IMPORT: &str = "no pools available to import";

/// The Pool names one `zpool import` label scan, run under `LC_ALL=C`, found. It accepts
/// exactly the two shapes ZFS prints: exit 0 with nothing on stdout and
/// "no pools available to import" on stderr, which finds nothing, and exit 0 with `pool:`
/// blocks on stdout and nothing on stderr. Anything else fails, so an output this judge
/// hasn't seen never reads as nothing to import.
///
/// # Errors
///
/// Returns an error for a failed scan or any other output.
pub fn scan(
    status: std::process::ExitStatus,
    stdout: &str,
    stderr: &str,
) -> Result<Vec<String>, Error> {
    let (stdout, stderr) = (stdout.trim(), stderr.trim());
    let unexpected = || {
        Error(format!(
            "zpool import {}: {}",
            if status.success() {
                "printed what Ployz doesn't recognize"
            } else {
                "failed"
            },
            if stderr.is_empty() { stdout } else { stderr }
        ))
    };
    if !status.success() {
        return Err(unexpected());
    }
    if stdout.is_empty() && stderr == NOTHING_TO_IMPORT {
        return Ok(Vec::new());
    }
    if !stderr.is_empty() {
        return Err(unexpected());
    }
    let pools: Vec<String> = stdout
        .lines()
        .filter_map(|line| line.trim().strip_prefix("pool:").map(str::trim))
        .map(str::to_owned)
        .collect();
    if pools.is_empty() || pools.iter().any(String::is_empty) {
        return Err(unexpected());
    }
    Ok(pools)
}

/// Reads the Pool names from one successful `zpool import` label scan, as the installer
/// reads it.
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
    if output.is_empty() || output == NOTHING_TO_IMPORT {
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

/// One imported Pool, whatever its health or mode.
#[derive(Debug, Eq, PartialEq)]
pub struct Imported {
    pub pool: MachinePool,
    pub health: String,
    pub readonly: bool,
}

/// Every imported Pool in [`ZPOOL_LIST`] output, read-only and unhealthy ones included.
///
/// # Errors
///
/// Returns an error for malformed output.
pub fn imported(output: &str) -> Result<Vec<Imported>, Error> {
    output.lines().map(parse_any).collect()
}

/// The vdev paths `zpool list -v -H -P -o name POOL` prints: every line after the Pool's
/// own, as `<TAB><path><TAB>…`.
#[must_use]
pub fn vdevs(output: &str) -> Vec<&std::ffi::OsStr> {
    output
        .lines()
        .skip(1)
        .filter_map(|line| line.trim_start_matches('\t').split('\t').next())
        .filter(|path| !path.is_empty())
        .map(std::ffi::OsStr::new)
        .collect()
}

/// What a Machine records about the Pool it owns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Records {
    pub backing: bool,
    pub device: bool,
    pub mount_root: bool,
}

/// What the judge of [`read`] sees, gathered without changing anything.
#[derive(Debug, Default)]
pub struct Inventory {
    pub records: Records,
    /// Every imported Pool, or `None` when `zpool` isn't installed.
    pub imported: Option<Vec<Imported>>,
    /// The imported Pool whose only vdev is the canonical backing file.
    pub on_backing: Option<String>,
    /// What the default scan, which sees devices only, could import.
    pub scan: Vec<String>,
    /// What a scan of the backing file could import, when the backing file is recorded.
    pub backing_scan: Vec<String>,
}

/// How a Machine knows a Pool is its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum By {
    File,
    Device,
    Sole,
}

/// The Pool a Machine reads, by the records it keeps and what ZFS shows, or why it can't
/// tell.
#[derive(Debug, Eq, PartialEq)]
pub enum Read {
    Unheld,
    Pool {
        pool: MachinePool,
        health: String,
        by: By,
    },
    Unproven(String),
}

/// Judges which Pool is this Machine's own. It never picks one by elimination where a
/// record says otherwise, and any Pool it can't account for leaves it unproven.
#[must_use]
pub fn read(inventory: Inventory) -> Read {
    let Inventory {
        records,
        imported,
        on_backing,
        scan,
        backing_scan,
    } = inventory;
    let Some(imported) = imported else {
        return if records == Records::default() {
            Read::Unheld
        } else {
            Read::Unproven("it records a Pool but can't list Pools".to_owned())
        };
    };
    let found = {
        let mut found: Vec<String> = imported
            .iter()
            .map(|imported| format!("{} (imported)", imported.pool.name))
            .chain(scan.iter().map(|name| format!("{name} (not imported)")))
            .collect();
        found.sort();
        found.join(", ")
    };
    let unproven = || Read::Unproven(format!("it can't tell which of these is its own: {found}"));
    let ployz = |name: &str| name.starts_with(ROOT_BACKED_POOL_NAME);
    match records {
        Records {
            backing: true,
            device: true,
            ..
        } => Read::Unproven("it records both a disk Pool and a file Pool".to_owned()),
        Records { backing: true, .. } => match on_backing {
            Some(_) if scan.iter().any(|name| ployz(name)) => unproven(),
            Some(name) => take(imported, &name, By::File)
                .unwrap_or_else(|| Read::Unproven(format!("its Pool {name} isn't imported"))),
            None if imported
                .iter()
                .any(|imported| imported.pool.name == ROOT_BACKED_POOL_NAME) =>
            {
                Read::Unproven(format!(
                    "the imported Pool {ROOT_BACKED_POOL_NAME} isn't the one in its Pool file"
                ))
            }
            None if backing_scan.is_empty() => {
                Read::Unproven("its Pool file holds no Pool it can import".to_owned())
            }
            None => Read::Unproven(format!(
                "its Pool {} isn't imported",
                backing_scan.join(", ")
            )),
        },
        Records { device: true, .. } => {
            let devices = imported
                .iter()
                .filter(|imported| imported.pool.name == DEVICE_BACKED_POOL_NAME)
                .count();
            let others = imported
                .iter()
                .map(|imported| imported.pool.name.as_str())
                .chain(scan.iter().map(String::as_str))
                .filter(|name| ployz(name))
                .count();
            if devices == 1 && others == 1 {
                take(imported, DEVICE_BACKED_POOL_NAME, By::Device).unwrap_or_else(unproven)
            } else {
                unproven()
            }
        }
        Records { .. } if imported.len() == 1 && scan.is_empty() => {
            let name = imported
                .first()
                .map(|imported| imported.pool.name.clone())
                .unwrap_or_default();
            take(imported, &name, By::Sole).unwrap_or_else(unproven)
        }
        Records {
            mount_root: true, ..
        } => unproven(),
        Records { .. } if imported.is_empty() && scan.is_empty() => Read::Unheld,
        Records { .. } => Read::Unproven(format!(
            "it records no Pool, and has these attached: {found}"
        )),
    }
}

fn take(imported: Vec<Imported>, name: &str, by: By) -> Option<Read> {
    let Imported { pool, health, .. } = imported
        .into_iter()
        .find(|imported| imported.pool.name == name)?;
    Some(Read::Pool { pool, health, by })
}

fn parse(line: &str) -> Result<Option<MachinePool>, Error> {
    let Imported {
        pool,
        health,
        readonly,
    } = parse_any(line)?;
    Ok((matches!(health.as_str(), "ONLINE" | "DEGRADED") && !readonly).then_some(pool))
}

fn parse_any(line: &str) -> Result<Imported, Error> {
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
    Ok(Imported {
        pool: MachinePool {
            name: name.to_owned(),
            size_bytes,
            used_bytes,
            free_bytes,
        },
        health: health.to_owned(),
        readonly: readonly == "on",
    })
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
        assert_eq!(files.read().unwrap_err().kind(), io::ErrorKind::NotFound);
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
        assert_eq!(files.read().unwrap_err().kind(), io::ErrorKind::NotFound);
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

    fn status(code: i32) -> std::process::ExitStatus {
        std::os::unix::process::ExitStatusExt::from_raw(code << 8)
    }

    /// Only the two shapes ZFS prints under `LC_ALL=C` read; a scan that failed, printed a
    /// translated message or warned beside its Pools never reads as nothing to import.
    #[test]
    fn a_label_scan_reads_only_what_zfs_prints_in_the_c_locale() {
        assert!(
            scan(status(0), "", "no pools available to import\n")
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            scan(
                status(0),
                "   pool: ployz\n     id: 1\n   pool: ployz-device\n",
                ""
            )
            .unwrap(),
            ["ployz", "ployz-device"]
        );
        for (code, stdout, stderr) in [
            (0, "", "aucun pool disponible pour l'importation"),
            (0, "", ""),
            (
                0,
                "   pool: ployz\n",
                "cannot open '/dev/sdb': Permission denied",
            ),
            (1, "", "no pools available to import"),
            (0, "unexpected", ""),
        ] {
            assert!(
                scan(status(code), stdout, stderr).is_err(),
                "{code} {stdout:?} {stderr:?}"
            );
        }
    }

    fn pool(name: &str, health: &str) -> Imported {
        parse_any(&format!("{name}\t100\t1\t99\t{health}\toff")).unwrap()
    }

    fn judged(
        records: Records,
        imported: &[&str],
        on_backing: Option<&str>,
        scan: &[&str],
    ) -> Read {
        read(Inventory {
            records,
            imported: Some(imported.iter().map(|name| pool(name, "ONLINE")).collect()),
            on_backing: on_backing.map(str::to_owned),
            scan: scan.iter().map(|name| (*name).to_owned()).collect(),
            backing_scan: Vec::new(),
        })
    }

    fn owned(read: &Read) -> Option<(&str, By)> {
        match read {
            Read::Pool { pool, by, .. } => Some((pool.name(), *by)),
            Read::Unheld | Read::Unproven(_) => None,
        }
    }

    const BACKING: Records = Records {
        backing: true,
        device: false,
        mount_root: true,
    };
    const DEVICE: Records = Records {
        backing: false,
        device: true,
        mount_root: true,
    };
    const NONE: Records = Records {
        backing: false,
        device: false,
        mount_root: false,
    };

    /// A Machine reads the Pool its records name, and none at all where another attached
    /// Pool could be its own: picking one by elimination answers from the wrong data.
    #[test]
    fn a_machine_reads_only_the_pool_its_records_name() {
        let file = judged(BACKING, &["ployz", "tank"], Some("ployz"), &[]);
        assert_eq!(owned(&file), Some(("ployz", By::File)));
        // A Pool named ployz that isn't the one in the backing file is someone else's.
        let elsewhere = judged(BACKING, &["ployz"], None, &[]);
        assert_eq!(owned(&elsewhere), None);
        let beside = judged(BACKING, &["ployz"], Some("ployz"), &["ployz-device"]);
        assert_eq!(owned(&beside), None);

        let device = judged(DEVICE, &["ployz-device", "tank"], None, &[]);
        assert_eq!(owned(&device), Some(("ployz-device", By::Device)));
        let unimported = judged(DEVICE, &["ployz"], None, &["ployz-device"]);
        assert_eq!(owned(&unimported), None);

        let sole = judged(NONE, &["tank"], None, &[]);
        assert_eq!(owned(&sole), Some(("tank", By::Sole)));
        assert_eq!(judged(NONE, &[], None, &[]), Read::Unheld);
        assert_eq!(owned(&judged(NONE, &["tank"], None, &["ployz"])), None);
        assert_eq!(owned(&judged(NONE, &["a", "b"], None, &[])), None);
        // Volumes left mounted from a Pool not imported now leave it unproven.
        let mounted = Records {
            mount_root: true,
            ..NONE
        };
        assert_eq!(owned(&judged(mounted, &[], None, &[])), None);
        assert!(matches!(
            judged(BOTH, &["ployz"], Some("ployz"), &[]),
            Read::Unproven(_)
        ));
        let missing = read(Inventory {
            records: DEVICE,
            ..Inventory::default()
        });
        assert!(matches!(missing, Read::Unproven(_)));
    }

    const BOTH: Records = Records {
        backing: true,
        device: true,
        mount_root: true,
    };
}
