//! Read a branch head from GitHub on this Machine.
//!
//! The Machine asks `api.github.com` over TLS by repository ID, so the answer is about the
//! approved repository whatever credential it is handed. The credential decides only
//! whether GitHub answers: one that cannot read the repository reads nothing, and that is
//! a refusal, never another head.

use std::{fmt, time::Duration};

use ployz_core::authority::CommitSha;
use reqwest::{StatusCode, header};
use serde::Deserialize;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

const API: &str = "https://api.github.com";
const TIMEOUT: Duration = Duration::from_secs(10);

/// A GitHub token that may read the repository, such as an installation token narrowed to
/// `contents: read` and that one repository. Its Debug never prints it.
pub struct GithubToken(Zeroizing<String>);

impl GithubToken {
    #[must_use]
    pub fn new(token: String) -> Self {
        Self(Zeroizing::new(token))
    }
}

impl fmt::Debug for GithubToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GithubToken(..)")
    }
}

/// Why this Machine could not read a branch head.
#[derive(Debug, Error)]
pub enum HeadError {
    /// GitHub answered that the branch does not exist for this credential: the repository
    /// is private and the credential cannot see it, the credential is bad, or the branch
    /// is gone.
    #[error("GitHub shows no branch {branch} this Machine can read")]
    Unreadable { branch: String },
    #[error("{branch} is not a branch name")]
    Branch { branch: String },
    #[error("GitHub did not answer for the branch head")]
    Unavailable(#[source] reqwest::Error),
    #[error("GitHub answered {status} for the branch head")]
    Status { status: StatusCode },
    #[error("GitHub's answer is not a branch head")]
    Malformed,
}

#[derive(Deserialize)]
struct GitRef {
    #[serde(rename = "ref")]
    name: String,
    object: GitObject,
}

#[derive(Deserialize)]
struct GitObject {
    #[serde(rename = "type")]
    kind: String,
    sha: CommitSha,
}

/// GitHub's REST API as this Machine reads it.
pub struct GithubApi {
    client: reqwest::Client,
    base: Url,
}

impl GithubApi {
    /// `api.github.com` over HTTPS, each request bounded by 10 seconds.
    ///
    /// # Errors
    /// Returns [`HeadError::Unavailable`] when the HTTPS client cannot be built.
    pub fn github() -> Result<Self, HeadError> {
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .user_agent("ployzd")
            .build()
            .map_err(HeadError::Unavailable)?;
        Ok(Self::with(
            client,
            Url::parse(API).expect("the API URL is a constant"),
        ))
    }

    fn with(client: reqwest::Client, base: Url) -> Self {
        Self { client, base }
    }

    /// The commit `branch` of repository `repository_id` points at now, read with
    /// `credential`, or with none for a public repository.
    ///
    /// # Errors
    /// Returns [`HeadError::Unreadable`] when GitHub answers 404 or 401,
    /// [`HeadError::Branch`] for a name git does not allow, [`HeadError::Unavailable`]
    /// when GitHub does not answer within 10 seconds, [`HeadError::Status`] for another
    /// status such as a rate limit, and [`HeadError::Malformed`] for an answer about
    /// another ref or not a commit.
    pub async fn branch_head(
        &self,
        repository_id: u64,
        branch: &str,
        credential: Option<&GithubToken>,
    ) -> Result<CommitSha, HeadError> {
        let url = self.ref_url(repository_id, branch)?;
        let mut request = self
            .client
            .get(url)
            .header(header::ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(GithubToken(token)) = credential {
            request = request.bearer_auth(token.as_str());
        }
        let response = request.send().await.map_err(HeadError::Unavailable)?;
        match response.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED => {
                return Err(HeadError::Unreadable {
                    branch: branch.to_owned(),
                });
            }
            status => return Err(HeadError::Status { status }),
        }
        let bytes = response.bytes().await.map_err(HeadError::Unavailable)?;
        let head: GitRef = serde_json::from_slice(&bytes).map_err(|_| HeadError::Malformed)?;
        if head.name != format!("refs/heads/{branch}") || head.object.kind != "commit" {
            return Err(HeadError::Malformed);
        }
        Ok(head.object.sha)
    }

    fn ref_url(&self, repository_id: u64, branch: &str) -> Result<Url, HeadError> {
        let segments: Vec<&str> = branch.split('/').collect();
        if segments
            .iter()
            .any(|segment| segment.is_empty() || *segment == "." || *segment == "..")
        {
            return Err(HeadError::Branch {
                branch: branch.to_owned(),
            });
        }
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("the API URL has a path")
            .pop_if_empty()
            .extend([
                "repositories",
                &repository_id.to_string(),
                "git",
                "ref",
                "heads",
            ])
            .extend(segments);
        Ok(url)
    }
}

#[cfg(test)]
#[path = "head_tests.rs"]
mod tests;
