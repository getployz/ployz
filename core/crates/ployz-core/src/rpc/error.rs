//! The Machine RPC error model: `RpcError`, its codes and retry classes, the codec error, and the gRPC status helpers that carry them.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use thiserror::Error;
use ts_rs::TS;

use crate::PullPolicy;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("Could not encode the JSON payload.")]
    EncodeJson(#[source] serde_json::Error),
    #[error("Could not decode the JSON payload.")]
    DecodeJson(#[source] serde_json::Error),
    #[error("unsupported RPC command {0:?}")]
    UnsupportedCommand(String),
    #[error("unsupported protocol major {requested}; this endpoint supports {supported}")]
    UnsupportedProtocolMajor { requested: u32, supported: u32 },
    #[error("expected response kind {expected}, received {}", .actual.escape_debug())]
    UnexpectedResponse {
        expected: &'static str,
        actual: String,
    },
    #[error("expected request command {expected}, received {actual}")]
    UnexpectedRequest {
        expected: &'static str,
        actual: String,
    },
}

crate::value::open_string_enum!(RpcErrorCode, Unknown {
    InvalidArgument => "invalid_argument",
    NotFound => "not_found",
    Ambiguous => "ambiguous",
    Unsupported => "unsupported",
    Unavailable => "unavailable",
    Conflict => "conflict",
    Internal => "internal",
    Unauthenticated => "unauthenticated",
    ConfirmationRequired => "confirmation_required",
});

/// How a client may repeat a call that failed. Clients decide this; it never
/// crosses the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retry {
    /// Transient: repeat soon, within the call's own budget.
    Now,
    /// The daemon did not serve the call: repeat only where the caller waits
    /// out a restart.
    AfterRestart,
    Never,
}

impl RpcErrorCode {
    #[must_use]
    pub fn retry(&self) -> Retry {
        match self {
            Self::Unavailable => Retry::Now,
            Self::InvalidArgument
            | Self::NotFound
            | Self::Ambiguous
            | Self::Unsupported
            | Self::Conflict
            | Self::Internal
            | Self::Unauthenticated
            | Self::ConfirmationRequired
            | Self::Unknown(_) => Retry::Never,
        }
    }
}

impl fmt::Display for RpcErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_str().escape_debug().fmt(f)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Serialize, Deserialize, TS)]
#[error("{message}")]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub message: String,
    #[serde(default)]
    pub details: Value,
    /// Each source below `message` on the peer that failed, outermost first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<String>>", optional)]
    pub cause: Vec<String>,
}

/// The code of a refusal on the caller's access, such as a Server refusing a scope.
pub const FORBIDDEN_CODE: &str = "forbidden";

/// The code of a refusal that waits on a person's approval.
pub const APPROVAL_REQUIRED_CODE: &str = "approval_required";

macro_rules! named_constructors {
    ($($(#[$meta:meta])* $name:ident => $code:ident,)*) => {
        $(
            $(#[$meta])*
            #[must_use]
            pub fn $name(message: impl Into<String>) -> Self {
                Self::new(RpcErrorCode::$code, message)
            }
        )*
    };
}

impl RpcError {
    /// An error with no details and no causes.
    #[must_use]
    pub fn new(code: RpcErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Value::Null,
            cause: Vec::new(),
        }
    }

    named_constructors! {
        invalid_argument => InvalidArgument,
        not_found => NotFound,
        ambiguous => Ambiguous,
        unsupported => Unsupported,
        unavailable => Unavailable,
        conflict => Conflict,
        internal => Internal,
        unauthenticated => Unauthenticated,
        confirmation_required => ConfirmationRequired,
    }

    /// A refusal on the caller's access, coded [`FORBIDDEN_CODE`].
    #[must_use]
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(RpcErrorCode::from(FORBIDDEN_CODE), message)
    }

    /// A refusal that waits on a person's approval, coded [`APPROVAL_REQUIRED_CODE`].
    #[must_use]
    pub fn approval_required(message: impl Into<String>) -> Self {
        Self::new(RpcErrorCode::from(APPROVAL_REQUIRED_CODE), message)
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    #[must_use]
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    /// `self` with `error`'s own sentence and every source below it as its causes.
    #[must_use]
    pub fn with_cause(mut self, error: &(dyn std::error::Error + 'static)) -> Self {
        self.cause = std::iter::once(error.to_string())
            .chain(crate::error_chain::causes(error))
            .collect();
        self
    }

    /// `self` with `cause` as its causes, outermost first.
    #[must_use]
    pub fn with_causes(mut self, cause: Vec<String>) -> Self {
        self.cause = cause;
        self
    }

    /// `error`'s own sentence as the message, and every source below it as a cause.
    #[must_use]
    pub fn caused(code: RpcErrorCode, error: &(dyn std::error::Error + 'static)) -> Self {
        Self::new(code, error.to_string()).with_causes(crate::error_chain::causes(error))
    }

    /// An error the caller made because its target Machine didn't answer: the target
    /// sent nothing, so it may answer later. Read back by [`Self::target_unanswered`].
    #[must_use]
    pub fn unanswered_target(code: RpcErrorCode, message: String) -> Self {
        Self::new(code, message)
            .with_details(serde_json::json!({ "reason": TARGET_UNANSWERED_REASON }))
    }

    /// `self` from a call that failed while a Leave was removing `server`, so its copy
    /// was the Leave's to stop. Code, message and every other detail stay as they were.
    /// Read back by [`Self::departure`]. Details that aren't an object are kept unmarked.
    #[must_use]
    pub fn overlapping_leave(mut self, server: &crate::MachineName) -> Self {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(details) = &mut self.details {
            details.insert(
                DEPARTING_KEY.into(),
                serde_json::json!(Departure {
                    server: server.clone()
                }),
            );
        }
        self
    }

    /// The Leave that overlapped the call this error answers, if one did.
    #[must_use]
    pub fn departure(&self) -> Option<Departure> {
        Departure::deserialize(self.details.get(DEPARTING_KEY)?).ok()
    }

    /// `self` from a call to `machine_id` whose connection dropped after it went out and
    /// before an answer came back, so the target may have done the work. Read back by
    /// [`Self::reply_lost`]. Details that aren't an object are kept unmarked.
    #[must_use]
    pub fn reply_lost_from(mut self, machine_id: &crate::MachineId) -> Self {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(details) = &mut self.details {
            details.insert(REPLY_LOST_KEY.into(), Value::Bool(true));
            details
                .entry("machine_id")
                .or_insert_with(|| Value::String(machine_id.to_string()));
        }
        self
    }

    /// `self` from a call that never reached its target, so the target did nothing with
    /// it. Read back by [`Self::never_reached`]. Details that aren't an object are kept
    /// unmarked.
    #[must_use]
    pub fn unreached(mut self) -> Self {
        if self.details.is_null() {
            self.details = Value::Object(serde_json::Map::new());
        }
        if let Value::Object(details) = &mut self.details {
            details.insert(REACHED_KEY.into(), Value::Bool(false));
        }
        self
    }

    /// `self`, an [`Self::unanswered_target`] error from a router that couldn't dial the
    /// target, so only small packets went its way. Read back by [`Self::undialed`].
    #[must_use]
    pub fn undialed_target(mut self) -> Self {
        if let Value::Object(details) = &mut self.details {
            details.insert(UNDIALED_KEY.into(), Value::Bool(true));
        }
        self
    }

    /// Whether the router couldn't dial the unanswered target.
    #[must_use]
    pub fn undialed(&self) -> bool {
        self.details.get(UNDIALED_KEY).and_then(Value::as_bool) == Some(true)
    }

    /// Whether the call never reached its target.
    #[must_use]
    pub fn never_reached(&self) -> bool {
        self.details.get(REACHED_KEY).and_then(Value::as_bool) == Some(false)
    }

    /// Whether the connection dropped before the target's answer came back, so the
    /// target may have done the work.
    #[must_use]
    pub fn reply_lost(&self) -> bool {
        self.details.get(REPLY_LOST_KEY).and_then(Value::as_bool) == Some(true)
    }

    /// Whether the target Machine didn't answer, rather than answering with this error.
    #[must_use]
    pub fn target_unanswered(&self) -> bool {
        self.details.get("reason").and_then(Value::as_str) == Some(TARGET_UNANSWERED_REASON)
    }

    /// Whether an entry Machine's routing refused this call because it no longer lists
    /// `machine_id`, as after `ployz server rm`. A daemon from before the tag, or a
    /// Server's own not-found answer, is not.
    #[must_use]
    pub fn server_unlisted(&self, machine_id: &crate::MachineId) -> bool {
        self.code == RpcErrorCode::NotFound
            && self.details.get("reason").and_then(Value::as_str) == Some(NO_SUCH_SERVER_REASON)
            && self
                .details
                .get("targets")
                .and_then(Value::as_array)
                .is_some_and(|targets| {
                    targets
                        .iter()
                        .any(|target| target.as_str() == Some(machine_id.as_str()))
                })
    }

    /// The capability this error says its target Machine lacks, if it is a refusal.
    #[must_use]
    pub fn refusal(&self) -> Option<CapabilityRefusal> {
        CapabilityRefusal::new(&self.code, self.details.clone())
    }

    /// `self` from a pull of `image` under `policy`, naming both when the answer is not
    /// found or the registry refused it. A registry answers not found for a missing tag
    /// and a private repository alike, and some, such as ghcr.io, answer `denied`.
    #[must_use]
    pub fn pulling(mut self, image: &str, policy: PullPolicy) -> Self {
        if self.details.is_null()
            && (self.code == RpcErrorCode::NotFound || self.registry_refused())
        {
            self.details = serde_json::json!({ MISSING_IMAGE: image, PULL_POLICY: policy });
        }
        self
    }

    /// The image a not-found or refused pull named through [`Self::pulling`].
    #[must_use]
    pub fn missing_image(&self) -> Option<MissingImage<'_>> {
        if self.code != RpcErrorCode::NotFound && !self.registry_refused() {
            return None;
        }
        Some(MissingImage {
            image: self.details.get(MISSING_IMAGE)?.as_str()?,
            policy: self
                .details
                .get(PULL_POLICY)
                .and_then(|policy| PullPolicy::deserialize(policy).ok()),
        })
    }

    /// The host a failed pull could not look up, from Docker's error: the registry,
    /// its token service or a proxy. None when Ployz lost the Machine itself.
    #[must_use]
    pub fn unresolved_host(&self) -> Option<&str> {
        if self.reply_lost() || self.never_reached() || self.target_unanswered() {
            return None;
        }
        self.lines().find_map(|line| {
            let (lookup, _) = line.split_once(": no such host")?;
            let (_, host) = lookup.rsplit_once("lookup ")?;
            host.split_whitespace().next()
        })
    }

    /// Whether the registry refused a pull for want of access, whatever status Docker gave.
    fn registry_refused(&self) -> bool {
        self.lines()
            .any(|line| REGISTRY_REFUSALS.iter().any(|sign| line.contains(sign)))
    }

    fn lines(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.message.as_str()).chain(self.cause.iter().map(String::as_str))
    }

    /// The full disk a failed pull ran into, as [`DiskFull::details`] named it.
    #[must_use]
    pub fn disk_full(&self) -> Option<DiskFull> {
        serde_json::from_value(self.details.get(DISK_FULL)?.clone()).ok()
    }
}

const MISSING_IMAGE: &str = "missing_image";
/// What Docker prints when a registry refuses a pull for want of access.
const REGISTRY_REFUSALS: [&str; 3] = [
    "error from registry: denied",
    "denied: requested access",
    "unauthorized: ",
];
const PULL_POLICY: &str = "pull_policy";
const DISK_FULL: &str = "disk_full";

/// The Leave that overlapped a call: what a Server's Containers did since it began is
/// the Leave's doing. Annotates a failure's details under [`DEPARTING_KEY`], an unhealthy
/// [`HealthVerdict`], and the `ExecutionError::Health` a deploy stores.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Departure {
    pub server: crate::MachineName,
}

/// An image a pull answered not found for, from [`RpcError::missing_image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MissingImage<'a> {
    pub image: &'a str,
    /// The policy it was pulled under, when the error recorded one.
    pub policy: Option<PullPolicy>,
}

/// A pull failed while a disk it writes to had almost no free space.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiskFull {
    /// The directory on the full disk: the Docker root, or containerd's image store
    /// when that is the fuller. The wire keeps the name it shipped with.
    pub docker_root: String,
    pub free_bytes: u64,
}

impl DiskFull {
    /// [`RpcError`] details that [`RpcError::disk_full`] reads back.
    #[must_use]
    pub fn details(&self) -> Value {
        serde_json::json!({ DISK_FULL: self })
    }
}

impl DiskFull {
    /// The sentence naming `server`, such as "Server machine-3", for the Server.
    #[must_use]
    pub fn on(&self, server: &str) -> String {
        format!(
            "{server}'s disk is full: {} MB free under {}",
            self.free_bytes / 1_000_000,
            self.docker_root
        )
    }
}

impl fmt::Display for DiskFull {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.on("The Server"))
    }
}

/// A Machine refused a route because its Ployz lacks the route's capability.
#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityRefusal {
    pub target: String,
    pub capability: String,
    pub details: Value,
}

impl CapabilityRefusal {
    /// The refusal `details` name: `Unsupported`, with string `target` and `capability`.
    #[must_use]
    pub fn new(code: &RpcErrorCode, details: Value) -> Option<Self> {
        if *code != RpcErrorCode::Unsupported {
            return None;
        }
        let target = details.get("target")?.as_str()?.to_owned();
        let capability = details.get("capability")?.as_str()?.to_owned();
        Some(Self {
            target,
            capability,
            details,
        })
    }

    #[must_use]
    pub fn naming(self, target: String) -> Self {
        Self { target, ..self }
    }
}

impl fmt::Display for CapabilityRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Server {} needs a newer Ployz. It cannot serve {}.",
            self.target, self.capability
        )
    }
}

/// Metadata an entry Machine's router puts on a status it made because the target
/// Machine did not answer the forwarded call. A status the target sent never has it.
pub const TARGET_UNANSWERED_KEY: &str = "ployz-target-unanswered";

const TARGET_UNANSWERED_REASON: &str = "target_unanswered";
/// The `reason` an entry Machine's routing gives its not-found refusal of a Machine
/// Target it does not list, beside the `targets` it looked up. Read back by
/// [`RpcError::server_unlisted`].
pub const NO_SUCH_SERVER_REASON: &str = "no_such_server";
/// The details key an [`RpcError::overlapping_leave`] mark lives under.
pub const DEPARTING_KEY: &str = "departing";
const REPLY_LOST_KEY: &str = "reply_lost";
const REACHED_KEY: &str = "reached";
const UNDIALED_KEY: &str = "undialed";

/// [`caused_status`] as `Unavailable`, marked [`TARGET_UNANSWERED_KEY`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn target_unanswered_status(error: &(dyn std::error::Error + 'static)) -> tonic::Status {
    let mut status = caused_status(tonic::Code::Unavailable, error);
    status.metadata_mut().insert(
        TARGET_UNANSWERED_KEY,
        tonic::metadata::MetadataValue::from_static("1"),
    );
    status
}

/// Metadata an entry Machine's router adds to a [`TARGET_UNANSWERED_KEY`] status when its
/// dial to the target failed, so the target never got the call. Its absence says nothing.
pub const TARGET_UNREACHED_KEY: &str = "ployz-target-unreached";

/// [`target_unanswered_status`], also marked [`TARGET_UNREACHED_KEY`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn target_unreached_status(error: &(dyn std::error::Error + 'static)) -> tonic::Status {
    let mut status = target_unanswered_status(error);
    status.metadata_mut().insert(
        TARGET_UNREACHED_KEY,
        tonic::metadata::MetadataValue::from_static("1"),
    );
    status
}

/// The `reason` an entry Machine's router puts in the details of a status it made for a
/// target that did not answer, when its own path probes found that large packets to that
/// target are dropped. Read back by `ConnectError::large_packets_dropped` in the CLI.
pub const LARGE_PACKETS_DROPPED_REASON: &str = "large_packets_dropped";

/// `status`, a router's [`target_unanswered_status`], marked
/// [`LARGE_PACKETS_DROPPED_REASON`] with `sentence` as its deepest cause. Code, message
/// and metadata stay.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn large_packets_dropped_status(status: tonic::Status, sentence: String) -> tonic::Status {
    let mut details = match serde_json::from_slice::<Value>(status.details()) {
        Ok(Value::Object(details)) => details,
        _ => serde_json::Map::new(),
    };
    details.insert("reason".into(), LARGE_PACKETS_DROPPED_REASON.into());
    match details.get_mut("cause") {
        Some(Value::Array(causes)) => causes.push(sentence.into()),
        _ => {
            details.insert("cause".into(), Value::Array(vec![sentence.into()]));
        }
    }
    let details = serde_json::to_vec(&details).expect("a JSON object serializes");
    tonic::Status::with_details_and_metadata(
        status.code(),
        status.message(),
        details.into(),
        status.metadata().clone(),
    )
}

/// A gRPC status with `error`'s own sentence as the message and its sources in
/// the details as `{"cause": [...]}`, read back by [`status_causes`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn caused_status(
    code: tonic::Code,
    error: &(dyn std::error::Error + 'static),
) -> tonic::Status {
    let cause = crate::error_chain::causes(error);
    if cause.is_empty() {
        return tonic::Status::new(code, error.to_string());
    }
    let details = serde_json::to_vec(&serde_json::json!({ "cause": cause }))
        .expect("a list of strings serializes");
    tonic::Status::with_details(code, error.to_string(), details.into())
}

/// [`caused_status`] whose details also carry `hints`: an object keyed like an
/// [`RpcError`]'s details (`next`, `inspect`, `retry`), read back by [`status_details`].
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn hinted_status(
    code: tonic::Code,
    error: &(dyn std::error::Error + 'static),
    hints: Value,
) -> tonic::Status {
    let mut details = hints;
    if let Some(fields) = details.as_object_mut() {
        fields.insert("cause".into(), crate::error_chain::causes(error).into());
    }
    let details = serde_json::to_vec(&details).expect("a JSON value serializes");
    tonic::Status::with_details(code, error.to_string(), details.into())
}

/// The details a [`caused_status`] or [`hinted_status`] carried, without their
/// causes: `Null` when they carried none, `None` for any other details.
#[must_use]
pub fn status_details(details: &[u8]) -> Option<Value> {
    status_causes(details)?;
    let Ok(Value::Object(mut fields)) = serde_json::from_slice::<Value>(details) else {
        return None;
    };
    fields.remove("cause");
    Some(if fields.is_empty() {
        Value::Null
    } else {
        Value::Object(fields)
    })
}

/// The causes a [`caused_status`] carried, or `None` for any other details.
#[must_use]
pub fn status_causes(details: &[u8]) -> Option<Vec<String>> {
    #[derive(Deserialize)]
    struct Caused {
        cause: Vec<String>,
    }
    serde_json::from_slice::<Caused>(details)
        .ok()
        .map(|caused| caused.cause)
}

#[cfg(test)]
mod rpc_error_wire {
    use super::*;
    use serde_json::json;

    #[test]
    fn constructors_serialize_like_the_literals_they_replace() {
        let literal = |code: RpcErrorCode, details: Value, cause: Vec<String>| RpcError {
            code,
            message: "m".into(),
            details,
            cause,
        };
        let wire = |error: &RpcError| serde_json::to_string(error).unwrap();
        for (built, code) in [
            (
                RpcError::invalid_argument("m"),
                RpcErrorCode::InvalidArgument,
            ),
            (RpcError::not_found("m"), RpcErrorCode::NotFound),
            (RpcError::ambiguous("m"), RpcErrorCode::Ambiguous),
            (RpcError::unsupported("m"), RpcErrorCode::Unsupported),
            (RpcError::unavailable("m"), RpcErrorCode::Unavailable),
            (RpcError::conflict("m"), RpcErrorCode::Conflict),
            (RpcError::internal("m"), RpcErrorCode::Internal),
            (
                RpcError::unauthenticated("m"),
                RpcErrorCode::Unauthenticated,
            ),
            (
                RpcError::confirmation_required("m"),
                RpcErrorCode::ConfirmationRequired,
            ),
            (RpcError::forbidden("m"), RpcErrorCode::from("forbidden")),
            (
                RpcError::approval_required("m"),
                RpcErrorCode::from("approval_required"),
            ),
            (
                RpcError::new(RpcErrorCode::Conflict, "m"),
                RpcErrorCode::Conflict,
            ),
        ] {
            let expected = literal(code, Value::Null, Vec::new());
            assert_eq!(built, expected);
            assert_eq!(wire(&built), wire(&expected));
        }
        assert_eq!(
            wire(&RpcError::forbidden("m")),
            r#"{"code":"forbidden","message":"m","details":null}"#
        );

        let detailed = RpcError::conflict("m")
            .with_details(json!({ "k": 1 }))
            .with_causes(vec!["a".into(), "b".into()]);
        assert_eq!(
            wire(&detailed),
            wire(&literal(
                RpcErrorCode::Conflict,
                json!({ "k": 1 }),
                vec!["a".into(), "b".into()]
            ))
        );
        assert_eq!(
            RpcError::conflict("m").with_message("n"),
            RpcError {
                message: "n".into(),
                ..literal(RpcErrorCode::Conflict, Value::Null, Vec::new())
            }
        );
    }

    #[test]
    fn with_cause_lists_the_error_then_its_sources() {
        let inner = std::io::Error::other("disk");
        let outer = RpcError::caused(RpcErrorCode::Internal, &inner);
        assert_eq!(outer.cause, Vec::<String>::new());
        let chained = RpcError::internal("m").with_cause(&inner);
        assert_eq!(chained.cause, vec!["disk".to_owned()]);
    }

    #[test]
    fn rpc_error_round_trips_cause() {
        let error = RpcError {
            code: RpcErrorCode::Internal,
            message: "Could not create the Container.".into(),
            details: Value::Null,
            cause: vec!["Docker responded with status code 500: denied".into()],
        };
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(
            wire.get("cause"),
            Some(&json!(["Docker responded with status code 500: denied"]))
        );
        assert_eq!(serde_json::from_value::<RpcError>(wire).unwrap(), error);
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// A Leave's mark names its Server and leaves the rest of the failure as it was,
    /// and an unmarked Conflict reads back none.
    #[test]
    fn a_leave_marks_a_failure_without_changing_it() {
        let server = crate::MachineName::parse("lc-3").unwrap();
        let departure = Departure {
            server: server.clone(),
        };

        let failed = RpcError {
            code: RpcErrorCode::NotFound,
            message: "No such container".into(),
            details: json!({ "container": "b" }),
            cause: vec!["docker".into()],
        };
        let marked = failed.clone().overlapping_leave(&server);
        assert_eq!(marked.code, failed.code);
        assert_eq!(marked.message, failed.message);
        assert_eq!(marked.cause, failed.cause);
        assert_eq!(marked.details.get("container"), Some(&json!("b")));
        assert_eq!(marked.departure(), Some(departure));

        let held = RpcError {
            code: RpcErrorCode::Conflict,
            message: "slot held".into(),
            details: Value::Null,
            cause: Vec::new(),
        };
        assert_eq!(held.departure(), None);
    }

    #[test]
    fn a_status_carries_its_causes_in_the_details() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Machine did not answer".into(),
            details: Value::Null,
            cause: vec!["keep-alive timed out".into()],
        };
        let status = caused_status(tonic::Code::Unavailable, &error);
        assert_eq!(status.message(), "Machine did not answer");
        assert_eq!(
            status_causes(status.details()),
            Some(vec!["keep-alive timed out".to_owned()])
        );
        assert_eq!(status_causes(b"opaque"), None);
    }

    #[test]
    fn a_routing_refusal_names_the_server_it_does_not_list() {
        let machine = crate::MachineId::parse("1".repeat(32)).unwrap();
        let other = crate::MachineId::parse("2".repeat(32)).unwrap();
        let unlisted = RpcError {
            code: RpcErrorCode::NotFound,
            message: format!("No Server named {:?}", machine.to_string()),
            details: json!({
                "inspect": ["ployz server ls"],
                "reason": NO_SUCH_SERVER_REASON,
                "targets": [machine.to_string()],
            }),
            cause: Vec::new(),
        };
        assert!(unlisted.server_unlisted(&machine));
        assert!(!unlisted.server_unlisted(&other));
        // A daemon from before the tag answers the same refusal without it.
        let untagged = RpcError {
            details: json!({ "inspect": ["ployz server ls"] }),
            ..unlisted.clone()
        };
        assert!(!untagged.server_unlisted(&machine));
        let answered = RpcError {
            code: RpcErrorCode::Internal,
            ..unlisted
        };
        assert!(!answered.server_unlisted(&machine));
    }

    #[test]
    fn rpc_error_without_cause_parses() {
        let error = serde_json::from_value::<RpcError>(json!({
            "code": "conflict",
            "message": "busy",
            "details": null,
        }))
        .unwrap();
        assert!(error.cause.is_empty());
        assert!(serde_json::to_value(&error).unwrap().get("cause").is_none());
    }
}
