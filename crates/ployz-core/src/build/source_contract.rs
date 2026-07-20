use serde::{Deserialize, Serialize};

use super::{
    BuildContextPath, BuildSource, GitCommit, GitRepositoryUrl, GitSource, LocalSnapshotDigest,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GitSourceEvidence {
    pub url: GitRepositoryUrl,
    pub commit: GitCommit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<BuildContextPath>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VerifiedGitCommit {
    pub url: GitRepositoryUrl,
    pub commit: GitCommit,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<BuildContextPath>,
}

impl VerifiedGitCommit {
    #[must_use]
    pub fn from_source(source: &GitSource) -> Self {
        Self {
            url: source.url().clone(),
            commit: source.commit().clone(),
            subdir: source.subdir().cloned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildSourceEvidence {
    Git {
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        git: GitSourceEvidence,
    },
    LocalSnapshot {
        digest: LocalSnapshotDigest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional))]
        subdir: Option<BuildContextPath>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum VerifiedBuildSource {
    Git {
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        git: VerifiedGitCommit,
    },
    LocalSnapshot {
        digest: LocalSnapshotDigest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "typescript", ts(optional))]
        subdir: Option<BuildContextPath>,
    },
}

impl VerifiedBuildSource {
    #[must_use]
    pub fn from_source(source: &BuildSource) -> Self {
        match source {
            BuildSource::Git { git } => Self::Git {
                git: VerifiedGitCommit::from_source(git),
            },
            BuildSource::LocalSnapshot { digest, subdir } => Self::LocalSnapshot {
                digest: digest.clone(),
                subdir: subdir.clone(),
            },
        }
    }

    #[must_use]
    pub fn evidence(&self) -> BuildSourceEvidence {
        match self {
            Self::Git { git } => BuildSourceEvidence::Git {
                git: GitSourceEvidence {
                    url: git.url.clone(),
                    commit: git.commit.clone(),
                    subdir: git.subdir.clone(),
                },
            },
            Self::LocalSnapshot { digest, subdir } => BuildSourceEvidence::LocalSnapshot {
                digest: digest.clone(),
                subdir: subdir.clone(),
            },
        }
    }
}
