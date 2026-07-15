//! NATS subject permissions and authorization-file rendering.

use ployz_core::ids::MachineId;
use ployz_core::nats_config::{
    CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsInternalAuthority,
    NatsUserPublicKey,
};
use ployz_core::security::NatsPrincipal;

use crate::server_config::quote_nats_string;
use crate::subjects::{
    CORE_RPC_QUERY_SCOPE, INGRESS_ENDPOINT_CHANGED, INGRESS_ENDPOINT_GET, INTENT_CHANGED,
    INTENT_GET, JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT, MACHINE_RPC_COMMAND_SCOPE,
    MACHINE_RPC_QUERY_SCOPE, OPERATION_PROGRESS_SCOPE, OPERATOR_INIT_FIRST_MACHINE_ACTIVATE,
    OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE, OPERATOR_MACHINE_IMAGE_QUERY_SCOPE,
    OPERATOR_RPC_COMMAND_SCOPE, OPERATOR_RPC_QUERY_SCOPE, PENDING_MACHINE_JOINS_CHANGED,
    RUNTIME_SNAPSHOT_STREAM, gateway_status, gateway_status_scope, machine_container_facts,
    machine_facts, machine_facts_scope, machine_service_command_scope, machine_service_query_scope,
};

const SYSTEM_EVENTS: &str = "$SYS.>";
const SYSTEM_REQUESTS: &str = "$SYS.REQ.>";
const NATS_SERVICE_DISCOVERY_SCOPE: &str = "$SRV.>";
const PRINCIPAL_MARKER_PREFIX: &str = "# ployz-principal: ";
const CREDENTIAL_NAME_MARKER_PREFIX: &str = "# ployz-credential-name: ";
const CREDENTIAL_ROLE_MARKER_PREFIX: &str = "# ployz-credential-role: ";

struct PendingAuthorization {
    principal: NatsPrincipal,
    credential_name: Option<CredentialName>,
    credential_role: Option<CredentialRole>,
}

/// Parses durable authority intent from a rendered NATS authorization include.
pub fn parse_authorized_users(
    rendered: &str,
) -> Result<Vec<NatsAuthorizationGrant>, AuthorizedUsersParseError> {
    let mut users = Vec::new();
    let mut pending: Option<PendingAuthorization> = None;
    for (index, line) in rendered.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix(PRINCIPAL_MARKER_PREFIX) {
            if pending.is_some() {
                return Err(AuthorizedUsersParseError::MarkerWithoutNkey { line_number });
            }
            let principal = NatsPrincipal::try_from_authority_key(value.trim()).map_err(|_| {
                AuthorizedUsersParseError::InvalidPrincipal {
                    line_number,
                    value: value.to_owned(),
                }
            })?;
            pending = Some(PendingAuthorization {
                principal,
                credential_name: None,
                credential_role: None,
            });
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(CREDENTIAL_NAME_MARKER_PREFIX) {
            let Some(pending) = pending.as_mut() else {
                return Err(
                    AuthorizedUsersParseError::CredentialMarkerWithoutPrincipal { line_number },
                );
            };
            pending.credential_name =
                Some(CredentialName::try_new(value).map_err(|_| {
                    AuthorizedUsersParseError::InvalidCredentialName { line_number }
                })?);
            continue;
        }
        if let Some(value) = trimmed.strip_prefix(CREDENTIAL_ROLE_MARKER_PREFIX) {
            let Some(pending) = pending.as_mut() else {
                return Err(
                    AuthorizedUsersParseError::CredentialMarkerWithoutPrincipal { line_number },
                );
            };
            pending.credential_role = Some(match value.trim() {
                "operator" => CredentialRole::Operator,
                value => {
                    return Err(AuthorizedUsersParseError::InvalidCredentialRole {
                        line_number,
                        value: value.to_owned(),
                    });
                }
            });
            continue;
        }
        if let Some(value) = trimmed.strip_prefix("nkey:") {
            let Some(pending) = pending.take() else {
                return Err(AuthorizedUsersParseError::NkeyWithoutMarker { line_number });
            };
            let public_key = NatsUserPublicKey::try_new(value.trim()).map_err(|_| {
                AuthorizedUsersParseError::InvalidPublicKey {
                    line_number,
                    value: value.trim().to_owned(),
                }
            })?;
            let PendingAuthorization {
                principal,
                credential_name,
                credential_role,
            } = pending;
            users.push(match principal {
                NatsPrincipal::Operator => {
                    let (Some(name), Some(role)) = (credential_name, credential_role) else {
                        return Err(AuthorizedUsersParseError::MissingCredentialMetadata {
                            line_number,
                        });
                    };
                    NatsAuthorizationGrant::Credential(CredentialGrant {
                        public_key,
                        name,
                        role,
                    })
                }
                NatsPrincipal::Machine { machine_id } => internal_authorization(
                    NatsInternalAuthority::Machine { machine_id },
                    credential_name,
                    credential_role,
                    public_key,
                    line_number,
                )?,
                NatsPrincipal::Controller => internal_authorization(
                    NatsInternalAuthority::Controller,
                    credential_name,
                    credential_role,
                    public_key,
                    line_number,
                )?,
                NatsPrincipal::Join => internal_authorization(
                    NatsInternalAuthority::Join,
                    credential_name,
                    credential_role,
                    public_key,
                    line_number,
                )?,
                NatsPrincipal::System => internal_authorization(
                    NatsInternalAuthority::System,
                    credential_name,
                    credential_role,
                    public_key,
                    line_number,
                )?,
            });
        }
    }
    if pending.is_some() {
        return Err(AuthorizedUsersParseError::TrailingMarker);
    }
    Ok(users)
}

fn internal_authorization(
    authority: NatsInternalAuthority,
    credential_name: Option<CredentialName>,
    credential_role: Option<CredentialRole>,
    public_key: NatsUserPublicKey,
    line_number: usize,
) -> Result<NatsAuthorizationGrant, AuthorizedUsersParseError> {
    if credential_name.is_some() || credential_role.is_some() {
        return Err(
            AuthorizedUsersParseError::InternalPrincipalHasCredentialMetadata { line_number },
        );
    }
    Ok(NatsAuthorizationGrant::Internal {
        authority,
        public_key,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthorizedUsersParseError {
    #[error("authorized-users line {line_number}: principal marker is not followed by an nkey")]
    MarkerWithoutNkey { line_number: usize },
    #[error("authorized-users line {line_number}: nkey entry has no preceding principal marker")]
    NkeyWithoutMarker { line_number: usize },
    #[error("authorized-users file ends with a principal marker and no nkey")]
    TrailingMarker,
    #[error("authorized-users line {line_number}: {value:?} is not a principal authority key")]
    InvalidPrincipal { line_number: usize, value: String },
    #[error("authorized-users line {line_number}: credential marker has no principal marker")]
    CredentialMarkerWithoutPrincipal { line_number: usize },
    #[error("authorized-users line {line_number}: credential name is invalid")]
    InvalidCredentialName { line_number: usize },
    #[error("authorized-users line {line_number}: {value:?} is not a credential role")]
    InvalidCredentialRole { line_number: usize, value: String },
    #[error("authorized-users line {line_number}: operator credential metadata is incomplete")]
    MissingCredentialMetadata { line_number: usize },
    #[error("authorized-users line {line_number}: internal principal has credential metadata")]
    InternalPrincipalHasCredentialMetadata { line_number: usize },
    #[error("authorized-users line {line_number}: {value:?} is not an NKey user public key")]
    InvalidPublicKey { line_number: usize, value: String },
}

#[must_use]
pub fn inbox_prefix(principal: &NatsPrincipal) -> String {
    match principal {
        NatsPrincipal::Machine { machine_id } => format!("_INBOX_machine_{}", machine_id.as_str()),
        NatsPrincipal::Controller => "_INBOX_ctl".to_owned(),
        NatsPrincipal::Operator => "_INBOX_operator".to_owned(),
        NatsPrincipal::Join => "_INBOX_join".to_owned(),
        NatsPrincipal::System => "_INBOX_sys".to_owned(),
    }
}

#[must_use]
pub fn inbox_subscribe_scope(principal: &NatsPrincipal) -> String {
    format!("{}.>", inbox_prefix(principal))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsPermissionProfile {
    pub principal: NatsPrincipal,
    pub publish: SubjectPermissions,
    pub subscribe: SubjectPermissions,
    pub allow_responses: ResponsePermission,
}

impl NatsPermissionProfile {
    #[must_use]
    pub fn render(principal: NatsPrincipal) -> Self {
        let inbox_scope = inbox_subscribe_scope(&principal);
        match &principal {
            NatsPrincipal::Machine { machine_id } => {
                let mut publish_allow = request_reply_publications(&principal);
                publish_allow.push(INTENT_GET.to_owned());
                publish_allow.push(INGRESS_ENDPOINT_GET.to_owned());
                publish_allow.push(machine_facts(machine_id));
                publish_allow.push(machine_container_facts(machine_id));
                publish_allow.push(gateway_status(machine_id));
                Self {
                    principal: principal.clone(),
                    publish: SubjectPermissions::allowing_all(publish_allow),
                    subscribe: machine_service_server_subscriptions(machine_id, inbox_scope),
                    allow_responses: ResponsePermission::Allowed,
                }
            }
            NatsPrincipal::Controller => Self {
                principal: principal.clone(),
                publish: controller_publications(),
                subscribe: controller_subscriptions(inbox_scope),
                allow_responses: ResponsePermission::Allowed,
            },
            NatsPrincipal::Operator => Self {
                principal: principal.clone(),
                publish: api_service_client_publications(),
                subscribe: SubjectPermissions::allowing([
                    inbox_scope,
                    INGRESS_ENDPOINT_CHANGED.to_owned(),
                    OPERATION_PROGRESS_SCOPE.to_owned(),
                    RUNTIME_SNAPSHOT_STREAM.to_owned(),
                ]),
                allow_responses: ResponsePermission::Denied,
            },
            NatsPrincipal::Join => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([
                    JOIN_MACHINE_REDEEM.to_owned(),
                    JOIN_MACHINE_REPORT.to_owned(),
                ]),
                subscribe: SubjectPermissions::allowing([inbox_scope]),
                allow_responses: ResponsePermission::Denied,
            },
            NatsPrincipal::System => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([SYSTEM_REQUESTS.to_owned()]),
                subscribe: SubjectPermissions::allowing([SYSTEM_EVENTS.to_owned(), inbox_scope]),
                allow_responses: ResponsePermission::Denied,
            },
        }
    }
}

#[must_use]
fn api_service_client_publications() -> SubjectPermissions {
    SubjectPermissions::allowing([
        OPERATOR_RPC_QUERY_SCOPE.to_owned(),
        OPERATOR_RPC_COMMAND_SCOPE.to_owned(),
        OPERATOR_MACHINE_IMAGE_QUERY_SCOPE.to_owned(),
        OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE.to_owned(),
        INTENT_GET.to_owned(),
        INGRESS_ENDPOINT_GET.to_owned(),
    ])
}

#[must_use]
fn machine_service_client_publications() -> SubjectPermissions {
    SubjectPermissions::allowing([
        MACHINE_RPC_QUERY_SCOPE.to_owned(),
        MACHINE_RPC_COMMAND_SCOPE.to_owned(),
    ])
}

#[must_use]
fn controller_subscriptions(inbox_scope: String) -> SubjectPermissions {
    SubjectPermissions::allowing([
        OPERATOR_RPC_QUERY_SCOPE.to_owned(),
        OPERATOR_RPC_COMMAND_SCOPE.to_owned(),
        JOIN_MACHINE_REDEEM.to_owned(),
        JOIN_MACHINE_REPORT.to_owned(),
        CORE_RPC_QUERY_SCOPE.to_owned(),
        MACHINE_RPC_QUERY_SCOPE.to_owned(),
        INTENT_GET.to_owned(),
        INTENT_CHANGED.to_owned(),
        INGRESS_ENDPOINT_CHANGED.to_owned(),
        machine_facts_scope(),
        gateway_status_scope(),
        NATS_SERVICE_DISCOVERY_SCOPE.to_owned(),
        inbox_scope,
    ])
}

#[must_use]
fn machine_service_server_subscriptions(
    machine_id: &MachineId,
    inbox_scope: String,
) -> SubjectPermissions {
    SubjectPermissions::allowing([
        machine_service_query_scope(machine_id),
        machine_service_command_scope(machine_id),
        INTENT_CHANGED.to_owned(),
        INGRESS_ENDPOINT_CHANGED.to_owned(),
        PENDING_MACHINE_JOINS_CHANGED.to_owned(),
        machine_facts_scope(),
        gateway_status_scope(),
        NATS_SERVICE_DISCOVERY_SCOPE.to_owned(),
        inbox_scope,
    ])
}

#[must_use]
fn controller_publications() -> SubjectPermissions {
    let mut allow = request_reply_publications(&NatsPrincipal::Controller);
    allow.push(OPERATOR_INIT_FIRST_MACHINE_ACTIVATE.to_owned());
    allow.extend(machine_service_client_publications().into_allowed_subjects());
    allow.extend([
        CORE_RPC_QUERY_SCOPE.to_owned(),
        OPERATION_PROGRESS_SCOPE.to_owned(),
        INTENT_CHANGED.to_owned(),
        INGRESS_ENDPOINT_CHANGED.to_owned(),
        PENDING_MACHINE_JOINS_CHANGED.to_owned(),
        RUNTIME_SNAPSHOT_STREAM.to_owned(),
    ]);
    SubjectPermissions::allowing_all(allow)
}

#[must_use]
fn request_reply_publications(principal: &NatsPrincipal) -> Vec<String> {
    vec![inbox_subscribe_scope(principal)]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectPermissions {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl SubjectPermissions {
    #[must_use]
    pub fn allowing<const N: usize>(patterns: [impl Into<String>; N]) -> Self {
        Self::allowing_all(patterns.into_iter().map(Into::into).collect())
    }

    #[must_use]
    pub fn allowing_all(patterns: Vec<String>) -> Self {
        Self {
            allow: patterns,
            deny: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_denied<const N: usize>(mut self, patterns: [&'static str; N]) -> Self {
        self.deny
            .extend(patterns.into_iter().map(ToOwned::to_owned));
        self
    }

    #[must_use]
    pub fn allowed_subjects(&self) -> &[String] {
        &self.allow
    }

    #[must_use]
    pub fn denied_subjects(&self) -> &[String] {
        &self.deny
    }

    #[must_use]
    fn into_allowed_subjects(self) -> Vec<String> {
        self.allow
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsePermission {
    Allowed,
    Denied,
}

/// Renders the NATS authorization include from durable authority intent.
#[must_use]
pub fn render_authorized_users(users: &[NatsAuthorizationGrant]) -> String {
    let mut rendered = String::from("authorization {\n  users [\n");
    for user in users {
        let principal = user.principal();
        let credential = match user {
            NatsAuthorizationGrant::Credential(grant) => Some(grant),
            NatsAuthorizationGrant::Internal { .. } => None,
        };
        let profile = NatsPermissionProfile::render(principal);
        rendered.push_str("    {\n");
        rendered.push_str(&format!(
            "      # ployz-principal: {}\n",
            profile.principal.authority_key()
        ));
        if let Some(CredentialGrant { name, role, .. }) = credential {
            rendered.push_str(&format!(
                "      # ployz-credential-name: {}\n",
                name.as_str()
            ));
            let role = match role {
                CredentialRole::Operator => "operator",
            };
            rendered.push_str(&format!("      # ployz-credential-role: {role}\n"));
        }
        rendered.push_str(&format!("      nkey: {}\n", user.public_key().as_str()));
        rendered.push_str("      permissions {\n        publish {\n");
        rendered.push_str(&render_subject_list(
            "allow",
            profile.publish.allowed_subjects(),
        ));
        if !profile.publish.denied_subjects().is_empty() {
            rendered.push_str(&render_subject_list(
                "deny",
                profile.publish.denied_subjects(),
            ));
        }
        rendered.push_str("        }\n        subscribe {\n");
        rendered.push_str(&render_subject_list(
            "allow",
            profile.subscribe.allowed_subjects(),
        ));
        if !profile.subscribe.denied_subjects().is_empty() {
            rendered.push_str(&render_subject_list(
                "deny",
                profile.subscribe.denied_subjects(),
            ));
        }
        rendered.push_str("        }\n");
        match profile.allow_responses {
            ResponsePermission::Allowed => rendered.push_str("        allow_responses: true\n"),
            ResponsePermission::Denied => {}
        }
        rendered.push_str("      }\n    }\n");
    }
    rendered.push_str("  ]\n}\n");
    rendered
}

fn render_subject_list(label: &str, subjects: &[String]) -> String {
    let quoted: Vec<String> = subjects
        .iter()
        .map(|subject| quote_nats_string(subject))
        .collect();
    format!("          {label}: [{}]\n", quoted.join(", "))
}
