//! Lowercase hexadecimal text of bytes: the one spelling digests, keys, signatures and
//! audiences share. Reading is lowercase only, so a value that was written here reads back
//! and nothing else does.

use std::fmt;

use crate::value::is_lower_hex;

/// `bytes` as lowercase hex, two digits each.
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    write(bytes, &mut text).expect("writing to a String cannot fail");
    text
}

/// Write `bytes` as lowercase hex, two digits each, into `out`.
///
/// # Errors
/// Fails only when `out` does.
pub fn write(bytes: &[u8], out: &mut impl fmt::Write) -> fmt::Result {
    bytes.iter().try_for_each(|byte| write!(out, "{byte:02x}"))
}

/// The `N` bytes `value` spells, or `None` unless it is exactly `2 * N` lowercase hex digits.
#[must_use]
pub fn decode<const N: usize>(value: &str) -> Option<[u8; N]> {
    if !is_lower_hex(value, N * 2) {
        return None;
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use std::fmt;

    #[test]
    fn bytes_encode_as_two_lowercase_digits_each() {
        assert_eq!(super::encode(&[]), "");
        assert_eq!(super::encode(&[0x00, 0x0f, 0xa0, 0xff]), "000fa0ff");
    }

    #[test]
    fn write_appends_to_any_writer_and_a_formatter() {
        let mut text = String::from("sha256:");
        super::write(&[0xde, 0xad], &mut text).unwrap();
        assert_eq!(text, "sha256:dead");

        struct Shown([u8; 2]);
        impl fmt::Display for Shown {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                super::write(&self.0, formatter)
            }
        }
        assert_eq!(Shown([0x01, 0xbe]).to_string(), "01be");
    }

    #[test]
    fn decode_reads_back_what_encode_wrote() {
        let bytes: [u8; 4] = [0x00, 0x7f, 0x80, 0xff];
        assert_eq!(super::decode::<4>(&super::encode(&bytes)), Some(bytes));
    }

    #[test]
    fn decode_reads_lowercase_of_exactly_the_length() {
        assert_eq!(super::decode::<2>("00ff"), Some([0x00, 0xff]));
        for refused in ["00FF", "00fF", "00f", "00ff0", "", "00fg", "0x00", "00 f"] {
            assert_eq!(super::decode::<2>(refused), None, "{refused:?}");
        }
        assert_eq!(super::decode::<0>(""), Some([]));
        assert_eq!(super::decode::<0>("00"), None);
    }
}
