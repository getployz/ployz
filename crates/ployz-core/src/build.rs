//! Validated source and adapter contracts for bounded image builds.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::image::OciPlatform;

const MAX_GIT_URL_BYTES: usize = 2_048;
const MAX_CREDENTIAL_USERNAME_BYTES: usize = 256;
const MAX_CREDENTIAL_SECRET_BYTES: usize = 8_192;
const MAX_BUILD_PATH_BYTES: usize = 1_024;
const MAX_DOCKERFILE_STAGE_BYTES: usize = 128;
const MAX_BUILD_CACHE_SCOPE_BYTES: usize = 256;

/// Longest build execution budget accepted by a machine.
pub const BUILD_MAX_EXECUTION_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// Time allowed for a build task to observe cancellation and stop.
pub const BUILD_TASK_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed for forced workspace cleanup after task shutdown.
pub const BUILD_FORCE_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest time a machine build request can legitimately take to answer.
pub const BUILD_MAX_MACHINE_RESPONSE_LIFETIME: Duration = BUILD_MAX_EXECUTION_TIMEOUT
    .saturating_add(BUILD_TASK_DRAIN_TIMEOUT)
    .saturating_add(BUILD_FORCE_CLEANUP_TIMEOUT);
/// Controller margin beyond the machine's maximum response lifetime.
pub const BUILD_CONTROL_RESPONSE_MARGIN: Duration = Duration::from_secs(5);
/// Outer machine endpoint margin beyond the machine's maximum response lifetime.
pub const BUILD_ENDPOINT_RESPONSE_MARGIN: Duration = Duration::from_secs(10);
/// Dynamic NATS response-authority margin beyond the machine endpoint lifetime.
pub const BUILD_RESPONSE_PERMISSION_MARGIN: Duration = Duration::from_secs(5);

/// Controller request budget for a build with the supplied execution budget.
#[must_use]
pub const fn build_control_request_timeout(execution_timeout: Duration) -> Duration {
    execution_timeout
        .saturating_add(BUILD_TASK_DRAIN_TIMEOUT)
        .saturating_add(BUILD_FORCE_CLEANUP_TIMEOUT)
        .saturating_add(BUILD_CONTROL_RESPONSE_MARGIN)
}

/// Maximum BuildStart handler lifetime.
pub const BUILD_START_ENDPOINT_TIMEOUT: Duration =
    BUILD_MAX_MACHINE_RESPONSE_LIFETIME.saturating_add(BUILD_ENDPOINT_RESPONSE_MARGIN);
/// Lifetime of one request-scoped NATS response grant.
pub const BUILD_RESPONSE_PERMISSION_EXPIRY: Duration =
    BUILD_START_ENDPOINT_TIMEOUT.saturating_add(BUILD_RESPONSE_PERMISSION_MARGIN);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GitSource {
    url: GitRepositoryUrl,
    commit: GitCommit,
    credential: GitBasicCredential,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    subdir: Option<BuildContextPath>,
}

impl GitSource {
    pub fn try_new(
        url: impl Into<String>,
        commit: impl Into<String>,
        username: impl Into<String>,
        secret: impl Into<String>,
        subdir: Option<impl Into<String>>,
    ) -> Result<Self, GitSourceError> {
        Ok(Self {
            url: GitRepositoryUrl::try_new(url)?,
            commit: GitCommit::try_new(commit)?,
            credential: GitBasicCredential::try_new(username, secret)?,
            subdir: subdir
                .map(|value| BuildContextPath::try_new(value.into()))
                .transpose()?,
        })
    }

    #[must_use]
    pub fn evidence(&self) -> GitSourceEvidence {
        GitSourceEvidence {
            url: self.url.clone(),
            commit: self.commit.clone(),
            subdir: self.subdir.clone(),
        }
    }

    #[must_use]
    pub const fn url(&self) -> &GitRepositoryUrl {
        &self.url
    }

    #[must_use]
    pub const fn commit(&self) -> &GitCommit {
        &self.commit
    }

    #[must_use]
    pub const fn subdir(&self) -> Option<&BuildContextPath> {
        self.subdir.as_ref()
    }

    #[must_use]
    pub fn credential(&self) -> &GitBasicCredential {
        &self.credential
    }
}

impl fmt::Debug for GitSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitSource")
            .field("url", &self.url)
            .field("commit", &self.commit)
            .field("credential", &self.credential)
            .field("subdir", &self.subdir)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct GitRepositoryUrl(String);

impl GitRepositoryUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        if value.len() > MAX_GIT_URL_BYTES || value.chars().any(char::is_control) {
            return Err(GitSourceError::InvalidUrl);
        }
        let parsed = Url::parse(&value).map_err(|_| GitSourceError::InvalidUrl)?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err(GitSourceError::InvalidUrl);
        }
        if !parsed.username().is_empty() || parsed.password().is_some() {
            return Err(GitSourceError::UrlContainsUserInfo);
        }
        if parsed.fragment().is_some() {
            return Err(GitSourceError::UrlContainsFragment);
        }
        if parsed.query().is_some() {
            return Err(GitSourceError::UrlContainsQuery);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for GitRepositoryUrl {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<GitRepositoryUrl> for String {
    fn from(value: GitRepositoryUrl) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct GitCommit(String);

impl GitCommit {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        if value.len() != 40 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(GitSourceError::InvalidCommit { value });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for GitCommit {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<GitCommit> for String {
    fn from(value: GitCommit) -> Self {
        value.0
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct GitCredentialSecret(String);

impl GitCredentialSecret {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GitSourceError::EmptyCredentialSecret);
        }
        if value.len() > MAX_CREDENTIAL_SECRET_BYTES || value.chars().any(char::is_control) {
            return Err(GitSourceError::InvalidCredentialSecret);
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for GitCredentialSecret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitCredentialSecret([redacted])")
    }
}
impl TryFrom<String> for GitCredentialSecret {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<GitCredentialSecret> for String {
    fn from(value: GitCredentialSecret) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GitBasicCredential {
    username: GitCredentialUsername,
    secret: GitCredentialSecret,
}

impl GitBasicCredential {
    pub fn try_new(
        username: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<Self, GitSourceError> {
        Ok(Self {
            username: GitCredentialUsername::try_new(username)?,
            secret: GitCredentialSecret::try_new(secret)?,
        })
    }
    #[must_use]
    pub fn username(&self) -> &GitCredentialUsername {
        &self.username
    }
    #[must_use]
    pub fn secret(&self) -> &GitCredentialSecret {
        &self.secret
    }

    #[must_use]
    pub fn redact_secret_in(&self, message: impl Into<String>) -> String {
        message.into().replace(self.secret.secret(), "[redacted]")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct GitCredentialUsername(String);
impl GitCredentialUsername {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GitSourceError::EmptyCredentialUsername);
        }
        if value.len() > MAX_CREDENTIAL_USERNAME_BYTES || value.chars().any(char::is_control) {
            return Err(GitSourceError::InvalidCredentialUsername);
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for GitCredentialUsername {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<GitCredentialUsername> for String {
    fn from(value: GitCredentialUsername) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct BuildContextPath(String);

impl BuildContextPath {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        let path = Path::new(&value);
        if value.is_empty()
            || value.len() > MAX_BUILD_PATH_BYTES
            || value.chars().any(char::is_control)
            || path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(GitSourceError::InvalidRelativePath { value });
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for BuildContextPath {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<BuildContextPath> for String {
    fn from(value: BuildContextPath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "adapter", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildAdapter {
    Dockerfile {
        dockerfile: BuildContextPath,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<DockerfileStageName>,
    },
    Railpack {
        cache_scope: BuildCacheScope,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct DockerfileStageName(String);
impl DockerfileStageName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GitSourceError::EmptyDockerfileStage);
        }
        if value.len() > MAX_DOCKERFILE_STAGE_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(GitSourceError::InvalidDockerfileStage);
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for DockerfileStageName {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<DockerfileStageName> for String {
    fn from(value: DockerfileStageName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct BuildCacheScope(String);
impl BuildCacheScope {
    pub fn try_new(value: impl Into<String>) -> Result<Self, GitSourceError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GitSourceError::EmptyRailpackCacheScope);
        }
        if value.len() > MAX_BUILD_CACHE_SCOPE_BYTES || value.chars().any(char::is_control) {
            return Err(GitSourceError::InvalidRailpackCacheScope);
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for BuildCacheScope {
    type Error = GitSourceError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<BuildCacheScope> for String {
    fn from(value: BuildCacheScope) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(try_from = "Vec<OciPlatform>", into = "Vec<OciPlatform>")]
pub struct BuildPlatforms(BTreeSet<OciPlatform>);
impl BuildPlatforms {
    pub fn try_new(
        platforms: impl IntoIterator<Item = OciPlatform>,
    ) -> Result<Self, BuildPlatformsError> {
        let platforms: Vec<_> = platforms.into_iter().collect();
        if platforms.is_empty() {
            return Err(BuildPlatformsError::Empty);
        }
        let mut seen = BTreeSet::new();
        for platform in &platforms {
            if !seen.insert(platform.clone()) {
                return Err(BuildPlatformsError::Duplicate {
                    platform: platform.clone(),
                });
            }
        }
        Ok(Self(seen))
    }
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &OciPlatform> {
        self.0.iter()
    }

    #[must_use]
    pub fn contains(&self, platform: &OciPlatform) -> bool {
        self.0.contains(platform)
    }
}
impl TryFrom<Vec<OciPlatform>> for BuildPlatforms {
    type Error = BuildPlatformsError;
    fn try_from(value: Vec<OciPlatform>) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<BuildPlatforms> for Vec<OciPlatform> {
    fn from(value: BuildPlatforms) -> Self {
        value.0.into_iter().collect()
    }
}

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
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(transparent)]
pub struct RailpackCacheKey(String);
impl RailpackCacheKey {
    const VERSION: &'static str = "ployz.railpack.cache.v1";
    #[must_use]
    pub fn derive(scope: &BuildCacheScope) -> Self {
        let mut hasher = Sha256::new();
        for value in [Self::VERSION, scope.as_str()] {
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        Self(format!("ployz-railpack-v1-{:x}", hasher.finalize()))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitSourceError {
    #[error("git source URL must be a bounded absolute HTTPS URL")]
    InvalidUrl,
    #[error("git source URL must not contain user information")]
    UrlContainsUserInfo,
    #[error("git source URL must not contain a fragment")]
    UrlContainsFragment,
    #[error("git source URL must not contain query parameters")]
    UrlContainsQuery,
    #[error("git commit must be exactly 40 hexadecimal characters: {value:?}")]
    InvalidCommit { value: String },
    #[error("git credential username must not be empty")]
    EmptyCredentialUsername,
    #[error("git credential username is invalid")]
    InvalidCredentialUsername,
    #[error("git credential secret must not be empty")]
    EmptyCredentialSecret,
    #[error("git credential secret is invalid")]
    InvalidCredentialSecret,
    #[error("build path must be a non-empty relative path without traversal: {value:?}")]
    InvalidRelativePath { value: String },
    #[error("Dockerfile stage must not be empty")]
    EmptyDockerfileStage,
    #[error("Dockerfile stage is invalid")]
    InvalidDockerfileStage,
    #[error("Railpack cache scope must not be empty")]
    EmptyRailpackCacheScope,
    #[error("Railpack cache scope is invalid")]
    InvalidRailpackCacheScope,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildPlatformsError {
    #[error("build requires at least one platform")]
    Empty,
    #[error("build platform {platform:?} is duplicated")]
    Duplicate { platform: OciPlatform },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> GitSource {
        GitSource::try_new(
            "https://github.com/getployz/example.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "super-secret",
            Some("apps/api"),
        )
        .expect("source")
    }

    #[test]
    fn git_source_rejects_unsafe_and_ambiguous_inputs() {
        assert!(
            GitSource::try_new(
                "http://example.com/repo",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://user@example.com/repo",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://example.com/repo#main",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://example.com/repo?token=secret",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://example.com/repo",
                "01234567",
                "git",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://example.com/repo",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "x",
                Some("../escape")
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                format!("https://example.com/{}", "a".repeat(MAX_GIT_URL_BYTES)),
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://example.com/repo",
                "0123456789abcdef0123456789abcdef01234567",
                "bad\nuser",
                "x",
                None::<String>
            )
            .is_err()
        );
        assert!(
            GitSource::try_new(
                "https://example.com/repo",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "bad\nsecret",
                None::<String>
            )
            .is_err()
        );
        assert!(DockerfileStageName::try_new("stage; rm -rf /").is_err());
        assert!(BuildCacheScope::try_new("unsafe\nscope").is_err());
    }

    #[test]
    fn invalid_url_error_never_echoes_possible_secret() {
        let error = GitRepositoryUrl::try_new("https://example.com/repo?token=do-not-echo")
            .expect_err("query rejected");
        assert!(!error.to_string().contains("do-not-echo"));
    }

    #[test]
    fn source_evidence_and_debug_never_expose_secret() {
        let source = source();
        assert!(!format!("{source:?}").contains("super-secret"));
        assert_eq!(
            source.credential().redact_secret_in("git: super-secret"),
            "git: [redacted]"
        );
        let evidence = serde_json::to_value(source.evidence()).expect("evidence");
        assert!(!evidence.to_string().contains("super-secret"));
        assert_eq!(
            evidence,
            serde_json::json!({
                "url": "https://github.com/getployz/example.git",
                "commit": "0123456789abcdef0123456789abcdef01234567",
                "subdir": "apps/api",
            })
        );
    }

    #[test]
    fn adapter_is_closed_and_platforms_are_nonempty_unique() {
        assert!(
            serde_json::from_str::<BuildAdapter>(r#"{"adapter":"custom","command":"no"}"#).is_err()
        );
        let linux = OciPlatform::try_new("linux", "amd64").expect("platform");
        assert_eq!(BuildPlatforms::try_new([]), Err(BuildPlatformsError::Empty));
        assert!(matches!(
            BuildPlatforms::try_new([linux.clone(), linux]),
            Err(BuildPlatformsError::Duplicate { .. })
        ));
    }

    #[test]
    fn railpack_cache_key_has_stable_versioned_known_value() {
        let scope = BuildCacheScope::try_new("scope_01J0Y1J7YK7M7SXW3NQW78J4D2").expect("scope");
        assert_eq!(
            RailpackCacheKey::derive(&scope).as_str(),
            "ployz-railpack-v1-069ae0f41f4be9a1870d5e21db7b669973c6aec0e34cd583359347f40f685ffd"
        );
    }

    #[test]
    fn build_platforms_normalize_to_semantic_set_order() {
        let arm64 = OciPlatform::try_new("linux", "arm64").expect("platform");
        let amd64 = OciPlatform::try_new("linux", "amd64").expect("platform");
        let platforms = BuildPlatforms::try_new([arm64, amd64]).expect("platforms");
        let architectures: Vec<_> = platforms.iter().map(OciPlatform::architecture).collect();
        assert_eq!(architectures, vec!["amd64", "arm64"]);
    }

    #[test]
    fn build_response_budgets_cover_both_cleanup_windows_in_order() {
        assert_eq!(
            BUILD_MAX_MACHINE_RESPONSE_LIFETIME,
            BUILD_MAX_EXECUTION_TIMEOUT + BUILD_TASK_DRAIN_TIMEOUT + BUILD_FORCE_CLEANUP_TIMEOUT
        );
        assert!(
            build_control_request_timeout(BUILD_MAX_EXECUTION_TIMEOUT)
                > BUILD_MAX_MACHINE_RESPONSE_LIFETIME
        );
        assert!(BUILD_START_ENDPOINT_TIMEOUT > BUILD_MAX_MACHINE_RESPONSE_LIFETIME);
        assert!(BUILD_RESPONSE_PERMISSION_EXPIRY > BUILD_START_ENDPOINT_TIMEOUT);
    }
}
