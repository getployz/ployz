//! NATS subject permission profiles per principal.
//!
//! Every NATS principal renders to one [`NatsPermissionProfile`]: the
//! server-side allow/deny subject lists that become its `authorization`
//! block entry. Request-reply inboxes are per-principal prefixes derived by
//! [`inbox_prefix`]; no profile may subscribe the shared `_INBOX.>` scope,
//! so a low-privilege credential cannot sniff another client's replies.
//!
//! There is no Gateway principal in v1: gateway and DNS processes
//! authenticate as their machine's `Node{node_id}` user, and the Node
//! profile carries the read-only route-state subjects they need.

use crate::security::NatsPrincipal;
use crate::state::{
    ACTIVE_MACHINE_STATE_PREFIX, ACTIVE_ROUTE_STATE_PREFIX, ACTIVE_SERVICE_STATE_PREFIX,
    KV_CORE_BUCKET, KV_LOCKS_BUCKET, KV_OBS_BUCKET, KV_OPS_BUCKET, NATS_AUTHORIZED_USER_PREFIX,
};
use crate::subjects::{
    API_MACHINE_JOIN_REDEEM, API_MACHINE_JOIN_REPORT, API_SERVICE_SCOPE, AUDIT_STREAM_SUBJECT,
    JOBS_STREAM_SUBJECT, NODE_SERVICE_SCOPE, OPS_STREAM_SUBJECT, node_observation_scope,
    node_service_scope,
};

const CORE_KV_WRITES: &str = "$KV.KV_CORE.>";
const SYSTEM_EVENTS: &str = "$SYS.>";
const SYSTEM_REQUESTS: &str = "$SYS.REQ.>";
const JETSTREAM_API_SCOPE: &str = "$JS.API.>";
const JETSTREAM_ACK_SCOPE: &str = "$JS.ACK.>";
/// Backup artifacts live in the `PLZ_BACKUPS` object store; its chunk and
/// meta writes publish under this subject space.
const BACKUP_OBJECT_SCOPE: &str = "$O.PLZ_BACKUPS.>";

/// The request-reply inbox prefix a principal connects with.
///
/// Profile render (subscribe allow) and client connect (custom inbox
/// prefix) both derive from this one function so they cannot disagree.
#[must_use]
pub fn inbox_prefix(principal: &NatsPrincipal) -> String {
    match principal {
        NatsPrincipal::Node { node_id } => format!("_INBOX_node_{}", node_id.as_str()),
        NatsPrincipal::Controller => "_INBOX_ctl".to_owned(),
        NatsPrincipal::User => "_INBOX_user".to_owned(),
        NatsPrincipal::Join => "_INBOX_join".to_owned(),
        NatsPrincipal::System => "_INBOX_sys".to_owned(),
    }
}

/// The subscribe scope covering a principal's own inboxes and nothing else.
#[must_use]
pub fn inbox_subscribe_scope(principal: &NatsPrincipal) -> String {
    format!("{}.>", inbox_prefix(principal))
}

#[must_use]
pub fn active_service_state_kv_write_scope() -> String {
    format!("$KV.{KV_CORE_BUCKET}.{ACTIVE_SERVICE_STATE_PREFIX}.*")
}

#[must_use]
pub fn active_route_state_kv_write_scope() -> String {
    format!("$KV.{KV_CORE_BUCKET}.{ACTIVE_ROUTE_STATE_PREFIX}.*.*")
}

#[must_use]
pub fn active_machine_state_kv_write_scope() -> String {
    format!("$KV.{KV_CORE_BUCKET}.{ACTIVE_MACHINE_STATE_PREFIX}.*")
}

/// Control's writes of the durable authorized-principal set.
#[must_use]
pub fn nats_authorized_user_kv_write_scope() -> String {
    format!("$KV.{KV_CORE_BUCKET}.{NATS_AUTHORIZED_USER_PREFIX}.*")
}

#[must_use]
pub fn operation_status_kv_write_scope() -> String {
    format!("$KV.{KV_OPS_BUCKET}.>")
}

#[must_use]
pub fn lock_kv_write_scope() -> String {
    format!("$KV.{KV_LOCKS_BUCKET}.>")
}

#[must_use]
pub fn observation_kv_write_scope() -> String {
    format!("$KV.{KV_OBS_BUCKET}.>")
}

/// JetStream API subjects a client publishes to for read-only access to one
/// KV bucket: stream info, direct gets, and ordered-consumer lifecycle for
/// watches/scans. Replies and watch deliveries arrive on the client's own
/// inbox prefix, so no extra subscribe permission is required.
#[must_use]
pub fn kv_read_js_api_subjects(bucket: &str) -> [String; 8] {
    let stream = format!("KV_{bucket}");
    [
        format!("$JS.API.STREAM.INFO.{stream}"),
        format!("$JS.API.DIRECT.GET.{stream}"),
        format!("$JS.API.DIRECT.GET.{stream}.>"),
        // Unnamed ordered consumers (KV watches/scans) create against the
        // bare stream subject; named consumers append name and filter.
        format!("$JS.API.CONSUMER.CREATE.{stream}"),
        format!("$JS.API.CONSUMER.CREATE.{stream}.>"),
        format!("$JS.API.CONSUMER.INFO.{stream}.>"),
        format!("$JS.API.CONSUMER.MSG.NEXT.{stream}.>"),
        format!("$JS.API.CONSUMER.DELETE.{stream}.>"),
    ]
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
            NatsPrincipal::Node { node_id } => {
                // Gateway and DNS authenticate as the machine's Node user in
                // v1, so this profile carries their read-only route-state
                // access (KV_CORE reads stay read-only via the publish deny).
                let mut publish_allow = vec![
                    node_observation_scope(node_id),
                    observation_kv_write_scope(),
                ];
                publish_allow.extend(kv_read_js_api_subjects(KV_OBS_BUCKET));
                publish_allow.extend(kv_read_js_api_subjects(KV_CORE_BUCKET));
                Self {
                    principal: principal.clone(),
                    publish: SubjectPermissions::allowing_all(publish_allow)
                        .with_denied([CORE_KV_WRITES]),
                    subscribe: SubjectPermissions::allowing([
                        node_service_scope(node_id),
                        inbox_scope,
                    ]),
                    allow_responses: ResponsePermission::Allowed,
                }
            }
            NatsPrincipal::Controller => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([
                    NODE_SERVICE_SCOPE.to_owned(),
                    OPS_STREAM_SUBJECT.to_owned(),
                    JOBS_STREAM_SUBJECT.to_owned(),
                    AUDIT_STREAM_SUBJECT.to_owned(),
                    JETSTREAM_API_SCOPE.to_owned(),
                    JETSTREAM_ACK_SCOPE.to_owned(),
                    BACKUP_OBJECT_SCOPE.to_owned(),
                    active_service_state_kv_write_scope(),
                    active_route_state_kv_write_scope(),
                    active_machine_state_kv_write_scope(),
                    nats_authorized_user_kv_write_scope(),
                    operation_status_kv_write_scope(),
                    lock_kv_write_scope(),
                ]),
                // Control serves the user-facing command API, so it
                // subscribes the API service scope and answers requests.
                subscribe: SubjectPermissions::allowing([
                    JOBS_STREAM_SUBJECT.to_owned(),
                    API_SERVICE_SCOPE.to_owned(),
                    inbox_scope,
                ]),
                allow_responses: ResponsePermission::Allowed,
            },
            NatsPrincipal::User => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([API_SERVICE_SCOPE.to_owned()]),
                subscribe: SubjectPermissions::allowing([
                    inbox_scope,
                    OPS_STREAM_SUBJECT.to_owned(),
                ]),
                allow_responses: ResponsePermission::Denied,
            },
            NatsPrincipal::Join => Self {
                principal: principal.clone(),
                publish: SubjectPermissions::allowing([
                    API_MACHINE_JOIN_REDEEM.to_owned(),
                    API_MACHINE_JOIN_REPORT.to_owned(),
                ]),
                subscribe: SubjectPermissions::allowing([inbox_scope]),
                allow_responses: ResponsePermission::Allowed,
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
    Allowed,
    Denied,
}
