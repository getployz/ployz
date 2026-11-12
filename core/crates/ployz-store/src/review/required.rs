//! What an `approval_required` refusal says: the shape Cloud, the Store and the CLI send
//! and read, so none of them spells its keys by hand.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::review::DestructiveEffect;

/// The operations Cloud gates with a human's approval, named as Cloud names them. An
/// operation this list lacks leaves its refusal unasked, so adding one here and to
/// Cloud's `OperationVerb` comes first.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum OperationVerb {
    Drain,
    Set,
    Clean,
    Remove,
    Forget,
    Release,
    RemoveMirror,
    Stop,
    Restart,
    Unprotect,
    Move,
    Restore,
    Resize,
    SetClusterName,
    SetIngressDoors,
    SetPrivateAccess,
}

/// An operation as a human approves it: its verb, what it acts on by name, and the
/// preview its digest covers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalOperation {
    pub verb: OperationVerb,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Value>,
}

impl ApprovalOperation {
    /// Whether approving it only gives up access to a Volume: a reset keeps Volume data on
    /// the Server's disk, and a forget is for a Server that may be gone.
    #[must_use]
    pub fn loses_volume_access(&self) -> bool {
        self.verb == OperationVerb::Remove
            && self
                .subject
                .as_deref()
                .is_some_and(|subject| subject.starts_with("server:"))
    }

    /// The Cluster name a rename moves away from, when its preview says.
    #[must_use]
    pub fn renamed_from(&self) -> Option<&str> {
        self.preview
            .as_ref()
            .and_then(|preview| preview.get("from"))
            .and_then(Value::as_str)
    }
}

/// What the approval covers: a Store write, which carries its diff, or an operation on one
/// named thing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSubject<Diff> {
    Diff(Diff),
    Operation(ApprovalOperation),
}

/// A redeploy's Deployment, by number, and the later ones it rolls back.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRedeploy {
    pub deployment: u64,
    #[serde(default)]
    pub rolls_back: Vec<u64>,
}

/// The details of an `approval_required` refusal. `Diff` is how the refusal holds the
/// review it asks about: the Store's own, or a removal's, or a boxed one the CLI reads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequired<Diff> {
    /// Cloud's record of the ask; the Store, which keeps none, sends no `approval_id`.
    #[serde(
        rename = "approval_id",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<String>,
    /// The `version:digest` to approve.
    #[serde(rename = "approval")]
    pub digest: String,
    pub effects: Vec<DestructiveEffect>,
    #[serde(flatten)]
    pub subject: ApprovalSubject<Diff>,
    /// The Organization an operation acts in, which can differ from the one this device
    /// last acted in once a browser has moved the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization: Option<String>,
    /// Set when a Protected Environment asks someone who can't protect it: only another
    /// person, an owner or admin, can approve, so even a terminal waits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected: Option<Value>,
    /// Set when the Deploy also removes every other Service running in its
    /// Environment, which only the Servers can name.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prunes: bool,
    /// The Servers that didn't answer because large packets to them are dropped, so
    /// a prune may remove what they run unnamed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dropped: Vec<String>,
    /// Set on a redeploy: its Deployment's number and the later ones it rolls back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redeploy: Option<ApprovalRedeploy>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ApprovalSubject, OperationVerb};

    #[test]
    fn the_verbs_cloud_names_keep_their_text() {
        for (verb, text) in [
            (OperationVerb::Drain, "drain"),
            (OperationVerb::Set, "set"),
            (OperationVerb::Clean, "clean"),
            (OperationVerb::Remove, "remove"),
            (OperationVerb::Forget, "forget"),
            (OperationVerb::Release, "release"),
            (OperationVerb::RemoveMirror, "remove_mirror"),
            (OperationVerb::Stop, "stop"),
            (OperationVerb::Restart, "restart"),
            (OperationVerb::Unprotect, "unprotect"),
            (OperationVerb::Move, "move"),
            (OperationVerb::Restore, "restore"),
            (OperationVerb::Resize, "resize"),
            (OperationVerb::SetClusterName, "set_cluster_name"),
            (OperationVerb::SetIngressDoors, "set_ingress_doors"),
            (OperationVerb::SetPrivateAccess, "set_private_access"),
        ] {
            assert_eq!(serde_json::to_value(verb).unwrap(), json!(text));
            assert_eq!(
                serde_json::from_value::<OperationVerb>(json!(text)).unwrap(),
                verb
            );
        }
        for unread in [
            "publish",
            "deploy",
            "retry",
            "Restart",
            "set-cluster-name",
            "",
        ] {
            assert!(
                serde_json::from_value::<OperationVerb>(json!(unread)).is_err(),
                "{unread}"
            );
        }
    }

    #[test]
    fn a_subject_is_a_diff_or_an_operation_by_its_key() {
        let operation: ApprovalSubject<serde_json::Value> =
            serde_json::from_value(json!({ "operation": { "verb": "stop", "name": "web" } }))
                .unwrap();
        assert!(matches!(operation, ApprovalSubject::Operation(_)));
        let diff: ApprovalSubject<serde_json::Value> =
            serde_json::from_value(json!({ "diff": { "any": 1 } })).unwrap();
        assert_eq!(diff, ApprovalSubject::Diff(json!({ "any": 1 })));
    }
}
