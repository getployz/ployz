//! NATS account, user, and subject permission rendering.

use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{
    API_SERVICE_SCOPE, AUDIT_STREAM_SUBJECT, DEPLOY_SUBMITTED_EVENTS_SUBJECT, JOBS_STREAM_SUBJECT,
    NODE_SERVICE_SCOPE, OPS_STREAM_SUBJECT, node_observation_scope, node_service_scope,
};

const RESPONSE_INBOX: &str = "_INBOX.>";
const CORE_KV_WRITES: &str = "$KV.KV_CORE.>";
const SYSTEM_EVENTS: &str = "$SYS.>";
const SYSTEM_REQUESTS: &str = "$SYS.REQ.>";

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
        match &principal {
            NatsPrincipal::Node { node_id } => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([node_observation_scope(node_id)])
                    .with_denied([CORE_KV_WRITES]),
                subscribe: SubjectPermissions::allowing([node_service_scope(node_id)]),
                allow_responses: ResponsePermission::Allowed,
            },
            NatsPrincipal::Controller => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([
                    NODE_SERVICE_SCOPE.to_owned(),
                    OPS_STREAM_SUBJECT.to_owned(),
                    JOBS_STREAM_SUBJECT.to_owned(),
                    AUDIT_STREAM_SUBJECT.to_owned(),
                ])
                .with_denied([CORE_KV_WRITES]),
                subscribe: SubjectPermissions::allowing([
                    DEPLOY_SUBMITTED_EVENTS_SUBJECT.to_owned(),
                    JOBS_STREAM_SUBJECT.to_owned(),
                    RESPONSE_INBOX.to_owned(),
                ]),
                allow_responses: ResponsePermission::Denied,
            },
            NatsPrincipal::User => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([API_SERVICE_SCOPE]),
                subscribe: SubjectPermissions::allowing([RESPONSE_INBOX, OPS_STREAM_SUBJECT]),
                allow_responses: ResponsePermission::Denied,
            },
            NatsPrincipal::System => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([SYSTEM_REQUESTS]),
                subscribe: SubjectPermissions::allowing([SYSTEM_EVENTS, RESPONSE_INBOX]),
                allow_responses: ResponsePermission::Denied,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectPermissions {
    allow: Vec<String>,
    deny: Vec<String>,
}

impl SubjectPermissions {
    #[must_use]
    pub fn allowing<const N: usize>(patterns: [impl Into<String>; N]) -> Self {
        Self {
            allow: patterns.into_iter().map(Into::into).collect(),
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
    Allowed,
    Denied,
}
