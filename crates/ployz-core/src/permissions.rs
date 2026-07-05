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
use crate::state::{
    CoreStateKeyFamily, GatewayStatusObservationKey, KV_CORE_BUCKET, KV_OBS_BUCKET, KV_OPS_BUCKET,
    MachineContainerObservationKey, MachinePublicIpObservationKey,
};
use crate::subjects::{
    API_MACHINE_JOIN_REDEEM, API_MACHINE_JOIN_REPORT, API_SERVICE_SCOPE, INTENT_CHANGED,
    INTENT_GET, MACHINE_SERVICE_SCOPE, OPS_STREAM_SUBJECT, machine_observation_scope,
    machine_service_scope,
};

const SYSTEM_EVENTS: &str = "$SYS.>";
const SYSTEM_REQUESTS: &str = "$SYS.REQ.>";
const JETSTREAM_API_SCOPE: &str = "$JS.API.>";
const JETSTREAM_ACK_SCOPE: &str = "$JS.ACK.>";
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

/// The controller's KV write grant for one state-key family. The wildcard
/// pattern comes from the key type itself (via [`CoreStateKeyFamily`]), so a
/// key format and its grant cannot drift apart; this function only adds the
/// bucket and decides nothing about key shape.
#[must_use]
pub fn core_state_kv_write_scope(family: CoreStateKeyFamily) -> String {
    format!("$KV.{KV_CORE_BUCKET}.{}", family.wildcard_pattern())
}

/// Operation status writes span the whole `KV_OPS` bucket: status records
/// are the bucket's only tenant, so this is a bucket grant rather than a
/// [`CoreStateKeyFamily`] and is immune to key-arity drift.
#[must_use]
pub fn operation_status_kv_write_scope() -> String {
    format!("$KV.{KV_OPS_BUCKET}.>")
}

/// A machine's observation writes in `KV_OBS`, fenced to its own keys.
///
/// The subjects derive from the same typed keys the observation store
/// writes (`containers.<machine>`, `machines.<machine>.public_ip`,
/// `gateways.<machine>.status`), so one machine's Machine credential cannot
/// overwrite another machine's observations — routing and serving
/// eligibility read these. Machines keep read access to the whole bucket via
/// [`kv_read_js_api_subjects`].
#[must_use]
pub fn machine_observation_kv_write_subjects(machine_id: &MachineId) -> [String; 3] {
    let containers = MachineContainerObservationKey::from_machine_id(machine_id);
    let public_ip = MachinePublicIpObservationKey::from_machine_id(machine_id);
    let gateway_status = GatewayStatusObservationKey::from_machine_id(machine_id);
    [
        format!("$KV.{KV_OBS_BUCKET}.{}", containers.as_str()),
        format!("$KV.{KV_OBS_BUCKET}.{}", public_ip.as_str()),
        format!("$KV.{KV_OBS_BUCKET}.{}", gateway_status.as_str()),
    ]
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
            NatsPrincipal::Machine { machine_id } => {
                let mut publish_allow = request_reply_publications(&principal);
                publish_allow.push(INTENT_GET.to_owned());
                publish_allow.push(machine_observation_scope(machine_id));
                publish_allow.extend(machine_observation_kv_write_subjects(machine_id));
                publish_allow.extend(kv_read_js_api_subjects(KV_OBS_BUCKET));
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
                subscribe: api_service_server_subscriptions(inbox_scope),
                allow_responses: ResponsePermission::Allowed,
            },
            NatsPrincipal::User => Self {
                principal: principal.clone(),
                publish: api_service_client_publications(),
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
    SubjectPermissions::allowing([API_SERVICE_SCOPE.to_owned()])
}

#[must_use]
fn machine_service_client_publications() -> SubjectPermissions {
    SubjectPermissions::allowing([MACHINE_SERVICE_SCOPE.to_owned()])
}

#[must_use]
fn api_service_server_subscriptions(inbox_scope: String) -> SubjectPermissions {
    SubjectPermissions::allowing([
        API_SERVICE_SCOPE.to_owned(),
        INTENT_CHANGED.to_owned(),
        INTENT_GET.to_owned(),
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
        machine_service_scope(machine_id),
        INTENT_CHANGED.to_owned(),
        NATS_SERVICE_DISCOVERY_SCOPE.to_owned(),
        inbox_scope,
    ])
}

#[must_use]
fn controller_publications() -> SubjectPermissions {
    let mut allow = request_reply_publications(&NatsPrincipal::Controller);
    allow.extend(api_service_client_publications().into_allowed_subjects());
    allow.extend(machine_service_client_publications().into_allowed_subjects());
    allow.extend([
        OPS_STREAM_SUBJECT.to_owned(),
        INTENT_GET.to_owned(),
        INTENT_CHANGED.to_owned(),
        JETSTREAM_API_SCOPE.to_owned(),
        JETSTREAM_ACK_SCOPE.to_owned(),
    ]);
    allow.extend(CoreStateKeyFamily::ALL.map(core_state_kv_write_scope));
    allow.push(operation_status_kv_write_scope());
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
