//! What a Cloud call acts with: `PLOYZ_TOKEN` when set, else this device's approved
//! sign-in; and the Cloud and Organization it acts in.

use super::{org::listed, token::TokenKind};
use crate::cloud_login::{self, CredentialStore, LoginError, Organization, Secret, SignedIn};
use crate::cloud_origin::CloudOrigin;

/// What a Cloud call acts with.
pub(crate) enum Credential {
    /// This device's approved sign-in; its Organization can be switched.
    Device(SignedIn),
    /// An Organization Token, bound to its Organization.
    Token {
        cloud: String,
        token: Secret,
        from: TokenFrom,
    },
}

/// Where an Organization Token came from, so a refusal names what to fix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenFrom {
    /// `PLOYZ_TOKEN`.
    Env,
    /// The agent token `ployz mcp login` stored.
    McpLogin,
}

impl Credential {
    pub(crate) fn cloud(&self) -> &str {
        match self {
            Self::Device(signed_in) => &signed_in.cloud,
            Self::Token { cloud, .. } => cloud,
        }
    }

    /// Cloud refused this token: the refusal names where it came from and its fix.
    pub(crate) fn token_refused(&self) -> LoginError {
        match self {
            Self::Token {
                from: TokenFrom::McpLogin,
                ..
            } => LoginError::AgentTokenRefused,
            Self::Device(_) | Self::Token { .. } => LoginError::TokenRefused,
        }
    }

    pub(crate) fn bearer(&self) -> &str {
        match self {
            Self::Device(signed_in) => signed_in.token().expose(),
            Self::Token { token, .. } => token.expose(),
        }
    }
}

/// `PLOYZ_TOKEN` (at [`named_cloud`], else the default) wins over the stored sign-in.
/// Never waits for a browser.
///
/// # Errors
///
/// Returns the stored sign-in's [`LoginError`] when there is no token.
pub(crate) async fn credential(
    store: &CredentialStore,
    token: Option<String>,
    cloud: Option<String>,
) -> Result<Credential, LoginError> {
    let Some(token) = pasted(token) else {
        return cloud_login::signed_in(store).await.map(Credential::Device);
    };
    if reqwest::header::HeaderValue::from_str(&token).is_err() {
        return Err(LoginError::TokenMalformed);
    }
    Ok(Credential::Token {
        cloud: named_cloud(store, true, cloud)?
            .unwrap_or_else(|| CloudOrigin::default_cloud().into_string()),
        token: Secret::new(token),
        from: TokenFrom::Env,
    })
}

/// `PLOYZ_TOKEN` as set, without the whitespace a paste into a CI secret or
/// `.env` file leaves around it; a token never holds any.
pub(crate) fn env_token() -> Option<String> {
    pasted(std::env::var(crate::env::TOKEN).ok())
}

fn pasted(token: Option<String>) -> Option<String> {
    token
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
}

/// The Cloud [`credential`] acts in, when anything names one: with a token, `cloud`
/// else the stored sign-in's; without, the stored sign-in's (a device acts only where
/// it signed in) else `cloud`. Whatever else talks to "the Cloud" reads this too, so
/// it never names a different one than the credential.
///
/// # Errors
///
/// Returns a credential store that cannot be read, when it is consulted.
pub(crate) fn named_cloud(
    store: &CredentialStore,
    token: bool,
    cloud: Option<String>,
) -> Result<Option<String>, LoginError> {
    let cloud = cloud.filter(|cloud| !cloud.trim().is_empty());
    let named = match cloud {
        Some(cloud) if token => Some(cloud),
        None if token => match store.cloud() {
            Err(LoginError::Corrupt { path, .. }) => {
                return Err(LoginError::TokenCloudUnreadable(path));
            }
            stored => stored?,
        },
        cloud => store.cloud()?.or(cloud),
    };
    Ok(named.map(|cloud| CloudOrigin::new(&cloud).into_string()))
}

/// [`credential`] from `PLOYZ_TOKEN` and `PLOYZ_CLOUD_URL`: the one way commands
/// pick what they act with.
///
/// # Errors
///
/// Returns the stored sign-in's [`LoginError`] when there is no token.
pub(crate) async fn from_env(store: &CredentialStore) -> Result<Credential, LoginError> {
    credential(store, env_token(), CloudOrigin::named_by_env()).await
}

/// The Organization `credential` acts in: the device's, or the token's own.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn acting_in(credential: &Credential) -> Result<Organization, LoginError> {
    Ok(acting_as(credential).await?.organization)
}

/// Who `credential` acts as.
pub(crate) struct Acting {
    pub(crate) organization: Organization,
    /// Cloud hears it as an agent: an agent token, which can't answer for a person.
    pub(crate) agent: bool,
}

/// [`acting_in`], and whether Cloud hears `credential` as an agent. A device is
/// never one; a token's kind is Cloud's to say.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn acting_as(credential: &Credential) -> Result<Acting, LoginError> {
    match credential {
        Credential::Device(signed_in) => Ok(Acting {
            organization: signed_in.organization.clone(),
            agent: false,
        }),
        Credential::Token { .. } => {
            let listed = listed(credential).await?;
            let organization = listed
                .organizations
                .into_iter()
                .find(|entry| entry.current)
                .map(|entry| Organization {
                    id: entry.id,
                    slug: entry.slug,
                })
                .ok_or_else(|| credential.token_refused())?;
            Ok(Acting {
                organization,
                agent: listed.via.as_deref() == Some(TokenKind::Agent.as_str()),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::testing::signed_in_store;

    #[test]
    fn a_device_acts_where_it_signed_in_and_a_token_where_the_cloud_url_says() {
        let (dir, store) = signed_in_store("http://signed-in.test");
        let other = || Some("http://other.test".to_owned());
        assert_eq!(
            named_cloud(&store, false, other()).unwrap().as_deref(),
            Some("http://signed-in.test")
        );
        assert_eq!(
            named_cloud(&store, true, other()).unwrap().as_deref(),
            Some("http://other.test")
        );
        let empty = CredentialStore::beside(&dir.path().join("elsewhere/config.yaml"));
        assert_eq!(
            named_cloud(&empty, false, other()).unwrap().as_deref(),
            Some("http://other.test"),
            "signed out, the Cloud URL still names where commands are read"
        );
    }

    #[tokio::test]
    async fn ployz_token_never_guesses_a_cloud_past_an_unreadable_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::beside(&dir.path().join("config.yaml"));
        std::fs::write(dir.path().join("cloud.json"), "{not json").unwrap();
        let token = || Some("ployz_secret".to_owned());
        let refused = credential(&store, token(), None).await.err();
        assert!(
            refused
                .as_ref()
                .is_some_and(|error| error.to_string().contains("PLOYZ_CLOUD_URL")),
            "{refused:?}"
        );
        let chosen = credential(&store, token(), Some("http://cloud.test".to_owned()))
            .await
            .unwrap();
        assert_eq!(chosen.cloud(), "http://cloud.test");
        assert!(named_cloud(&store, false, None).is_err());
    }

    #[tokio::test]
    async fn ployz_token_drops_the_whitespace_a_paste_leaves_around_it() {
        let (_dir, store) = signed_in_store("http://cloud.test");
        for pasted in [
            "ployz_secret\n",
            "ployz_secret\r\n",
            " ployz_secret ",
            "\tployz_secret",
        ] {
            let chosen = credential(&store, Some(pasted.to_owned()), None)
                .await
                .unwrap();
            assert_eq!(chosen.bearer(), "ployz_secret", "{pasted:?}");
        }
        let chosen = credential(&store, Some(" \n".to_owned()), None)
            .await
            .unwrap();
        assert!(matches!(chosen, Credential::Device(_)));
    }

    #[tokio::test]
    async fn ployz_token_no_header_can_carry_is_refused_without_printing_it() {
        let (_dir, store) = signed_in_store("http://cloud.test");
        let error = credential(&store, Some("ployz_sec\nret".to_owned()), None)
            .await
            .err()
            .unwrap();
        assert!(matches!(error, LoginError::TokenMalformed), "{error:?}");
        assert!(!error.to_string().contains("ployz_sec"), "{error}");
    }

    #[tokio::test]
    async fn ployz_token_wins_over_the_stored_sign_in() {
        let (_dir, store) = signed_in_store("http://cloud.test");
        let chosen = credential(&store, Some("ployz_secret".to_owned()), None)
            .await
            .unwrap();
        assert!(matches!(&chosen, Credential::Token { cloud, .. } if cloud == "http://cloud.test"));
        let chosen = credential(&store, Some(String::new()), None).await.unwrap();
        assert!(matches!(chosen, Credential::Device(_)));
    }
}
