use std::collections::BTreeSet;

use serde_json::json;

use super::{Target, gate};
use crate::digest::short_digest;
use crate::id::{ApprovalDigest, EnvironmentId, OrganizationId, ProjectId};
use crate::review::DestructiveEffect;
use crate::{Actor, Approval};

const VERSION: &str = "7:1:0.2";

fn who() -> Actor {
    Actor::system(OrganizationId::parse("org").unwrap())
}

fn production() -> EnvironmentId {
    EnvironmentId::parse("00000000-0000-4000-8000-000000000002").unwrap()
}

fn effects() -> BTreeSet<DestructiveEffect> {
    let web = json!({
        "kind": "removes_service",
        "name": "web",
        "node": "00000000-0000-4000-8000-000000000003",
        "path": "web",
    });
    BTreeSet::from([serde_json::from_value(web).unwrap()])
}

/// The digest `command` asks a human to approve for removing production's `web`.
fn asked(command: &str, approval: &Approval) -> Result<(), ployz_core::RpcError> {
    gate(
        &who(),
        command,
        &Target::Environment(production()),
        VERSION,
        effects(),
        approval,
        || json!({}),
    )
}

fn digest_of(refused: ployz_core::RpcError) -> ApprovalDigest {
    assert_eq!(refused.code.as_str(), "approval_required", "{refused:?}");
    serde_json::from_value(refused.details.get("approval").cloned().unwrap()).unwrap()
}

#[test]
fn a_removal_and_a_shutdown_of_one_environment_ask_for_different_approvals() {
    let removal = digest_of(asked("remove", &Approval::Required).unwrap_err());
    let shutdown = digest_of(asked("shut_down", &Approval::Required).unwrap_err());
    assert_ne!(removal, shutdown);

    asked("remove", &Approval::Approved(removal.clone())).unwrap();
    let carried = digest_of(asked("shut_down", &Approval::Approved(removal)).unwrap_err());
    assert_eq!(carried, shutdown);
}

#[test]
fn an_approval_from_before_the_command_was_bound_asks_again() {
    let bound = json!({
        "organization": who().organization,
        "environment": production(),
        "effects": effects(),
    });
    let before =
        ApprovalDigest::parse(format!("{VERSION}:{}", short_digest(&bound.to_string()))).unwrap();
    let fresh = digest_of(asked("remove", &Approval::Approved(before.clone())).unwrap_err());
    assert_ne!(fresh, before);
    asked("remove", &Approval::Approved(fresh)).unwrap();
}

/// Cloud holds a digest between ask and approve, so what a digest is over, and its
/// text, are frozen: any drift refuses every approval waiting on a human.
#[test]
fn the_digests_a_human_is_asked_to_approve_keep_their_text() {
    assert_eq!(short_digest("{}"), "44136fa355b3678a");

    let environment = digest_of(asked("remove", &Approval::Required).unwrap_err());
    assert_eq!(environment.as_str(), "7:1:0.2:f205f84b319fe5b1");

    let project = ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap();
    let refused = gate(
        &who(),
        "remove",
        &Target::Project(project),
        VERSION,
        effects(),
        &Approval::Required,
        || json!({}),
    )
    .unwrap_err();
    assert_eq!(digest_of(refused).as_str(), "7:1:0.2:a0a17c2d0b878d7a");
}
