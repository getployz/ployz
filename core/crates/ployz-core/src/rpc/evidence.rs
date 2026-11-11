//! Typed readers and writers for [`RpcError::details`]: the evidence a refusal carries as
//! its body, and the marks a caller merges into details a peer already sent.
//!
//! Details are frozen wire JSON, so each type here pins its keys in one place and
//! `tests/rpc_details_golden.rs` pins the bytes. A value of the wrong type or shape reads
//! as absent, as every hand-written reader did.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use ts_rs::TS;

use super::{DiskFull, RpcError, RpcErrorCode};
use crate::{MachineId, PullPolicy};

/// A typed body of [`RpcError::details`]: the whole value when [`Self::KEY`] is `None`,
/// else the value under that key.
pub trait Evidence: Serialize + DeserializeOwned {
    const KEY: Option<&'static str>;

    /// The `details` that carry this evidence.
    ///
    /// # Panics
    ///
    /// If the evidence does not serialize to JSON, which no evidence type allows.
    #[must_use]
    fn to_details(&self) -> Value {
        let body = serde_json::to_value(self).expect("evidence is JSON serializable");
        match Self::KEY {
            None => body,
            Some(key) => Value::Object(Map::from_iter([(key.to_owned(), body)])),
        }
    }
}

/// Evidence that also fixes the code of the error that carries it.
pub trait CodedRefusal: Evidence {
    fn code(&self) -> RpcErrorCode;
}

/// A fact about a failed call that a caller merges into the details it got back, without
/// changing the rest of them. Details that are not an object are left unmarked.
pub trait Mark: Sized {
    /// Whether marking `Null` details makes them an object holding the mark.
    const CREATES_OBJECT: bool;

    fn insert(&self, fields: &mut Map<String, Value>);

    /// The mark `details` carry, if any.
    fn read(details: &Value) -> Option<Self>;
}

impl RpcError {
    /// `self` with `evidence` as its details.
    #[must_use]
    pub fn with_evidence<E: Evidence>(self, evidence: &E) -> Self {
        self.with_details(evidence.to_details())
    }

    /// An error coded and detailed by `refusal`.
    #[must_use]
    pub fn refused<R: CodedRefusal>(refusal: &R, message: impl Into<String>) -> Self {
        Self::new(refusal.code(), message).with_evidence(refusal)
    }

    /// The evidence the details carry, if they carry `E`. The caller still decides by verb
    /// or code whether it is the evidence it asked for: `reason` names overlap across types.
    #[must_use]
    pub fn evidence<E: Evidence>(&self) -> Option<E> {
        let body = match E::KEY {
            None => &self.details,
            Some(key) => self.details.get(key)?,
        };
        E::deserialize(body).ok()
    }

    /// `self` with `mark` merged into its details.
    #[must_use]
    pub fn mark<M: Mark>(mut self, mark: &M) -> Self {
        if M::CREATES_OBJECT && self.details.is_null() {
            self.details = Value::Object(Map::new());
        }
        if let Value::Object(fields) = &mut self.details {
            mark.insert(fields);
        }
        self
    }

    /// The mark the details carry, if they carry `M`.
    #[must_use]
    pub fn marked<M: Mark>(&self) -> Option<M> {
        M::read(&self.details)
    }
}

/// The Leave that overlapped a call: what a Server's Containers did since it began is
/// the Leave's doing. Annotates a failure's details under [`DEPARTING_KEY`], an unhealthy
/// `HealthVerdict`, and the `ExecutionError::Health` a deploy stores.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Departure {
    pub server: crate::MachineName,
}

/// The details key a [`Departure`] mark lives under.
pub const DEPARTING_KEY: &str = "departing";

impl Mark for Departure {
    const CREATES_OBJECT: bool = true;

    fn insert(&self, fields: &mut Map<String, Value>) {
        fields.insert(
            DEPARTING_KEY.into(),
            serde_json::to_value(self).expect("a departure is JSON serializable"),
        );
    }

    fn read(details: &Value) -> Option<Self> {
        Self::deserialize(details.get(DEPARTING_KEY)?).ok()
    }
}

/// The connection dropped after the call went out and before an answer came back, so the
/// target may have done the work. The first `machine_id` a failure names stays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyLost {
    pub machine_id: Option<MachineId>,
}

impl Mark for ReplyLost {
    const CREATES_OBJECT: bool = true;

    fn insert(&self, fields: &mut Map<String, Value>) {
        fields.insert("reply_lost".into(), Value::Bool(true));
        if let Some(machine_id) = &self.machine_id {
            fields
                .entry("machine_id")
                .or_insert_with(|| Value::String(machine_id.to_string()));
        }
    }

    fn read(details: &Value) -> Option<Self> {
        (details.get("reply_lost").and_then(Value::as_bool) == Some(true)).then(|| Self {
            machine_id: details
                .get("machine_id")
                .and_then(Value::as_str)
                .and_then(|id| MachineId::parse(id).ok()),
        })
    }
}

/// The call never reached its target, so the target did nothing with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unreached;

impl Mark for Unreached {
    const CREATES_OBJECT: bool = true;

    fn insert(&self, fields: &mut Map<String, Value>) {
        fields.insert("reached".into(), Value::Bool(false));
    }

    fn read(details: &Value) -> Option<Self> {
        (details.get("reached").and_then(Value::as_bool) == Some(false)).then_some(Self)
    }
}

/// A router could not dial the target of an unanswered call, so only small packets went
/// its way. It marks an unanswered-target error and never makes details of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Undialed;

impl Mark for Undialed {
    const CREATES_OBJECT: bool = false;

    fn insert(&self, fields: &mut Map<String, Value>) {
        fields.insert("undialed".into(), Value::Bool(true));
    }

    fn read(details: &Value) -> Option<Self> {
        (details.get("undialed").and_then(Value::as_bool) == Some(true)).then_some(Self)
    }
}

/// An image a pull answered not found or refused for, with the policy it was pulled
/// under when the error recorded one. Read back by [`RpcError::missing_image`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingImage {
    #[serde(rename = "missing_image")]
    pub image: String,
    #[serde(
        rename = "pull_policy",
        default,
        deserialize_with = "policy_or_none",
        skip_serializing_if = "Option::is_none"
    )]
    pub policy: Option<PullPolicy>,
}

fn policy_or_none<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PullPolicy>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(PullPolicy::deserialize(value).ok())
}

impl Evidence for MissingImage {
    const KEY: Option<&'static str> = None;
}

impl Evidence for DiskFull {
    const KEY: Option<&'static str> = Some("disk_full");
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Whole {
        reason: String,
    }

    impl Evidence for Whole {
        const KEY: Option<&'static str> = None;
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Keyed {
        One,
    }

    impl Evidence for Keyed {
        const KEY: Option<&'static str> = Some("keyed");
    }

    #[test]
    fn evidence_reads_the_whole_body_or_the_value_under_its_key() {
        let whole = Whole { reason: "r".into() };
        let error = RpcError::conflict("m").with_evidence(&whole);
        assert_eq!(error.details, json!({ "reason": "r" }));
        assert_eq!(error.evidence::<Whole>(), Some(whole));
        assert_eq!(error.evidence::<Keyed>(), None);

        let error = RpcError::conflict("m").with_evidence(&Keyed::One);
        assert_eq!(error.details, json!({ "keyed": "one" }));
        assert_eq!(error.evidence::<Keyed>(), Some(Keyed::One));
        assert_eq!(
            RpcError::conflict("m")
                .with_details(json!({ "keyed": "two" }))
                .evidence::<Keyed>(),
            None
        );
        assert_eq!(
            RpcError::conflict("m")
                .with_details(json!("keyed"))
                .evidence::<Keyed>(),
            None
        );
    }
}
