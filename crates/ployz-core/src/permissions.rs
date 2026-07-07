//! NATS subject permission profiles per principal.
//!
//! Every NATS principal renders to one [`NatsPermissionProfile`]: the
//! server-side allow/deny subject lists that become its `authorization`
//! block entry. Request-reply inboxes are per-principal prefixes derived by
//! [`inbox_prefix`]; no profile may subscribe the shared `_INBOX.>` scope,
//! so a low-privilege credential cannot sniff another client's replies.
//!
//! There is no Gateway principal in v1: gateway and DNS processes
//! authenticate as their machine's `Machine{machine_id}` user, and read
//! operator intent through the core intent service.

use crate::ids::MachineId;
use crate::security::NatsPrincipal;
use crate::subjects::{
    CORE_RPC_QUERY_SCOPE, INTENT_CHANGED, INTENT_GET, JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT,
    MACHINE_RPC_COMMAND_SCOPE, MACHINE_RPC_QUERY_SCOPE, OPERATION_PROGRESS_SCOPE,
    OPERATOR_RPC_COMMAND_SCOPE, OPERATOR_RPC_QUERY_SCOPE, gateway_status, gateway_status_scope,
    machine_container_facts, machine_facts, machine_facts_scope, machine_service_command_scope,
    machine_service_query_scope,
};

const SYSTEM_EVENTS: &str = "$SYS.>";
const SYSTEM_REQUESTS: &str = "$SYS.REQ.>";
const NATS_SERVICE_DISCOVERY_SCOPE: &str = "$SRV.>";

/// The request-reply inbox prefix a principal connects with.
///
/// Profile render (subscribe allow) and client connect (custom inbox
/// prefix) both derive from this one function so they cannot disagree.
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

/// The subscribe scope covering a principal's own inboxes and nothing else.
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
                    OPERATION_PROGRESS_SCOPE.to_owned(),
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
        machine_facts_scope(),
        gateway_status_scope(),
        NATS_SERVICE_DISCOVERY_SCOPE.to_owned(),
        inbox_scope,
    ])
}

#[must_use]
fn controller_publications() -> SubjectPermissions {
    let mut allow = request_reply_publications(&NatsPrincipal::Controller);
    allow.extend(machine_service_client_publications().into_allowed_subjects());
    allow.extend([
        CORE_RPC_QUERY_SCOPE.to_owned(),
        OPERATION_PROGRESS_SCOPE.to_owned(),
        INTENT_CHANGED.to_owned(),
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
