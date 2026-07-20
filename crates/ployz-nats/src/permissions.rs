//! NATS subject permissions and authorization-file rendering.

use ployz_core::build::BUILD_RESPONSE_PERMISSION_EXPIRY;
use ployz_core::ids::{BuildExecutorId, BuildPoolId, MachineId};
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    NatsAuthorizationGrant, NatsInternalAuthority, NatsUserPublicKey,
};
use ployz_core::security::NatsPrincipal;

use crate::server_config::quote_nats_string;
use crate::services::{
    API_SERVICE_NAME, BUILD_EXECUTOR_SERVICE_NAME, DNS_SERVICE_NAME, GATEWAY_MACHINE_SERVICE_NAME,
    INGRESS_ENDPOINT_SERVICE_NAME, INTENT_SERVICE_NAME, MACHINE_SERVICE_NAME,
    RUNTIME_PROJECTION_SERVICE_NAME, service_discovery_subscriptions,
};
use crate::subjects::{
    BUILD_EXECUTOR_SIGNAL_LOG_SCOPE, BuildExecutorServiceEndpoint, CoreQueryEndpoint,
    INGRESS_ENDPOINT_CHANGED, INTENT_CHANGED, JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT,
    OPERATION_PROGRESS_SCOPE, OperationApiCaller, OperationApiEndpoint,
    PENDING_MACHINE_JOINS_CHANGED, RUNTIME_SNAPSHOT_SEED, RUNTIME_SNAPSHOT_STREAM,
    build_executor_log_publish_scope, build_executor_service, build_executor_service_scope,
    gateway_status, gateway_status_scope, machine_build_log_publish_scope,
    machine_build_log_subscribe_scope, machine_container_facts, machine_facts, machine_facts_scope,
    machine_service, machine_service_scope,
};
use std::time::Duration;

const SYSTEM_EVENTS: &str = "$SYS.>";
const SYSTEM_REQUESTS: &str = "$SYS.REQ.>";
const PRINCIPAL_MARKER_PREFIX: &str = "# ployz-principal: ";
const CREDENTIAL_NAME_MARKER_PREFIX: &str = "# ployz-credential-name: ";
const CREDENTIAL_ROLE_MARKER_PREFIX: &str = "# ployz-credential-role: ";
const CONTROLLER_RESPONSE_PERMISSION_EXPIRY: Duration = Duration::from_secs(2 * 60);

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
            pending.credential_role = Some(parse_credential_role(value.trim(), line_number)?);
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
                principal @ (NatsPrincipal::Operator | NatsPrincipal::BuildExecutor { .. }) => {
                    let (Some(name), Some(role)) = (credential_name, credential_role) else {
                        return Err(AuthorizedUsersParseError::MissingCredentialMetadata {
                            line_number,
                        });
                    };
                    let authorization = NatsAuthorizationGrant::Credential(CredentialGrant {
                        public_key,
                        name,
                        role,
                    });
                    if authorization.principal() != principal {
                        return Err(AuthorizedUsersParseError::PrincipalCredentialMismatch {
                            line_number,
                        });
                    }
                    authorization
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

fn parse_credential_role(
    value: &str,
    line_number: usize,
) -> Result<CredentialRole, AuthorizedUsersParseError> {
    if value == "operator" {
        return Ok(CredentialRole::Operator);
    }
    let Some(value) = value.strip_prefix("build_executor.") else {
        return Err(invalid_credential_role(line_number, value));
    };
    let mut parts = value.split('.');
    let (Some(pool_id), Some(executor_id), Some(expires_at), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid_credential_role(line_number, value));
    };
    let pool_id =
        BuildPoolId::try_new(pool_id).map_err(|_| invalid_credential_role(line_number, value))?;
    let executor_id = BuildExecutorId::try_new(executor_id)
        .map_err(|_| invalid_credential_role(line_number, value))?;
    let expires_at = expires_at
        .parse::<u64>()
        .ok()
        .and_then(|expires_at| BuildExecutorCredentialExpiresAt::try_new(expires_at).ok())
        .ok_or_else(|| invalid_credential_role(line_number, value))?;
    Ok(CredentialRole::BuildExecutor {
        pool_id,
        executor_id,
        expires_at,
    })
}

fn invalid_credential_role(line_number: usize, value: &str) -> AuthorizedUsersParseError {
    AuthorizedUsersParseError::InvalidCredentialRole {
        line_number,
        value: value.to_owned(),
    }
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
    #[error("authorized-users line {line_number}: credential metadata is incomplete")]
    MissingCredentialMetadata { line_number: usize },
    #[error("authorized-users line {line_number}: principal and credential role do not match")]
    PrincipalCredentialMismatch { line_number: usize },
    #[error("authorized-users line {line_number}: internal principal has credential metadata")]
    InternalPrincipalHasCredentialMetadata { line_number: usize },
    #[error("authorized-users line {line_number}: {value:?} is not an NKey user public key")]
    InvalidPublicKey { line_number: usize, value: String },
}

#[must_use]
pub fn inbox_prefix(principal: &NatsPrincipal) -> String {
    match principal {
        NatsPrincipal::Machine { machine_id } => format!("_INBOX_machine_{}", machine_id.as_str()),
        NatsPrincipal::BuildExecutor {
            pool_id,
            executor_id,
        } => format!(
            "_INBOX_build_executor.{}.{}",
            pool_id.as_str(),
            executor_id.as_str()
        ),
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
                let publish_allow = core_query_endpoints()
                    .map(ToOwned::to_owned)
                    .chain([
                        machine_facts(machine_id),
                        machine_container_facts(machine_id),
                        gateway_status(machine_id),
                        machine_build_log_publish_scope(machine_id),
                    ])
                    .collect();
                Self {
                    principal: principal.clone(),
                    publish: SubjectPermissions::allowing_all(publish_allow),
                    subscribe: machine_service_server_subscriptions(machine_id, inbox_scope),
                    allow_responses: ResponsePermission::RequestScoped {
                        expires: BUILD_RESPONSE_PERMISSION_EXPIRY,
                    },
                }
            }
            NatsPrincipal::BuildExecutor {
                pool_id,
                executor_id,
            } => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing_all(
                    std::iter::once(build_executor_log_publish_scope(pool_id, executor_id))
                        .chain(image_transfer_publications())
                        .collect(),
                ),
                subscribe: build_executor_service_server_subscriptions(
                    pool_id,
                    executor_id,
                    inbox_scope,
                ),
                allow_responses: ResponsePermission::RequestScoped {
                    expires: BUILD_RESPONSE_PERMISSION_EXPIRY,
                },
            },
            NatsPrincipal::Controller => Self {
                principal: principal.clone(),
                publish: controller_publications(),
                subscribe: controller_subscriptions(inbox_scope),
                allow_responses: ResponsePermission::RequestScoped {
                    expires: CONTROLLER_RESPONSE_PERMISSION_EXPIRY,
                },
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
    let mut allow: Vec<String> = OperationApiEndpoint::ALL
        .iter()
        .filter(|endpoint| endpoint.caller() == OperationApiCaller::Operator)
        .map(|endpoint| endpoint.subject().to_owned())
        .collect();
    allow.push(RUNTIME_SNAPSHOT_SEED.to_owned());
    allow.extend(core_query_endpoints().map(ToOwned::to_owned));
    allow.extend(image_transfer_publications());
    SubjectPermissions::allowing_all(allow)
}

#[must_use]
fn core_query_endpoints() -> impl Iterator<Item = &'static str> {
    CoreQueryEndpoint::ALL
        .iter()
        .map(|endpoint| endpoint.subject())
}

#[must_use]
fn image_transfer_publications() -> impl Iterator<Item = String> {
    crate::subjects::MachineServiceEndpoint::IMAGE_TRANSFER
        .iter()
        .copied()
        .map(machine_service_scope)
}

#[must_use]
fn controller_subscriptions(inbox_scope: String) -> SubjectPermissions {
    let mut allow: Vec<String> = OperationApiEndpoint::ALL
        .iter()
        .map(|endpoint| endpoint.subject().to_owned())
        .collect();
    allow.extend(core_query_endpoints().map(ToOwned::to_owned));
    allow.extend([
        RUNTIME_SNAPSHOT_SEED.to_owned(),
        INTENT_CHANGED.to_owned(),
        INGRESS_ENDPOINT_CHANGED.to_owned(),
        machine_facts_scope(),
        machine_build_log_subscribe_scope(),
        BUILD_EXECUTOR_SIGNAL_LOG_SCOPE.to_owned(),
        gateway_status_scope(),
    ]);
    allow.extend(service_discovery_subscriptions(&[
        API_SERVICE_NAME,
        INTENT_SERVICE_NAME,
        INGRESS_ENDPOINT_SERVICE_NAME,
        RUNTIME_PROJECTION_SERVICE_NAME,
    ]));
    allow.push(inbox_scope);
    SubjectPermissions::allowing_all(allow)
}

#[must_use]
fn build_executor_service_server_subscriptions(
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
    inbox_scope: String,
) -> SubjectPermissions {
    let mut allow: Vec<String> = BuildExecutorServiceEndpoint::ALL
        .iter()
        .copied()
        .map(|endpoint| build_executor_service(pool_id, executor_id, endpoint))
        .collect();
    allow.extend(service_discovery_subscriptions(&[
        BUILD_EXECUTOR_SERVICE_NAME,
    ]));
    allow.push(inbox_scope);
    SubjectPermissions::allowing_all(allow)
}

#[must_use]
fn machine_service_server_subscriptions(
    machine_id: &MachineId,
    inbox_scope: String,
) -> SubjectPermissions {
    let mut allow: Vec<String> = crate::subjects::MachineServiceEndpoint::ALL
        .iter()
        .copied()
        .map(|endpoint| machine_service(machine_id, endpoint))
        .collect();
    allow.extend([
        INTENT_CHANGED.to_owned(),
        INGRESS_ENDPOINT_CHANGED.to_owned(),
        PENDING_MACHINE_JOINS_CHANGED.to_owned(),
        machine_facts_scope(),
        gateway_status_scope(),
    ]);
    allow.extend(service_discovery_subscriptions(&[
        MACHINE_SERVICE_NAME,
        GATEWAY_MACHINE_SERVICE_NAME,
        DNS_SERVICE_NAME,
    ]));
    allow.push(inbox_scope);
    SubjectPermissions::allowing_all(allow)
}

#[must_use]
fn controller_publications() -> SubjectPermissions {
    let mut allow: Vec<String> = crate::subjects::MachineServiceEndpoint::ALL
        .iter()
        .copied()
        .map(machine_service_scope)
        .collect();
    allow.extend(
        BuildExecutorServiceEndpoint::ALL
            .iter()
            .copied()
            .map(build_executor_service_scope),
    );
    allow.push(
        OperationApiEndpoint::InitFirstMachineActivate
            .subject()
            .to_owned(),
    );
    allow.extend(core_query_endpoints().map(ToOwned::to_owned));
    allow.extend([
        OPERATION_PROGRESS_SCOPE.to_owned(),
        INTENT_CHANGED.to_owned(),
        INGRESS_ENDPOINT_CHANGED.to_owned(),
        PENDING_MACHINE_JOINS_CHANGED.to_owned(),
        RUNTIME_SNAPSHOT_STREAM.to_owned(),
    ]);
    SubjectPermissions::allowing_all(allow)
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponsePermission {
    RequestScoped { expires: std::time::Duration },
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
                CredentialRole::Operator => "operator".to_owned(),
                CredentialRole::BuildExecutor {
                    pool_id,
                    executor_id,
                    expires_at,
                } => format!(
                    "build_executor.{}.{}.{}",
                    pool_id.as_str(),
                    executor_id.as_str(),
                    expires_at.unix_seconds()
                ),
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
            ResponsePermission::RequestScoped { expires } => rendered.push_str(&format!(
                "        allow_responses: {{ max: 1, expires: {}s }}\n",
                expires.as_secs()
            )),
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
