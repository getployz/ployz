//! Opening a Build's Sealed values on this Machine.

use std::path::Path;

use ployz_core::OpenedSealed;

use crate::sealed_env::{SealedRecord, Unseal as _};

/// The private file a capture writes its Sealed values to, still sealed.
pub(crate) const RECORD: &str = "sealed.json";

/// Open the record with this Machine's Sealing Key. The message names a
/// variable at most, never what it holds.
pub(crate) fn open(record: &[u8], key_dir: Option<&Path>) -> Result<OpenedSealed, String> {
    let record: SealedRecord = serde_json::from_slice(record)
        .map_err(|_| "the Build's sealed values are not a valid record".to_owned())?;
    record
        .open_for_build(key_dir)
        .map_err(|error| error.to_string())
}
