//! Organization Tokens and signed-in devices: listing, making and revoking them.

use ployz_core::MachineId;
use reqwest::Method;
use serde::{Deserialize, Serialize};

use super::{calls::call, credential::Credential};
use crate::cloud_login::LoginError;

/// An Organization Token as listed: never its secret.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: String,
    pub(crate) expires_at: String,
    pub(crate) expired: bool,
    pub(crate) current: bool,
    #[serde(default)]
    pub(crate) kind: TokenKind,
    pub(crate) scope: String,
    /// Whose token it is. A Cloud older than 0.3.2 names no owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner: Option<TokenOwner>,
}

/// The member a listed token acts as; `you` when that is the caller.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenOwner {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) email: String,
    pub(crate) you: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "preset", rename_all = "snake_case")]
pub(crate) enum TokenPreset {
    Read,
    Deploy { environment: String },
    Full,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TokenKind {
    #[default]
    Cli,
    Agent,
}

impl TokenKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Agent => "agent",
        }
    }
}

/// A signed-in device of the caller's.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct DeviceEntry {
    pub(crate) id: String,
    pub(crate) created_at: String,
    pub(crate) expires_at: String,
    pub(crate) current: bool,
}

/// A revoked token or device whose Clear some Servers haven't confirmed yet.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct RevokingEntry {
    pub(crate) id: String,
    pub(crate) kind: CredentialKind,
    /// Servers still holding its Management Client.
    pub(crate) unconfirmed: Vec<MachineId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CredentialKind {
    /// An Organization Token (`PLOYZ_TOKEN`).
    Token,
    Device,
}

/// The Organization's tokens, the caller's signed-in devices, and revocations still to confirm.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub(crate) tokens: Vec<TokenEntry>,
    pub(crate) devices: Vec<DeviceEntry>,
    pub(crate) revoking: Vec<RevokingEntry>,
}

/// Which Servers (by Machine ID) confirmed clearing a revoked credential's Management Client.
/// Cloud refuses the credential at once either way; `unconfirmed` Servers still let its key in.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct ServerClears {
    pub(crate) confirmed: Vec<MachineId>,
    pub(crate) unconfirmed: Vec<MachineId>,
}

/// A token just made: the one reply that carries its secret.
#[derive(Serialize, Deserialize)]
pub(crate) struct NewToken {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) organization: String,
    pub(crate) expires_at: String,
    pub(crate) secret: String,
    #[serde(default)]
    pub(crate) kind: TokenKind,
    pub(crate) scope: String,
}

/// A revoked token or signed-out device.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Removed {
    pub(crate) id: String,
    pub(crate) kind: CredentialKind,
}

/// The Organization's tokens and the caller's signed-in devices.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn credentials(credential: &Credential) -> Result<Credentials, LoginError> {
    call(credential, Method::GET, "tokens", None).await
}

/// Make an Organization Token acting as the caller in the caller's Organization.
///
/// # Errors
///
/// Returns a Cloud failure; a rejected name or expiry is HTTP 422.
pub(crate) async fn new_token(
    credential: &Credential,
    name: &str,
    kind: TokenKind,
    preset: &TokenPreset,
    expires_in_days: u16,
) -> Result<NewToken, LoginError> {
    #[derive(Serialize)]
    struct Body<'a> {
        name: &'a str,
        kind: TokenKind,
        expires_in_days: u16,
        #[serde(flatten)]
        preset: &'a TokenPreset,
    }
    #[derive(Deserialize)]
    struct Reply {
        token: NewToken,
    }
    let body = serde_json::to_value(Body {
        name,
        kind,
        expires_in_days,
        preset,
    })
    .expect("a token request serializes");
    let reply: Reply = call(credential, Method::POST, "tokens", Some(&body)).await?;
    Ok(reply.token)
}

/// The ID `target` names in `ployz token rm`. Cloud revokes only by ID, a hyphenated
/// UUID, and refuses every token caller, so such a UUID or a token caller's target goes
/// as given: an ID Cloud no longer lists, such as a fully revoked one, never resolves to
/// a token named like it. Otherwise it is the one token named exactly `target`, else as
/// given.
///
/// # Errors
///
/// Returns [`LoginError::AmbiguousCredential`] when several tokens have that name, or a
/// Cloud failure listing them.
pub(crate) async fn credential_id(
    credential: &Credential,
    target: &str,
) -> Result<String, LoginError> {
    if matches!(credential, Credential::Token { .. }) || cloud_id(target) {
        return Ok(target.to_owned());
    }
    named_credential(&credentials(credential).await?, target)
}

/// Cloud's ID shape: the 36-character hyphenated UUID. The uuid crate parses no other
/// form at that length, so a 32-hex, braced or URN name still resolves as a name.
fn cloud_id(target: &str) -> bool {
    target.len() == 36 && uuid::Uuid::try_parse(target).is_ok()
}

fn named_credential(listed: &Credentials, target: &str) -> Result<String, LoginError> {
    let mut named: Vec<String> = listed
        .tokens
        .iter()
        .filter(|token| token.name == target)
        .map(|token| token.id.clone())
        .collect();
    match named.len() {
        0 => Ok(target.to_owned()),
        1 => Ok(named.remove(0)),
        _ => Err(LoginError::AmbiguousCredential {
            name: target.to_owned(),
            ids: named,
        }),
    }
}

/// Revoke a token of the Organization, or sign out one of the caller's devices, then clear
/// its Management Client on each Server. Rerun on a revoked id, it retries unconfirmed Servers.
///
/// # Errors
///
/// Returns [`LoginError::UnknownCredential`] when there is no such id, or a Cloud failure.
pub(crate) async fn remove_token(
    credential: &Credential,
    id: &str,
) -> Result<(Removed, ServerClears), LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        removed: Removed,
        servers: ServerClears,
    }
    match call::<Reply>(credential, Method::DELETE, &format!("tokens/{id}"), None).await {
        Ok(reply) => Ok((reply.removed, reply.servers)),
        Err(LoginError::Status { status: 404, .. }) => {
            Err(LoginError::UnknownCredential(id.to_owned()))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::{
        credential, organizations,
        testing::{fake_cloud, signed_in_store, token},
    };

    #[tokio::test]
    async fn token_rm_finds_a_token_by_its_exact_name_and_refuses_a_shared_one() {
        const CI: &str = "00000000-0000-4000-8000-00000000000a";
        const HEX: &str = "00000000-0000-4000-8000-00000000000f";
        let entry = |id: &str, name: &str| {
            serde_json::json!({
                "id": id, "name": name, "created_at": "2026-11-01", "expires_at": "2027-01-30",
                "expired": false, "current": false, "kind": "cli", "scope": "full",
            })
        };
        let listed = serde_json::json!({
            "tokens": [
                entry(CI, "ci"),
                entry("00000000-0000-4000-8000-00000000000b", "shared"),
                entry("00000000-0000-4000-8000-00000000000c", "shared"),
                // Names in UUID forms Cloud never uses as an ID.
                entry(HEX, "0000000000004000800000000000000a"),
                entry("00000000-0000-4000-8000-000000000010", "{00000000-0000-4000-8000-00000000000a}"),
            ],
            "devices": [],
            "revoking": [],
        });
        let cloud = fake_cloud(move |route, _| match route {
            "GET /api/cli/tokens" => (200, listed.clone()),
            _ => (404, serde_json::json!({ "code": "NOT_FOUND" })),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();

        assert_eq!(credential_id(&device, "ci").await.unwrap(), CI);
        assert_eq!(
            credential_id(&device, "0000000000004000800000000000000a")
                .await
                .unwrap(),
            HEX
        );
        assert_eq!(
            credential_id(&device, "{00000000-0000-4000-8000-00000000000a}")
                .await
                .unwrap(),
            "00000000-0000-4000-8000-000000000010"
        );
        assert_eq!(
            credential_id(&device, "Ci").await.unwrap(),
            "Ci",
            "names match exactly"
        );
        assert_eq!(
            credential_id(&device, "d1").await.unwrap(),
            "d1",
            "an unknown target goes as given"
        );
        let error = credential_id(&device, "shared").await.unwrap_err();
        assert!(
            matches!(&error, LoginError::AmbiguousCredential { name, ids } if name == "shared" && ids.len() == 2),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("00000000-0000-4000-8000-00000000000b"),
            "{error}"
        );
        assert_eq!(
            crate::failure::login_code(&error),
            ployz_core::RpcErrorCode::Ambiguous
        );
    }

    #[tokio::test]
    async fn token_rm_sends_an_id_or_a_token_caller_s_target_without_listing() {
        // A fully revoked ID that a live token is named like, as Cloud no longer lists it.
        const GONE: &str = "00000000-0000-4000-8000-0000000000aa";
        let cloud = fake_cloud(|route, _| match route {
            "GET /api/cli/tokens" => (500, serde_json::json!({ "code": "INTERNAL" })),
            _ => (404, serde_json::json!({ "code": "NOT_FOUND" })),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();

        assert_eq!(credential_id(&device, GONE).await.unwrap(), GONE);
        assert_eq!(credential_id(&token(&cloud), "ci").await.unwrap(), "ci");
        assert!(
            credential_id(&device, "ci").await.is_err(),
            "a name needs the list"
        );
        assert!(
            credential_id(&device, "0000000000004000800000000000000a")
                .await
                .is_err(),
            "a 32-hex target is a name: Cloud's IDs are hyphenated"
        );
    }

    #[tokio::test]
    async fn a_refused_credential_names_its_fix() {
        let cloud = fake_cloud(|route, _| match route {
            "GET /api/cli/tokens" => (401, serde_json::json!({ "code": "UNAUTHORIZED" })),
            _ => (403, serde_json::json!({ "code": "FORBIDDEN" })),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        assert!(matches!(credentials(&device).await, Err(LoginError::Ended)));
        assert!(matches!(
            credentials(&token(&cloud)).await,
            Err(LoginError::TokenRefused)
        ));
        let error = organizations(&device).await.unwrap_err();
        assert!(
            matches!(&error, LoginError::NotMember(slug) if slug == "acme"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn cloud_answers_become_their_own_errors() {
        let cloud = fake_cloud(|_, _| (404, serde_json::json!({ "code": "NOT_FOUND" })));
        let credential = token(&cloud);
        let error = remove_token(&credential, "t1").await.unwrap_err();
        assert!(matches!(error, LoginError::UnknownCredential(_)), "{error}");
        // A Cloud without the CLI surface (or with it switched off).
        let error = credentials(&credential).await.unwrap_err();
        assert!(matches!(error, LoginError::Unsupported(_)), "{error}");
    }

    #[tokio::test]
    async fn a_new_token_carries_its_secret_once() {
        let cloud = fake_cloud(|route, body| match route {
            "POST /api/cli/tokens" => {
                let body: serde_json::Value = serde_json::from_str(body).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({
                        "name": "ci", "kind": "agent", "expires_in_days": 30,
                        "preset": "deploy", "environment": "staging",
                    })
                );
                (
                    200,
                    serde_json::json!({ "token": {
                        "id": "t1", "name": "ci", "organization": "acme",
                        "expires_at": "2026-10-29T00:00:00.000Z", "secret": "ployz_new", "kind": "agent",
                        "scope": "Deploy staging",
                    } }),
                )
            }
            other => panic!("unexpected {other}"),
        });
        let deploy = TokenPreset::Deploy {
            environment: "staging".into(),
        };
        let made = new_token(&token(&cloud), "ci", TokenKind::Agent, &deploy, 30)
            .await
            .unwrap();
        assert_eq!(made.secret, "ployz_new");
        assert_eq!(made.kind, TokenKind::Agent);
        assert_eq!(made.scope, "Deploy staging");
    }
}
