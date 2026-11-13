//! What the Cloud tests share: a fake Cloud, a stored sign-in and a token credential.

use super::credential::{Credential, TokenFrom};
use crate::cloud_login::{CredentialStore, Secret};

pub(crate) use crate::cloud_login::tests::fake_cloud;

/// A device signed in to `cloud` as Organization `acme`.
pub(crate) fn signed_in_store(cloud: &str) -> (tempfile::TempDir, CredentialStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::beside(&dir.path().join("config.yaml"));
    let stored = serde_json::json!({
        "state": "signed_in",
        "cloud": cloud,
        "token": "session-token",
        "account": { "id": "u1", "email": "dev@example.test", "name": "Dev" },
        "organization": { "id": "o1", "slug": "acme" },
    });
    std::fs::write(dir.path().join("cloud.json"), stored.to_string()).unwrap();
    (dir, store)
}

pub(crate) fn token(cloud: &str) -> Credential {
    Credential::Token {
        cloud: cloud.to_owned(),
        token: Secret::new("ployz_secret".to_owned()),
        from: TokenFrom::Env,
    }
}
