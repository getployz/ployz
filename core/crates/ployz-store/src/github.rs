//! Reads of a GitHub repository's files. Cloud answers them, not the Store: it
//! checks the Organization may read the repository and caps what comes back.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::RepositoryName;

/// List a repository's file paths.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubTreeQuery {
    pub repository: RepositoryName,
    /// Only files under this directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// Branch, tag or commit; the default branch when omitted.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub git_ref: Option<String>,
    /// Only paths matching this glob, like `*.rs` or `src/**`.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub glob: Option<String>,
}

/// A repository's file paths at one ref.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubTreeView {
    pub repository: RepositoryName,
    /// The ref read: the one asked for, or the default branch.
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub paths: Vec<String>,
    /// Whether GitHub or Cloud cut the listing short.
    pub truncated: bool,
}

/// Read one file of a repository.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GithubFileQuery {
    pub repository: RepositoryName,
    pub path: String,
    /// Branch, tag or commit; the default branch when omitted.
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub git_ref: Option<String>,
}

/// One file of a repository at one ref.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubFileView {
    pub repository: RepositoryName,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub path: String,
    /// Its size in bytes, as GitHub reports it.
    pub size: u64,
    #[serde(flatten)]
    #[ts(flatten)]
    pub body: GithubFileBody,
}

/// A file's text, or why Cloud withheld it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum GithubFileBody {
    Text {
        content: String,
    },
    /// A binary or oversized file.
    Withheld {
        note: String,
    },
}
