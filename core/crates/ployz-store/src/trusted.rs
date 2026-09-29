//! Evidence Cloud gathers outside the Store and passes in-process with a write. It is
//! never caller testimony: `read` and `write` over HTTPS carry none, only what Cloud found.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::domain::DomainEvidence;
use crate::git::AuthorizedRepository;

/// What Cloud checked for one write. Empty unless Cloud supplies it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ConfigTrusted")]
pub struct Trusted {
    /// Repositories the calling Organization may read, with the branches Cloud saw.
    #[serde(default)]
    pub repositories: Vec<AuthorizedRepository>,
    /// What Cloud observed of the Organization's public domains and traffic.
    #[serde(default)]
    pub domains: DomainEvidence,
    /// Who Cloud authenticated for this write: an admitted upload records them.
    #[serde(default)]
    pub uploader: Option<String>,
}
