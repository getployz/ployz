//! Imported Machine Pool eligibility.

/// A writable imported Machine Pool with usable health.
#[derive(Debug, Eq, PartialEq)]
pub struct MachinePool(String);

impl MachinePool {
    /// The imported ZFS Pool name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.0
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

fn parse(line: &str) -> Result<Option<MachinePool>, Error> {
    let mut fields = line.split('\t');
    let invalid = || Error(format!("invalid ZFS Pool output: {line}"));
    let (Some(name), Some(health), Some(readonly), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Err(invalid());
    };
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
        (matches!(health, "ONLINE" | "DEGRADED") && readonly == "off")
            .then(|| MachinePool(name.to_owned())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_output_is_an_error() {
        assert_eq!(
            one_usable("broken\tONLINE").unwrap_err().to_string(),
            "invalid ZFS Pool output: broken\tONLINE"
        );
    }
}
