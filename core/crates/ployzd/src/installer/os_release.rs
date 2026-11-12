//! The Machine's Linux distribution, from os-release.

use std::{fs, path::Path};

use ployz_core::probe_document::OsRelease;

use super::{Error, refuse};

const STAGE: &str = "identify Linux distribution";

/// # Errors
///
/// Fails when `path` can't be read or names no distribution `ID`.
pub(super) fn read(path: &Path) -> Result<OsRelease, Error> {
    let value = fs::read_to_string(path).map_err(|source| Error::Io {
        stage: STAGE,
        source,
    })?;
    let field = |key: &str| {
        value
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|field| field.trim().trim_matches(['"', '\'']).to_owned())
            .filter(|field| !field.is_empty())
    };
    let Some(id) = field("ID") else {
        return Err(refuse(STAGE, "Could not identify the Linux distribution"));
    };
    Ok(OsRelease {
        // Debian's NAME is "Debian GNU/Linux"; messages say "Debian 13".
        name: field("NAME").map_or_else(
            || id.clone(),
            |name| name.trim_end_matches(" GNU/Linux").to_owned(),
        ),
        version_id: field("VERSION_ID"),
        id,
    })
}
