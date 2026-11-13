//! The Organizations a credential may act in, switching between them, making and
//! removing them.

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use super::{
    calls::{approved_answer, call},
    credential::Credential,
    http::{self, Effect},
    reply::{StoreCallError, answer},
    token::ServerClears,
};
use crate::cloud_login::{CredentialStore, LoginError, Organization};

/// One Organization the credential may act in.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OrganizationEntry {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) current: bool,
}

/// The Organizations this credential may act in.
///
/// # Errors
///
/// Returns a Cloud failure.
pub(crate) async fn organizations(
    credential: &Credential,
) -> Result<Vec<OrganizationEntry>, LoginError> {
    Ok(listed(credential).await?.organizations)
}

/// `GET organizations`: the Organizations, and how Cloud hears the caller
/// (`cli` or `agent`; a Cloud that predates it says nothing).
#[derive(Deserialize)]
pub(super) struct Listed {
    pub(super) organizations: Vec<OrganizationEntry>,
    #[serde(default)]
    pub(super) via: Option<String>,
}

pub(super) async fn listed(credential: &Credential) -> Result<Listed, LoginError> {
    call(credential, Method::GET, "organizations", None).await
}

/// The Organization to switch to: a slug the user typed, or one Cloud listed, sent by ID.
pub(crate) enum Target<'a> {
    Slug(&'a str),
    Listed(&'a OrganizationEntry),
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
    target: Target<'_>,
) -> Result<Organization, LoginError> {
    let Credential::Device(signed_in) = credential else {
        return Err(LoginError::TokenBound);
    };
    let (body, slug) = match target {
        Target::Slug(slug) => (serde_json::json!({ "organizationSlug": slug }), slug),
        Target::Listed(listed) => (
            serde_json::json!({ "organizationId": listed.id }),
            listed.slug.as_str(),
        ),
    };
    // better-auth's own endpoint moves the session's active Organization, checking membership.
    let url = format!("{}/api/auth/organization/set-active", signed_in.cloud);
    let request = http::request(credential, Method::POST, &url, Some(&body))?;
    let response = http::dispatch(credential.cloud(), request, Effect::Read).await?;
    let status = response.status();
    if matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN) {
        return Err(LoginError::UnknownOrganization(slug.to_owned()));
    }
    let body = http::read_body(credential.cloud(), response, Effect::Read).await?;
    let organization: Organization = answer(credential, status, &body)?;
    store
        .set_organization(signed_in, organization)
        .map(|signed_in| signed_in.organization)
}

/// An Organization just made: Cloud picked its slug.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NewOrganization {
    pub(crate) id: String,
    pub(crate) slug: String,
    pub(crate) name: String,
}

/// Make an Organization the user owns and switch this device's sign-in to it. Cloud
/// slugifies `slug` (else `name`) and suffixes it when taken; the user's other sign-ins
/// stay where they are.
///
/// # Errors
///
/// Returns [`LoginError::TokenBound`] for a token, Cloud's refusal of the name or slug,
/// or a Cloud or store failure.
pub(crate) async fn new_organization(
    store: &CredentialStore,
    credential: &Credential,
    name: &str,
    slug: Option<&str>,
) -> Result<NewOrganization, LoginError> {
    let Credential::Device(signed_in) = credential else {
        return Err(LoginError::TokenBound);
    };
    #[derive(Deserialize)]
    struct Reply {
        organization: NewOrganization,
    }
    let body = match slug {
        Some(slug) => serde_json::json!({ "name": name, "slug": slug }),
        None => serde_json::json!({ "name": name }),
    };
    let reply: Reply = call(credential, Method::POST, "organizations", Some(&body)).await?;
    let made = reply.organization;
    store.set_organization(
        signed_in,
        Organization {
            id: made.id.clone(),
            slug: made.slug.clone(),
        },
    )?;
    Ok(made)
}

/// What `org rm` did: whether the Organization is gone, and which Servers confirmed
/// clearing Cloud's key and every device key. Until all do, it stays, disabled.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OrganizationRemoval {
    pub(crate) organization: String,
    pub(crate) removed: bool,
    pub(crate) servers: ServerClears,
}

/// Remove the credential's Organization, by its ID, once it has no Project.
/// Rerun while a Server hasn't confirmed, it retries the Clears.
///
/// # Errors
///
/// Returns Cloud's refusal (`conflict` while it has a Project, `invalid_argument`
/// for another Organization than the credential's, `approval_required` until a human
/// approves it), or a Cloud failure.
pub(crate) async fn remove_organization(
    credential: &Credential,
    id: &str,
    approval: Option<&str>,
) -> Result<OrganizationRemoval, StoreCallError> {
    let path = format!("organizations/{id}");
    approved_answer(
        credential,
        Method::DELETE,
        &path,
        &serde_json::json!({}),
        approval,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::{
        credential, refusal,
        testing::{signed_in_store, token},
    };
    use crate::{cloud_account::testing::fake_cloud, cloud_login};

    #[tokio::test]
    async fn org_use_moves_the_device_among_its_own_organizations() {
        let cloud = fake_cloud(|route, body| {
            let body: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
            let beta = body == serde_json::json!({ "organizationSlug": "beta" })
                || body == serde_json::json!({ "organizationId": "o2" });
            match route {
                "POST /api/auth/organization/set-active" if beta => (
                    200,
                    serde_json::json!({ "id": "o2", "slug": "beta", "name": "Beta", "members": [] }),
                ),
                _ => (403, serde_json::json!({ "message": "not a member" })),
            }
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        let error = use_organization(&store, &device, Target::Slug("gamma"))
            .await
            .unwrap_err();
        assert!(
            matches!(error, LoginError::UnknownOrganization(_)),
            "{error}"
        );
        let error = use_organization(&store, &token(&cloud), Target::Slug("beta"))
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::TokenBound), "{error}");

        let organization = use_organization(&store, &device, Target::Slug("beta"))
            .await
            .unwrap();
        assert_eq!(organization.slug, "beta");
        let stored = cloud_login::signed_in(&store).await.unwrap();
        assert_eq!(stored.organization.id, "o2");

        // A listed Organization goes by its ID, so a slug Cloud no longer knows still moves.
        let listed = OrganizationEntry {
            id: "o2".to_owned(),
            slug: "beta-renamed".to_owned(),
            name: "Beta".to_owned(),
            current: false,
        };
        let organization = use_organization(&store, &device, Target::Listed(&listed))
            .await
            .unwrap();
        assert_eq!(organization.id, "o2");
    }

    #[tokio::test]
    async fn org_new_moves_the_device_into_the_organization_cloud_made() {
        let cloud = fake_cloud(|route, body| match route {
            "POST /api/cli/organizations" if body.contains("\"slug\"") => (
                422,
                serde_json::json!({ "error": {
                    "code": "invalid_argument",
                    "message": "An Organization needs a name of 1 to 64 characters, and a slug of 1 to 64 letters, digits or hyphens.",
                    "details": null,
                }}),
            ),
            "POST /api/cli/organizations" => {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(body).unwrap(),
                    serde_json::json!({ "name": "Acme Corp" })
                );
                (
                    200,
                    serde_json::json!({ "organization": {
                        "id": "o2", "slug": "acme-corp-1a2b3c4d", "name": "Acme Corp",
                    }}),
                )
            }
            other => panic!("unexpected {other}"),
        });
        let (_dir, store) = signed_in_store(&cloud);
        let device = credential(&store, None, None).await.unwrap();
        let error = new_organization(&store, &token(&cloud), "Acme Corp", None)
            .await
            .unwrap_err();
        assert!(matches!(error, LoginError::TokenBound), "{error}");

        let error = new_organization(&store, &device, "Acme Corp", Some("!!!"))
            .await
            .unwrap_err();
        let LoginError::Status { status: 422, body } = &error else {
            panic!("expected the refusal, got {error}");
        };
        assert!(refusal(body.as_bytes()).is_some());
        assert_eq!(
            cloud_login::signed_in(&store)
                .await
                .unwrap()
                .organization
                .slug,
            "acme",
            "a refused name leaves the device where it was"
        );

        let made = new_organization(&store, &device, "Acme Corp", None)
            .await
            .unwrap();
        assert_eq!(made.slug, "acme-corp-1a2b3c4d");
        let stored = cloud_login::signed_in(&store).await.unwrap();
        assert_eq!(
            stored.organization,
            Organization {
                id: "o2".to_owned(),
                slug: "acme-corp-1a2b3c4d".to_owned(),
            }
        );
    }
}
