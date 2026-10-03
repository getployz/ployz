//! The Machine's Linux distribution, from os-release.

use std::{fs, path::Path};

use super::{Error, refuse};

const STAGE: &str = "identify Linux distribution";

/// The os-release fields that pick an install route and name the OS in a refusal.
pub(super) struct OsRelease {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) version_id: Option<String>,
}

impl OsRelease {
    /// # Errors
    ///
    /// Fails when `path` can't be read or names no distribution `ID`.
    pub(super) fn read(path: &Path) -> Result<Self, Error> {
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
        Ok(Self {
            // Debian's NAME is "Debian GNU/Linux"; messages say "Debian 13".
            name: field("NAME").map_or_else(
                || id.clone(),
                |name| name.trim_end_matches(" GNU/Linux").to_owned(),
            ),
            version_id: field("VERSION_ID"),
            id,
        })
    }

    pub(super) fn is_amazon_linux(&self) -> bool {
        self.id == "amzn"
    }

    /// `NAME VERSION_ID`, e.g. "Amazon Linux 2023"; rolling releases have no version.
    pub(super) fn display(&self) -> String {
        match &self.version_id {
            Some(version) => format!("{} {version}", self.name),
            None => self.name.clone(),
        }
    }
}
