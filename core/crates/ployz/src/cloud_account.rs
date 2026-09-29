//! Acting in Cloud from the CLI: Organizations, Organization Tokens and signed-in
//! devices, and billing, through Cloud's `/api/cli` surface.
//!
//! Every call carries one credential: `PLOYZ_TOKEN` when set, else this device's
//! approved sign-in. Either acts in exactly one Organization.

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::cloud_enroll::cloud_origin;
use crate::cloud_login::{
    self, CredentialStore, DEFAULT_CLOUD, LoginError, Organization, Secret, SignedIn,
};

/// What a Cloud call acts with.
pub(crate) enum Credential {
    /// This device's approved sign-in; its Organization can be switched.
    Device(SignedIn),
    /// An Organization Token from `PLOYZ_TOKEN`, bound to its Organization.
    Token { cloud: String, token: Secret },
}

impl Credential {
    fn cloud(&self) -> &str {
        match self {
            Self::Device(signed_in) => &signed_in.cloud,
            Self::Token { cloud, .. } => cloud,
        }
    }

    fn bearer(&self) -> &str {
        match self {
            Self::Device(signed_in) => signed_in.token().expose(),
            Self::Token { token, .. } => token.expose(),
        }
    }
}

/// `PLOYZ_TOKEN` (at `cloud`, else the stored sign-in's Cloud, else the default)
/// wins over the stored sign-in. Never waits for a browser.
///
/// # Errors
///
/// Returns the stored sign-in's [`LoginError`] when there is no token.
pub(crate) async fn credential(
    store: &CredentialStore,
    token: Option<String>,
    cloud: Option<String>,
) -> Result<Credential, LoginError> {
    let Some(token) = token.filter(|token| !token.is_empty()) else {
        return cloud_login::signed_in(store).await.map(Credential::Device);
    };
    let cloud = match cloud {
        Some(cloud) => cloud,
        None => store.cloud()?.unwrap_or_else(|| DEFAULT_CLOUD.to_owned()),
    };
    Ok(Credential::Token {
        cloud: cloud_origin(&cloud),
        token: Secret::new(token),
    })
}

/// One Organization the credential may act in.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OrganizationEntry {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) current: bool,
}

/// An Organization Token as listed: never its secret.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct TokenEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: String,
    pub(crate) expires_at: String,
    pub(crate) expired: bool,
    pub(crate) current: bool,
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
    pub(crate) kind: String,
    /// Machine IDs still holding its Management Client.
    pub(crate) unconfirmed: Vec<String>,
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
    pub(crate) confirmed: Vec<String>,
    pub(crate) unconfirmed: Vec<String>,
}

/// This credential's own Management Capability on each Server Cloud could reach.
#[derive(Debug, Deserialize)]
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "live operations (#1241) dial with it")
)]
pub(crate) struct ServerAccess {
    pub(crate) connections: Vec<crate::context::Connection>,
    /// Machine IDs Cloud couldn't provision a holder on now.
    pub(crate) unreachable: Vec<String>,
}

/// A token just made: the one reply that carries its secret.
#[derive(Serialize, Deserialize)]
pub(crate) struct NewToken {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) organization: String,
    pub(crate) expires_at: String,
    pub(crate) secret: String,
}

/// A revoked token or signed-out device.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Removed {
    pub(crate) id: String,
    pub(crate) kind: String,
}

/// An Organization's Billing Plan and the capability it grants.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Billing {
    pub(crate) organization: String,
    pub(crate) self_hosted: bool,
    pub(crate) pro: bool,
    pub(crate) custom_domains: bool,
}

/// The Organizations this credential may act in.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn organizations(
    credential: &Credential,
) -> Result<Vec<OrganizationEntry>, LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        organizations: Vec<OrganizationEntry>,
    }
    let reply: Reply = call(credential, Method::GET, "organizations", None).await?;
    Ok(reply.organizations)
}

/// Switch this device's sign-in to another of the user's Organizations.
///
/// # Errors
///
/// Returns [`LoginError::TokenBound`] for a token, [`LoginError::UnknownOrganization`]
/// when the user isn't a member, or a Cloud or store failure.
pub(crate) async fn use_organization(
    store: &CredentialStore,
    credential: &Credential,
    slug: &str,
) -> Result<Organization, LoginError> {
    let Credential::Device(signed_in) = credential else {
        return Err(LoginError::TokenBound);
    };
    // better-auth's own endpoint moves the session's active Organization, checking membership.
    let response = send(
        credential,
        Method::POST,
        &format!("{}/api/auth/organization/set-active", signed_in.cloud),
        Some(&serde_json::json!({ "organizationSlug": slug })),
    )
    .await?;
    let status = response.status();
    if matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN) {
        return Err(LoginError::UnknownOrganization(slug.to_owned()));
    }
    let organization: Organization = read(credential, response).await?;
    store
        .set_organization(signed_in, organization)
        .map(|signed_in| signed_in.organization)
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
    expires_in_days: u16,
) -> Result<NewToken, LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        token: NewToken,
    }
    let body = serde_json::json!({ "name": name, "expires_in_days": expires_in_days });
    let reply: Reply = call(credential, Method::POST, "tokens", Some(&body)).await?;
    Ok(reply.token)
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

/// This credential's own Management Capability on each Server of its Organization; Cloud
/// provisions a missing one, including on Servers enrolled after login.
///
/// # Errors
///
/// Returns a Cloud failure.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "live operations (#1241) dial with it")
)]
pub(crate) async fn server_access(credential: &Credential) -> Result<ServerAccess, LoginError> {
    call(credential, Method::POST, "server-access", None).await
}

/// The Organization's Billing Plan and Custom Domain Capability.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn billing(credential: &Credential) -> Result<Billing, LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        billing: Billing,
    }
    let reply: Reply = call(credential, Method::GET, "billing", None).await?;
    Ok(reply.billing)
}

/// Where to buy Pro (`checkout`) or manage it (`portal`).
///
/// # Errors
///
/// Returns [`LoginError::NoBilling`] on a Self-hosted Cloud, [`LoginError::AlreadyPro`]
/// for a checkout the Organization doesn't need, or a Cloud failure.
pub(crate) async fn billing_url(
    credential: &Credential,
    page: BillingPage,
) -> Result<String, LoginError> {
    #[derive(Deserialize)]
    struct Reply {
        url: String,
    }
    let path = match page {
        BillingPage::Checkout => "billing/checkout",
        BillingPage::Portal => "billing/portal",
    };
    match call::<Reply>(credential, Method::POST, path, None).await {
        Ok(reply) => Ok(reply.url),
        Err(LoginError::Status { status: 404, .. }) => {
            Err(LoginError::NoBilling(credential.cloud().to_owned()))
        }
        Err(LoginError::Status { status: 409, .. }) => Err(LoginError::AlreadyPro),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum BillingPage {
    Checkout,
    Portal,
}

/// Call `/api/cli/<path>`. A 404 on a read means Cloud doesn't offer the CLI surface.
async fn call<T: DeserializeOwned>(
    credential: &Credential,
    method: Method,
    path: &str,
    body: Option<&serde_json::Value>,
) -> Result<T, LoginError> {
    let read_only = method == Method::GET;
    let url = format!("{}/api/cli/{path}", credential.cloud());
    let response = send(credential, method, &url, body).await?;
    if read_only && response.status() == StatusCode::NOT_FOUND {
        return Err(LoginError::Unsupported(credential.cloud().to_owned()));
    }
    read(credential, response).await
}

async fn send(
    credential: &Credential,
    method: Method,
    url: &str,
    body: Option<&serde_json::Value>,
) -> Result<reqwest::Response, LoginError> {
    let mut request = cloud_login::http()?
        .request(method, url)
        .bearer_auth(credential.bearer());
    if let Some(body) = body {
        request = request.json(body);
    }
    request
        .send()
        .await
        .map_err(|error| cloud_login::unreachable(credential.cloud(), error))
}

/// Decode a success; turn a refused credential into the error that names its fix.
async fn read<T: DeserializeOwned>(
    credential: &Credential,
    response: reqwest::Response,
) -> Result<T, LoginError> {
    let status = response.status();
    let body = response
        .bytes()
        .await
        .map_err(|error| cloud_login::unreachable(credential.cloud(), error))?;
    match (status, credential) {
        (status, _) if status.is_success() => cloud_login::decode(body.to_vec()),
        (StatusCode::UNAUTHORIZED, Credential::Device(_)) => Err(LoginError::Ended),
        (StatusCode::UNAUTHORIZED, Credential::Token { .. }) => Err(LoginError::TokenRefused),
        (StatusCode::FORBIDDEN, Credential::Device(signed_in)) => {
            Err(LoginError::NotMember(signed_in.organization.slug.clone()))
        }
        (status, _) => Err(LoginError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&body).into_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_login::tests::fake_cloud;

    /// A device signed in to `cloud` as Organization `acme`.
    fn signed_in_store(cloud: &str) -> (tempfile::TempDir, CredentialStore) {
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

    fn token(cloud: &str) -> Credential {
        Credential::Token {
            cloud: cloud.to_owned(),
            token: Secret::new("ployz_secret".to_owned()),
        }
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

    #[tokio::test]
    async fn a_refused_credential_names_its_fix() {
        let cloud = fake_cloud(|route, _| match route {
            "GET /api/cli/billing" => (401, serde_json::json!({ "code": "UNAUTHORIZED" })),
            _ => (403, serde_json::json!({ "code": "FORBIDDEN" })),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        assert!(matches!(billing(&device).await, Err(LoginError::Ended)));
        assert!(matches!(
            billing(&token(&cloud)).await,
            Err(LoginError::TokenRefused)
        ));
        let error = organizations(&device).await.unwrap_err();
        assert!(
            matches!(&error, LoginError::NotMember(slug) if slug == "acme"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn org_use_moves_the_device_among_its_own_organizations() {
        let cloud = fake_cloud(|route, body| match route {
            "POST /api/auth/organization/set-active" if body.contains("\"beta\"") => (
                200,
                serde_json::json!({ "id": "o2", "slug": "beta", "name": "Beta", "members": [] }),
            ),
            _ => (403, serde_json::json!({ "message": "not a member" })),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        let error = use_organization(&store, &device, "gamma")
            .await
            .unwrap_err();
        assert!(
            matches!(error, LoginError::UnknownOrganization(_)),
            "{error}"
        );
        let error = use_organization(&store, &token(&cloud), "beta")
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::TokenBound), "{error}");

        let organization = use_organization(&store, &device, "beta").await.unwrap();
        assert_eq!(organization.slug, "beta");
        let stored = cloud_login::signed_in(&store).await.unwrap();
        assert_eq!(stored.organization.id, "o2");
    }

    #[tokio::test]
    async fn cloud_answers_become_their_own_errors() {
        let cloud = fake_cloud(|route, _| match route {
            "POST /api/cli/billing/checkout" => (409, serde_json::json!({ "code": "CONFLICT" })),
            _ => (404, serde_json::json!({ "code": "NOT_FOUND" })),
        });
        let credential = token(&cloud);
        let error = remove_token(&credential, "t1").await.unwrap_err();
        assert!(matches!(error, LoginError::UnknownCredential(_)), "{error}");
        let error = billing_url(&credential, BillingPage::Checkout)
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::AlreadyPro), "{error}");
        let error = billing_url(&credential, BillingPage::Portal)
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::NoBilling(_)), "{error}");
        // A Cloud without the CLI surface (or with it switched off).
        let error = credentials(&credential).await.unwrap_err();
        assert!(matches!(error, LoginError::Unsupported(_)), "{error}");
    }

    #[tokio::test]
    async fn server_access_reads_dialable_connections() {
        let capability = format!("ployz1:{}", "A".repeat(86));
        let reply = serde_json::json!({
            "connections": [{ "machine_id": "a".repeat(32), "management": capability }],
            "unreachable": ["b".repeat(32)],
        });
        let cloud = fake_cloud(move |route, _| match route {
            "POST /api/cli/server-access" => (200, reply.clone()),
            other => panic!("unexpected {other}"),
        });
        let access = server_access(&token(&cloud)).await.unwrap();
        assert_eq!(access.connections.len(), 1);
        assert_eq!(access.unreachable, ["b".repeat(32)]);
        assert!(!format!("{access:?}").contains(&capability));
    }

    #[tokio::test]
    async fn a_new_token_carries_its_secret_once() {
        let cloud = fake_cloud(|route, body| match route {
            "POST /api/cli/tokens" => {
                let body: serde_json::Value = serde_json::from_str(body).unwrap();
                assert_eq!(
                    body,
                    serde_json::json!({ "name": "ci", "expires_in_days": 30 })
                );
                (
                    200,
                    serde_json::json!({ "token": {
                        "id": "t1", "name": "ci", "organization": "acme",
                        "expires_at": "2026-10-29T00:00:00.000Z", "secret": "ployz_new",
                    } }),
                )
            }
            other => panic!("unexpected {other}"),
        });
        let made = new_token(&token(&cloud), "ci", 30).await.unwrap();
        assert_eq!(made.secret, "ployz_new");
    }
}
