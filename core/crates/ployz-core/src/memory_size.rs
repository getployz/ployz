//! A `memLimit` in GB as people read it.

pub const MIB: f64 = 1_048_576.0;
pub const GIB: f64 = 1_073_741_824.0;

/// A `memLimit` in GB as people read it, in the unit it was likely given: `192 MiB`,
/// `1 GiB`, `0.5 GB`.
#[must_use]
pub fn memory_size(gb: f64) -> String {
    let (amount, unit) = unit((gb * 1e9).round());
    format!("{amount} {unit}")
}

/// Whole `bytes` in GiB or MiB when they divide evenly, else in GB.
#[must_use]
pub fn unit(bytes: f64) -> (f64, &'static str) {
    if bytes % GIB == 0.0 {
        (bytes / GIB, "GiB")
    } else if bytes % MIB == 0.0 {
        (bytes / MIB, "MiB")
    } else {
        (bytes / 1e9, "GB")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memory_limit_reads_in_the_unit_it_was_given() {
        assert_eq!(memory_size(0.201_326_592), "192 MiB");
        assert_eq!(memory_size(1.073_741_824), "1 GiB");
        assert_eq!(memory_size(0.5), "0.5 GB");
        assert_eq!(memory_size(2.0), "2 GB");
        assert_eq!(memory_size(0.001), "0.001 GB");
    }
}
