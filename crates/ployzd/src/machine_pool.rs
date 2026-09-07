//! Imported Machine Pool eligibility.

use std::num::NonZeroU64;

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
