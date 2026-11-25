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

/// The `zpool list` arguments whose output [`imported`] reads: [`ZPOOL_LIST`] with each
/// Pool's guid on its line, so another Pool imported under the same name reads as changed.
pub const ZPOOL_IDS: [&str; 4] = [
    "list",
    "-Hp",
    "-o",
    "name,guid,size,allocated,free,health,readonly",
];

/// A writable imported Machine Pool with usable health.
#[derive(Clone, Debug, Eq, PartialEq)]
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
    blocks(stdout).ok_or_else(unexpected)
}

/// The keys `zpool import` prints in a Pool's block.
const BLOCK_KEYS: [&str; 9] = [
    "pool", "id", "state", "status", "action", "see", "config", "comment", "errata",
];

/// The Pool names in a label scan's `pool:` blocks, when every line belongs to one: a
/// `key: value` line with a key ZFS prints, a tab-indented continuation or config line,
/// or a blank line. Each block gives its `id` and `state` a value, and a config tree after
/// `config:`. `None` for anything else, so a line this judge hasn't seen never hides a
/// Pool.
fn blocks(stdout: &str) -> Option<Vec<String>> {
    let mut pools = Vec::new();
    // The keys this block printed, and `tree` once a config line follows its `config:`.
    let mut keys: Vec<&str> = Vec::new();
    let complete = |keys: &[&str]| ["id", "state", "tree"].iter().all(|key| keys.contains(key));
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with('\t') && !pools.is_empty() {
            if keys.contains(&"config") && !keys.contains(&"tree") {
                keys.push("tree");
            }
            continue;
        }
        let (key, value) = line.trim_start_matches(' ').split_once(':')?;
        if !BLOCK_KEYS.contains(&key) || !(value.is_empty() || value.starts_with(' ')) {
            return None;
        }
        // ZFS prints a value after each of these, and its tree after `config:`.
        if matches!(key, "pool" | "id" | "state") && value.trim().is_empty()
            || key == "config" && !value.trim().is_empty()
        {
            return None;
        }
        if key == "pool" {
            if !pools.is_empty() && !complete(&keys) {
                return None;
            }
            let name = value.trim();
            if name.is_empty() {
                return None;
            }
            pools.push(name.to_owned());
            keys.clear();
        } else if pools.is_empty() {
            return None;
        }
        keys.push(key);
    }
    (!pools.is_empty() && complete(&keys)).then_some(pools)
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Imported {
    pub pool: MachinePool,
    /// Its guid, from [`ZPOOL_IDS`]; empty where [`ZPOOL_LIST`] listed it.
    pub guid: String,
    pub health: String,
    pub readonly: bool,
}

impl Imported {
    /// Whether Volumes may be written to it: writable, and `ONLINE` or `DEGRADED`.
    #[must_use]
    pub fn usable(&self) -> bool {
        !self.readonly && matches!(self.health.as_str(), "ONLINE" | "DEGRADED")
    }
}

/// Every imported Pool in [`ZPOOL_IDS`] output, read-only and unhealthy ones included.
///
/// # Errors
///
/// Returns an error for malformed output.
pub fn imported(output: &str) -> Result<Vec<Imported>, Error> {
    output.lines().map(parse_ids).collect()
}

/// The leaf vdev paths `zpool list -v -H -P -o name POOL` prints for `pool`: its own
/// line, then a `<TAB><vdev><TAB>…` line per vdev, where a vdev is a leaf's path or a
/// group such as `mirror-0`, and a `logs`, `cache`, `spare`, `special` or `dedup` header
/// padded with dashes before each class. Any other line fails, so a listing cut short or
/// reworded never reads as a Pool on some other file.
///
/// # Errors
///
/// Returns an error when the listing has no leaf or a line it doesn't recognize.
pub fn vdevs<'output>(
    output: &'output str,
    pool: &str,
) -> Result<Vec<&'output std::ffi::OsStr>, Error> {
    let invalid = || Error(format!("invalid ZFS vdev listing for {pool}: {output}"));
    let mut lines = output.lines();
    if lines.next() != Some(pool) {
        return Err(invalid());
    }
    let mut paths = Vec::new();
    for line in lines {
        match line.strip_prefix('\t').map(|line| line.split('\t').next()) {
            Some(Some(path)) if path.starts_with('/') => paths.push(std::ffi::OsStr::new(path)),
            Some(Some(vdev)) if group(vdev) => {}
            Some(_) => return Err(invalid()),
            None => {
                let mut words = line.split_whitespace();
                let header = words.next().is_some_and(|class| {
                    matches!(
                        class,
                        "logs" | "cache" | "spare" | "spares" | "special" | "dedup"
                    )
                });
                if !header || words.any(|word| word != "-") {
                    return Err(invalid());
                }
            }
        }
    }
    if paths.is_empty() {
        return Err(invalid());
    }
    Ok(paths)
}

/// Whether `vdev` names one of ZFS's interior vdevs, which hold leaves and no path:
/// `mirror-0`, `raidz2-1`, `draid1:1d:3c:0s-0`, `spare-0`, `replacing-0` or `indirect-1`.
fn group(vdev: &str) -> bool {
    let Some((kind, id)) = vdev.rsplit_once('-') else {
        return false;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let draid = kind.strip_prefix("draid").is_some_and(|layout| {
        let mut parts = layout.split(':');
        parts
            .next()
            .is_some_and(|parity| matches!(parity, "1" | "2" | "3"))
            && ["d", "c", "s"].iter().all(|unit| {
                parts
                    .next()
                    .and_then(|part| part.strip_suffix(unit))
                    .is_some_and(digits)
            })
            && parts.next().is_none()
    });
    digits(id)
        && (draid
            || matches!(
                kind,
                "mirror" | "raidz1" | "raidz2" | "raidz3" | "spare" | "replacing" | "indirect"
            ))
}

/// What a Machine records about the Pool it owns.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Records {
    pub backing: bool,
    pub device: bool,
    pub mount_root: bool,
}

/// What [`choose`] sees, gathered without changing anything.
#[derive(Debug, Default)]
pub struct Inventory {
    pub records: Records,
    /// Every imported Pool, or `None` when `zpool` isn't installed.
    pub imported: Option<Vec<Imported>>,
    /// The imported Pool whose only vdev is the canonical backing file.
    pub on_backing: Option<String>,
    /// Each canonical vdev path that more than one imported Pool lists, with those Pools,
    /// as `path (a, b)`.
    pub collisions: Vec<String>,
    /// What the default scan, which sees devices only, could import. Empty where no
    /// scan ran, as on the write path.
    pub scan: Vec<String>,
    /// What a scan of the backing file could import, when the backing file is recorded.
    pub backing_scan: Vec<String>,
    /// The imported Pools listed again after the scans, where they ran. A Pool imported or
    /// exported between the two listings is in neither the first listing nor the scans.
    pub relisted: Option<Vec<Imported>>,
}

/// How a Machine knows a Pool is its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum By {
    File,
    Device,
    Sole,
}

/// The Pool a Machine's records and its ZFS make its own, before anything asks whether
/// it can be written or proves anything.
#[derive(Debug, Eq, PartialEq)]
pub enum Choice {
    /// It records no Pool and has none imported.
    None,
    /// It records a Pool that isn't imported, and no other imported Pool is usable, so
    /// recovery may import it.
    Missing(String),
    Pool {
        imported: Imported,
        by: By,
    },
    /// It can't tell which Pool is its own.
    Unproven(String),
}

/// Chooses this Machine's own Pool, for every path that reads or writes it: the
/// imported Pool its records name, and none where any other imported Pool is usable or
/// a Pool it can't account for could be its own. It never picks one by elimination where
/// a record says otherwise.
#[must_use]
pub fn choose(inventory: &Inventory) -> Choice {
    let Inventory {
        records,
        imported,
        on_backing,
        collisions,
        scan,
        backing_scan,
        relisted,
    } = inventory;
    if !collisions.is_empty() {
        return Choice::Unproven(format!(
            "more than one imported Pool lists the same file: {}",
            collisions.join(", ")
        ));
    }
    if let (Some(before), Some(after)) = (imported, relisted) {
        let (before, after) = (ids(before), ids(after));
        if before != after {
            return Choice::Unproven(format!(
                "its imported Pools changed while it read them, from {before} to {after}"
            ));
        }
    }
    let Some(imported) = imported else {
        return if *records == Records::default() {
            Choice::None
        } else {
            Choice::Missing("it records a Pool but can't list Pools".to_owned())
        };
    };
    let found = found(imported, scan);
    let unproven = || Choice::Unproven(format!("it can't tell which of these is its own: {found}"));
    let ployz = |name: &str| name.starts_with(ROOT_BACKED_POOL_NAME);
    let usable = imported.iter().filter(|imported| imported.usable());
    let (name, by) = match records {
        Records {
            backing: true,
            device: true,
            ..
        } => {
            return Choice::Unproven("it records both a disk Pool and a file Pool".to_owned());
        }
        Records { backing: true, .. } => match on_backing {
            Some(name) if !backing_scan.is_empty() => {
                return Choice::Unproven(format!(
                    "its Pool file holds {}, which isn't the imported Pool {name}",
                    backing_scan.join(", ")
                ));
            }
            Some(_) if scan.iter().any(|name| ployz(name)) => return unproven(),
            Some(name) => (name.as_str(), By::File),
            None if imported
                .iter()
                .any(|imported| imported.pool.name == ROOT_BACKED_POOL_NAME) =>
            {
                return Choice::Unproven(format!(
                    "the imported Pool {ROOT_BACKED_POOL_NAME} isn't the one in its Pool file"
                ));
            }
            None if usable.clone().next().is_some() => return unproven(),
            None if backing_scan.is_empty() => {
                return Choice::Missing("its Pool file holds no Pool it can import".to_owned());
            }
            None => {
                return Choice::Missing(format!(
                    "its Pool {} isn't imported",
                    backing_scan.join(", ")
                ));
            }
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
                (DEVICE_BACKED_POOL_NAME, By::Device)
            } else if devices == 0
                && others <= 1
                && usable.clone().next().is_none()
                && !imported.iter().any(|imported| ployz(&imported.pool.name))
            {
                return Choice::Missing(format!(
                    "its Pool {DEVICE_BACKED_POOL_NAME} isn't imported"
                ));
            } else {
                return unproven();
            }
        }
        Records { .. } => {
            let mut usable = usable.clone();
            match (usable.next(), usable.next()) {
                (Some(sole), None) => (sole.pool.name.as_str(), By::Sole),
                (None, _) if imported.len() == 1 => {
                    let sole = imported.first().map_or("", |sole| sole.pool.name.as_str());
                    (sole, By::Sole)
                }
                (None, _) if imported.is_empty() && records.mount_root => {
                    return Choice::Missing(
                        "it has Volumes left mounted from a Pool that isn't imported".to_owned(),
                    );
                }
                (None, _) if imported.is_empty() => return Choice::None,
                _ => {
                    return Choice::Unproven(format!(
                        "it records no Pool, and has these attached: {found}"
                    ));
                }
            }
        }
    };
    if usable.clone().any(|other| other.pool.name != name) {
        return unproven();
    }
    match imported.iter().find(|imported| imported.pool.name == name) {
        Some(imported) => Choice::Pool {
            imported: imported.clone(),
            by,
        },
        None => unproven(),
    }
}

/// The Pool Volumes are written to: what [`choose`] picks, when it is usable. `None`
/// where no Pool is imported, so recovery may import or create one.
///
/// # Errors
///
/// Returns why no Pool is written to, when one is imported but not the Machine's own, or
/// its own can't be written.
pub fn writable(inventory: &Inventory) -> Result<Option<MachinePool>, Error> {
    match choose(inventory) {
        Choice::None | Choice::Missing(_) => Ok(None),
        Choice::Pool { imported, .. } if imported.usable() => Ok(Some(imported.pool)),
        Choice::Pool { imported, .. } => Err(Error(format!(
            "This Server won't write to its Pool {}, since it is {}.",
            imported.pool.name,
            unusable(&imported)
        ))),
        Choice::Unproven(why) => Err(Error(format!(
            "This Server won't write to a Pool, since {why}."
        ))),
    }
}

fn unusable(imported: &Imported) -> String {
    if imported.readonly {
        "imported read-only".to_owned()
    } else {
        imported.health.clone()
    }
}

/// Each Pool in `imported` as `name (guid)`, in name order, or `none`.
fn ids(imported: &[Imported]) -> String {
    let mut ids: Vec<String> = imported
        .iter()
        .map(|imported| format!("{} ({})", imported.pool.name, imported.guid))
        .collect();
    ids.sort();
    if ids.is_empty() {
        "none".to_owned()
    } else {
        ids.join(", ")
    }
}

fn found(imported: &[Imported], scan: &[String]) -> String {
    let mut found: Vec<String> = imported
        .iter()
        .map(|imported| format!("{} (imported)", imported.pool.name))
        .chain(scan.iter().map(|name| format!("{name} (not imported)")))
        .collect();
    found.sort();
    found.join(", ")
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

/// What a reader needs of the Pool [`choose`] picks. Either way it must be the only Pool
/// imported, with nothing a label scan could import.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Need {
    /// An answer that something is absent, or a restore that relies on one: only an
    /// `ONLINE` Pool.
    Proof,
    /// A departure's seal, which a `DEGRADED` Pool still takes.
    Seal,
}

/// Judges which Pool is this Machine's own for `need`. It is [`choose`]'s Pool, or why it
/// can't be relied on.
#[must_use]
pub fn read(inventory: &Inventory, need: Need) -> Read {
    let scan = &inventory.scan;
    let imported = inventory.imported.as_deref().unwrap_or_default();
    match choose(inventory) {
        Choice::None if scan.is_empty() => Read::Unheld,
        Choice::None => Read::Unproven(format!(
            "it records no Pool, and has these attached: {}",
            found(imported, scan)
        )),
        Choice::Missing(why) | Choice::Unproven(why) => Read::Unproven(why),
        Choice::Pool { imported: own, by } => {
            let name = &own.pool.name;
            let healthy = match need {
                Need::Proof => !own.readonly && own.health == "ONLINE",
                Need::Seal => own.usable(),
            };
            if !healthy {
                return Read::Unproven(format!("its Pool {name} is {}", unusable(&own)));
            }
            // Any other Pool it can see, imported or importable and whatever its name, could
            // be the home of its Pool under another name, so it proves nothing.
            if imported.len() != 1 || !scan.is_empty() {
                let found = found(imported, scan);
                return Read::Unproven(match by {
                    By::Sole => format!("it records no Pool, and has these attached: {found}"),
                    By::File | By::Device => {
                        format!("it can't tell which of these is its own: {found}")
                    }
                });
            }
            Read::Pool {
                pool: own.pool,
                health: own.health,
                by,
            }
        }
    }
}

fn parse(line: &str) -> Result<Option<MachinePool>, Error> {
    let imported = parse_any(line)?;
    Ok(imported.usable().then_some(imported.pool))
}

/// One [`ZPOOL_IDS`] line: a name, a guid of digits, then a [`ZPOOL_LIST`] line's rest.
fn parse_ids(line: &str) -> Result<Imported, Error> {
    let invalid = || Error(format!("invalid ZFS Pool output: {line}"));
    let (name, rest) = line.split_once('\t').ok_or_else(invalid)?;
    let (guid, rest) = rest.split_once('\t').ok_or_else(invalid)?;
    if guid.is_empty() || !guid.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let mut imported = parse_any(&format!("{name}\t{rest}")).map_err(|_| invalid())?;
    imported.guid = guid.to_owned();
    Ok(imported)
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
        guid: String::new(),
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

    /// A block as ZFS 2.1 and 2.2 print it for a Pool they could import.
    fn block(name: &str) -> String {
        format!(
            "   pool: {name}\n     id: 1\n  state: ONLINE\n action: The pool can be imported using its name or numeric identifier.\n config:\n\n\t{name}  ONLINE\n\t  loop0  ONLINE\n"
        )
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
        let degraded = " status: One or more devices are missing from the system.\n action: The pool can be imported despite missing or damaged devices.  The\n\tfault tolerance of the pool may be compromised if imported.\n   see: https://openzfs.github.io/openzfs-docs/msg/ZFS-8000-2Q\n";
        let two = format!(
            "{}\n{}",
            block("ployz"),
            block("ployz-device")
                .replace(" action: The pool", &format!("{degraded} action: The pool"))
        );
        assert_eq!(
            scan(status(0), &two, "").unwrap(),
            ["ployz", "ployz-device"]
        );
        for (code, stdout, stderr) in [
            (0, String::new(), "aucun pool disponible pour l'importation"),
            (0, String::new(), ""),
            (
                0,
                block("ployz"),
                "cannot open '/dev/sdb': Permission denied",
            ),
            (1, String::new(), "no pools available to import"),
            (0, "unexpected".to_owned(), ""),
            // A recognized block beside a line ZFS doesn't print in one.
            (
                0,
                format!("{}warning: a label is damaged\n", block("ployz")),
                "",
            ),
            (0, format!("note\n{}", block("ployz")), ""),
            (0, format!("{}   pool: tank\n", block("ployz")), ""),
            (0, "   pool: ployz\n     id: 1\n".to_owned(), ""),
            // A block cut short or with a value missing.
            (0, block("ployz").replace("id: 1", "id:"), ""),
            (0, block("ployz").replace("state: ONLINE", "state:"), ""),
            (
                0,
                block("ployz")
                    .split(" config:")
                    .next()
                    .map(|head| format!("{head} config:\n"))
                    .unwrap_or_default(),
                "",
            ),
            (0, block("ployz").replace(" config:", " config: x"), ""),
        ] {
            assert!(
                scan(status(code), &stdout, stderr).is_err(),
                "{code} {stdout:?} {stderr:?}"
            );
        }
    }

    #[test]
    fn a_vdev_listing_reads_only_whole_lines() {
        let listing = "ployz\n\t/var/lib/ployz-machine-pool\t2.93G\t1M\n";
        assert_eq!(
            vdevs(listing, "ployz").unwrap(),
            [std::ffi::OsStr::new("/var/lib/ployz-machine-pool")]
        );
        for listing in [
            "",
            "ployz\n",
            "tank\n\t/var/lib/ployz-machine-pool\t2.93G\n",
            "ployz\n\t/var/lib/ployz-machine-pool\t2.93G\nwarning\n",
            "ployz\n\tmirror\t-\n\t/dev/sdb\t-\n",
            "ployz\n\tmirror-0-x\t-\n\t/dev/sdb\t-\n",
            "ployz\n\tdraid1:1d:3c-0\t-\n\t/dev/sdb\t-\n",
            "ployz\n\t/dev/sdb\t-\nlogs  -  4G\n",
            "ployz\n\t/dev/sdb\t-\nl2arc  -  -\n",
            "ployz\n\tmirror-0\t-\n",
        ] {
            assert!(vdevs(listing, "ployz").is_err(), "{listing:?}");
        }
    }

    /// What ZFS 2.2.2 printed for Pools of file vdevs: a mirror with an active spare and a
    /// vdev of every class, a raidz, a draid, and a Pool with a removed vdev. Groups and
    /// class headers are structure; only the leaves are read.
    #[test]
    fn a_vdev_listing_reads_the_leaves_of_every_layout_zfs_prints() {
        let listings: [(&str, String, Vec<&str>); 4] = [
        (
            "probe",
            [
                "probe\n",
                "\tmirror-0\t240M\t0\t240M\t-\t-\t0%\t0.00%\t-\tONLINE\n",
                "\tspare-0\t-\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f1\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f6\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f2\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "dedup                     -      -      -        -         -      -      -      -         -\n",
                "\t/root/probe/f9\t256M\t0\t240M\t-\t-\t0%\t0.00%\t-\tONLINE\n",
                "special                   -      -      -        -         -      -      -      -         -\n",
                "\tmirror-2\t240M\t178K\t240M\t-\t-\t3%\t0.07%\t-\tONLINE\n",
                "\t/root/probe/f7\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f8\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "logs                      -      -      -        -         -      -      -      -         -\n",
                "\tmirror-1\t240M\t0\t240M\t-\t-\t0%\t0.00%\t-\tONLINE\n",
                "\t/root/probe/f3\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f4\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "cache                     -      -      -        -         -      -      -      -         -\n",
                "\t/root/probe/f5\t256M\t15K\t251M\t-\t-\t0%\t0.00%\t-\tONLINE\n",
                "spare                     -      -      -        -         -      -      -      -         -\n",
                "\t/root/probe/f6\t256M\t-\t-\t-\t-\t-\t-\t-\tINUSE\n",
            ]
            .concat(),
            vec!["/root/probe/f1", "/root/probe/f6", "/root/probe/f2", "/root/probe/f9", "/root/probe/f7", "/root/probe/f8", "/root/probe/f3", "/root/probe/f4", "/root/probe/f5", "/root/probe/f6"],
        ),
        (
            "probez",
            [
                "probez\n",
                "\traidz1-0\t704M\t196K\t704M\t-\t-\t0%\t0.02%\t-\tONLINE\n",
                "\t/root/probe/f10\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f11\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f12\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
            ]
            .concat(),
            vec!["/root/probe/f10", "/root/probe/f11", "/root/probe/f12"],
        ),
        (
            "probed",
            [
                "probed\n",
                "\tdraid1:1d:3c:0s-0\t576M\t213K\t576M\t-\t-\t0%\t0.03%\t-\tONLINE\n",
                "\t/root/probe/f15\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f16\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
                "\t/root/probe/f17\t256M\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
            ]
            .concat(),
            vec!["/root/probe/f15", "/root/probe/f16", "/root/probe/f17"],
        ),
        (
            "prober",
            [
                "prober\n",
                "\t/root/probe/f18\t256M\t148K\t240M\t-\t-\t3%\t0.06%\t-\tONLINE\n",
                "\tindirect-1\t-\t-\t-\t-\t-\t-\t-\t-\tONLINE\n",
            ]
            .concat(),
            vec!["/root/probe/f18"],
        ),
        ];
        for (pool, listing, leaves) in listings {
            let read: Vec<_> = vdevs(&listing, pool)
                .unwrap_or_else(|error| panic!("{pool}: {error}"))
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect();
            assert_eq!(read, leaves, "{pool}");
        }
    }

    fn pool(name: &str, health: &str, readonly: &str) -> Imported {
        parse_any(&format!("{name}\t100\t1\t99\t{health}\t{readonly}")).unwrap()
    }

    fn inventory(
        records: Records,
        imported: &[Imported],
        on_backing: Option<&str>,
        scan: &[&str],
    ) -> Inventory {
        Inventory {
            records,
            imported: Some(imported.to_vec()),
            on_backing: on_backing.map(str::to_owned),
            collisions: Vec::new(),
            scan: scan.iter().map(|name| (*name).to_owned()).collect(),
            backing_scan: Vec::new(),
            relisted: None,
        }
    }

    fn judged(
        records: Records,
        imported: &[&str],
        on_backing: Option<&str>,
        scan: &[&str],
    ) -> Read {
        let imported: Vec<_> = imported
            .iter()
            .map(|name| pool(name, "ONLINE", "off"))
            .collect();
        read(
            &inventory(records, &imported, on_backing, scan),
            Need::Proof,
        )
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
        let file = judged(BACKING, &["ployz"], Some("ployz"), &[]);
        assert_eq!(owned(&file), Some(("ployz", By::File)));
        // A Pool named ployz that isn't the one in the backing file is someone else's.
        let elsewhere = judged(BACKING, &["ployz"], None, &[]);
        assert_eq!(owned(&elsewhere), None);
        let beside = judged(BACKING, &["ployz"], Some("ployz"), &["ployz-device"]);
        assert_eq!(owned(&beside), None);

        let device = judged(DEVICE, &["ployz-device"], None, &[]);
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
        let missing = read(
            &Inventory {
                records: DEVICE,
                ..Inventory::default()
            },
            Need::Proof,
        );
        assert!(matches!(missing, Read::Unproven(_)));
    }

    /// The Pool a Machine reads and the Pool it writes to are one choice: where another
    /// imported Pool could be written, or its own is read-only, neither happens, so a
    /// Volume's root never lands where the proof didn't look.
    #[test]
    fn a_machine_writes_only_to_the_pool_it_reads() {
        let readonly_own = [pool("ployz", "ONLINE", "on"), pool("tank", "ONLINE", "off")];
        let file = inventory(BACKING, &readonly_own, Some("ployz"), &[]);
        assert!(writable(&file).is_err());
        assert!(matches!(read(&file, Need::Proof), Read::Unproven(_)));
        assert!(matches!(read(&file, Need::Seal), Read::Unproven(_)));
        // One recording no Pool writes to its sole writable Pool, and proves nothing beside
        // a read-only one.
        let readonly_other = [
            pool("backup", "ONLINE", "on"),
            pool("tank", "ONLINE", "off"),
        ];
        let none = inventory(NONE, &readonly_other, None, &[]);
        assert_eq!(writable(&none).unwrap().unwrap().name(), "tank");
        assert!(matches!(read(&none, Need::Proof), Read::Unproven(_)));

        for records in [BACKING, DEVICE] {
            let own = if records == BACKING {
                "ployz"
            } else {
                "ployz-device"
            };
            let beside = [pool(own, "ONLINE", "off"), pool("tank", "ONLINE", "off")];
            let both = inventory(records, &beside, Some(own), &[]);
            assert!(writable(&both).is_err(), "{records:?}");
            assert!(matches!(read(&both, Need::Proof), Read::Unproven(_)));
        }
        // A file Pool not imported beside a usable Pool: recovery may not run, nor writes.
        let tank = [pool("tank", "ONLINE", "off")];
        assert!(writable(&inventory(BACKING, &tank, None, &[])).is_err());
        assert_eq!(writable(&inventory(BACKING, &[], None, &[])).unwrap(), None);
        assert_eq!(writable(&inventory(DEVICE, &[], None, &[])).unwrap(), None);
    }

    /// Beside its recorded Pool, any other Pool it can see, imported or importable and
    /// whatever its name, could be its original Pool under another name, so it proves
    /// nothing: the replacement holds none of what the original does.
    #[test]
    fn a_recorded_pool_beside_any_other_pool_proves_nothing() {
        let renamed = [
            pool("ployz-device", "ONLINE", "off"),
            pool("backup", "ONLINE", "on"),
        ];
        let device = inventory(DEVICE, &renamed, None, &[]);
        assert!(matches!(read(&device, Need::Proof), Read::Unproven(_)));
        let alone = [pool("ployz-device", "ONLINE", "off")];
        let scanned = inventory(DEVICE, &alone, None, &["backup"]);
        assert!(matches!(read(&scanned, Need::Proof), Read::Unproven(_)));
        let file = [
            pool("ployz", "ONLINE", "off"),
            pool("backup", "ONLINE", "on"),
        ];
        let beside = inventory(BACKING, &file, Some("ployz"), &[]);
        assert!(matches!(read(&beside, Need::Proof), Read::Unproven(_)));
        let scanned = inventory(BACKING, &file[..1], Some("ployz"), &["backup"]);
        assert!(matches!(read(&scanned, Need::Proof), Read::Unproven(_)));
    }

    /// A `DEGRADED` Pool takes writes and a seal, and proves nothing.
    #[test]
    fn a_degraded_pool_proves_nothing() {
        let degraded = [pool("ployz", "DEGRADED", "off")];
        let file = inventory(BACKING, &degraded, Some("ployz"), &[]);
        assert_eq!(writable(&file).unwrap().unwrap().name(), "ployz");
        assert_eq!(
            read(&file, Need::Proof),
            Read::Unproven("its Pool ployz is DEGRADED".to_owned())
        );
        assert_eq!(owned(&read(&file, Need::Seal)), Some(("ployz", By::File)));
    }

    /// The imported Pool on the backing file's path proves nothing when the file there now
    /// holds another Pool: the open vdev is a file moved away from that path.
    #[test]
    fn a_pool_file_holding_another_pool_proves_nothing() {
        let imported = [pool("ployz", "ONLINE", "off")];
        let mut moved = inventory(BACKING, &imported, Some("ployz"), &[]);
        moved.backing_scan = vec!["ployz".to_owned()];
        assert!(matches!(read(&moved, Need::Proof), Read::Unproven(_)));
    }

    const BOTH: Records = Records {
        backing: true,
        device: true,
        mount_root: true,
    };
}
