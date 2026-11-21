//! The first record of a `zfs send` stream, read before any byte of it leaves this Machine.
//!
//! A stream opens with one `dmu_replay_record_t` of type `DRR_BEGIN`: 312 bytes in the
//! sender's byte order, its BEGIN payload after it. `drr_toguid` names the snapshot the
//! stream builds, so checking it against the GUID read with the origin binds the origin to
//! these bytes. Only a substream qualifies: `-R`, `-p` and `-I` wrap theirs in a compound
//! BEGIN whose substreams this check never sees.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _};

/// `sizeof(dmu_replay_record_t)`.
pub const RECORD_BYTES: usize = 312;
/// How long the sender waits for `zfs send` to write its BEGIN record.
pub const READ_BOUND: Duration = Duration::from_secs(30);

const DRR_BEGIN: u32 = 0;
const DMU_BACKUP_MAGIC: u64 = 0x2_F5BA_CBAC;
const DMU_SUBSTREAM: u64 = 1;
const HEADER_TYPE_MASK: u64 = 0x3;

/// Why a stream's first record is not the substream BEGIN of the expected snapshot.
#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum BeginError {
    #[error("the send stream ended after {0} bytes, before its BEGIN record")]
    Short(usize),
    #[error("the send stream wrote no BEGIN record within {0:?}")]
    Slow(Duration),
    #[error("the send stream could not be read: {0}")]
    Read(String),
    #[error("the send stream does not open with a BEGIN record")]
    NotBegin,
    #[error("the send stream's BEGIN record has no ZFS magic")]
    NoMagic,
    #[error("the send stream is compound, not one snapshot's substream")]
    Compound,
    #[error("the send stream builds snapshot {found}, not {expected}")]
    OtherSnapshot { expected: u64, found: u64 },
}

/// Checks `record`, the first [`RECORD_BYTES`] of a stream, names `guid`.
///
/// # Errors
/// Returns a [`BeginError`] unless `record` is a substream BEGIN whose `drr_toguid` is
/// `guid`, in either byte order.
pub fn check(record: &[u8; RECORD_BYTES], guid: u64) -> Result<(), BeginError> {
    let u32_at = |at: usize, swap: bool| {
        let raw = u32::from_le_bytes(field(record, at));
        if swap { raw.swap_bytes() } else { raw }
    };
    let u64_at = |at: usize, swap: bool| {
        let raw = u64::from_le_bytes(field(record, at));
        if swap { raw.swap_bytes() } else { raw }
    };
    let swap = match u64_at(8, false) {
        DMU_BACKUP_MAGIC => false,
        magic if magic.swap_bytes() == DMU_BACKUP_MAGIC => true,
        _ => {
            return Err(if u32_at(0, false) == DRR_BEGIN {
                BeginError::NoMagic
            } else {
                BeginError::NotBegin
            });
        }
    };
    if u32_at(0, swap) != DRR_BEGIN {
        return Err(BeginError::NotBegin);
    }
    if u64_at(16, swap) & HEADER_TYPE_MASK != DMU_SUBSTREAM {
        return Err(BeginError::Compound);
    }
    let found = u64_at(40, swap);
    if found != guid {
        return Err(BeginError::OtherSnapshot {
            expected: guid,
            found,
        });
    }
    Ok(())
}

/// The `N` bytes at `at`, a fixed field offset inside the record.
fn field<const N: usize>(record: &[u8; RECORD_BYTES], at: usize) -> [u8; N] {
    record
        .get(at..at + N)
        .and_then(|bytes| bytes.try_into().ok())
        .expect("every field lies inside the record")
}

/// Reads the BEGIN record from `stream` within [`READ_BOUND`] and checks it names `guid`.
/// The record's bytes are returned, so the caller sends them first, unchanged.
///
/// # Errors
/// Returns a [`BeginError`] for a short, slow, unreadable or wrong record.
pub async fn read(
    stream: &mut (impl AsyncRead + Unpin),
    guid: u64,
    bound: Duration,
) -> Result<[u8; RECORD_BYTES], BeginError> {
    let mut record = [0; RECORD_BYTES];
    let mut filled = 0;
    let reading = async {
        while let Some(rest) = record.get_mut(filled..).filter(|rest| !rest.is_empty()) {
            match stream.read(rest).await {
                Ok(0) => return Err(BeginError::Short(filled)),
                Ok(read) => filled += read,
                Err(error) => return Err(BeginError::Read(error.to_string())),
            }
        }
        Ok(())
    };
    tokio::time::timeout(bound, reading)
        .await
        .map_err(|_| BeginError::Slow(bound))??;
    check(&record, guid)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// BEGIN records `zfs send` (OpenZFS 2.2.2, x86_64) wrote for one dataset: a full and an
    /// incremental stream of a snapshot of GUID [`TO`], and the compound records of `-I`
    /// and `-p`.
    const FULL: &[u8; RECORD_BYTES] = include_bytes!("fixtures/begin-full.bin");
    const INCREMENTAL: &[u8; RECORD_BYTES] = include_bytes!("fixtures/begin-incremental.bin");
    const INTERMEDIARY: &[u8; RECORD_BYTES] = include_bytes!("fixtures/begin-intermediary.bin");
    const PROPERTIES: &[u8; RECORD_BYTES] = include_bytes!("fixtures/begin-properties.bin");
    const TO: u64 = 10_550_143_828_866_502_470;

    #[test]
    fn a_real_substream_begin_passes_only_for_the_snapshot_it_builds() {
        for record in [FULL, INCREMENTAL] {
            assert_eq!(check(record, TO), Ok(()));
            assert_eq!(
                check(record, TO + 1),
                Err(BeginError::OtherSnapshot {
                    expected: TO + 1,
                    found: TO
                })
            );
        }
    }

    #[test]
    fn a_real_compound_begin_is_refused() {
        for record in [INTERMEDIARY, PROPERTIES] {
            assert_eq!(check(record, TO), Err(BeginError::Compound));
        }
    }

    #[tokio::test]
    async fn a_short_or_silent_stream_has_no_begin() {
        let mut short = FULL.get(..100).unwrap();
        assert_eq!(
            read(&mut short, TO, READ_BOUND).await,
            Err(BeginError::Short(100))
        );
        let (_writer, mut silent) = tokio::io::duplex(8);
        assert_eq!(
            read(&mut silent, TO, Duration::from_millis(20)).await,
            Err(BeginError::Slow(Duration::from_millis(20)))
        );
    }
}
