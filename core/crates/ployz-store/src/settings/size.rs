//! Memory sizes with a unit, as `memLimit` takes them: `1GiB`, `512MiB`, `0.5 GB`.

use ployz_core::config::MEM_LIMIT_MAX;

/// The units a size takes, and their bytes.
const UNITS: [(&str, f64); 6] = [
    ("MB", 1e6),
    ("GB", 1e9),
    ("TB", 1e12),
    ("MiB", MIB),
    ("GiB", GIB),
    ("TiB", 1_099_511_627_776.0),
];
const MIB: f64 = 1_048_576.0;
const GIB: f64 = 1_073_741_824.0;

/// The pattern a size matches; [`bytes`] parses exactly these.
pub(crate) const PATTERN: &str = "^[0-9]+(\\.[0-9]+)?\\s?(MB|GB|TB|MiB|GiB|TiB)$";

/// The whole bytes in `text`, a number and a unit, or `None` when it is not a size.
pub(crate) fn bytes(text: &str) -> Option<f64> {
    let (number, scale) = UNITS
        .iter()
        .find_map(|(unit, scale)| Some((text.strip_suffix(unit)?, *scale)))?;
    let number = number.strip_suffix(char::is_whitespace).unwrap_or(number);
    let (whole, fraction) = number.split_once('.').unwrap_or((number, "0"));
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    if !digits(whole) || !digits(fraction) {
        return None;
    }
    Some((number.parse::<f64>().ok()? * scale).round())
}

/// For a `memLimit` number too large to be GB but large enough to be bytes, those
/// bytes as a size: `("1 GiB", "1GiB")`.
pub(crate) fn looks_like_bytes(number: f64) -> Option<(String, String)> {
    if number <= MEM_LIMIT_MAX || number < MIB || !number.is_finite() {
        return None;
    }
    let bytes = number.round();
    let (amount, unit) = if bytes % GIB == 0.0 {
        (bytes / GIB, "GiB")
    } else if bytes % MIB == 0.0 {
        (bytes / MIB, "MiB")
    } else {
        (bytes / 1e9, "GB")
    };
    Some((format!("{amount} {unit}"), format!("{amount}{unit}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_a_number_and_a_unit() {
        assert_eq!(bytes("1GiB"), Some(1_073_741_824.0));
        assert_eq!(bytes("512MiB"), Some(536_870_912.0));
        assert_eq!(bytes("0.5 GB"), Some(500_000_000.0));
        assert_eq!(bytes("2TB"), Some(2e12));
        assert_eq!(bytes("1.5MiB"), Some(1_572_864.0));
        for text in [
            "1", "GiB", "1 gib", "1KiB", ".5GB", "1.GB", "-1GB", "1  GB", "1e3MB", " 1GB",
        ] {
            assert_eq!(bytes(text), None, "{text}");
        }
    }

    #[test]
    fn bytes_stored_as_gb_round_back_to_the_same_bytes() {
        for bytes in [1_073_741_824.0_f64, 536_870_912.0, 1_099_511_627_776.0, 1.0] {
            assert_eq!((bytes / 1e9 * 1e9).round(), bytes);
        }
    }

    #[test]
    fn only_a_number_too_large_for_gb_looks_like_bytes() {
        assert_eq!(
            looks_like_bytes(1_073_741_824.0),
            Some(("1 GiB".to_owned(), "1GiB".to_owned()))
        );
        assert_eq!(
            looks_like_bytes(536_870_912.0),
            Some(("512 MiB".to_owned(), "512MiB".to_owned()))
        );
        assert_eq!(
            looks_like_bytes(1_500_000_000.0),
            Some(("1.5 GB".to_owned(), "1.5GB".to_owned()))
        );
        assert_eq!(looks_like_bytes(1024.0), None);
        assert_eq!(looks_like_bytes(2000.0), None);
    }
}
