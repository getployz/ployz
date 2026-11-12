use std::collections::BTreeSet;

use ployz_core::MachineName;
use serde_json::json;

use super::{Gate, Pruning};
use crate::id::ApprovalDigest;
use crate::review::{DestructiveEffect, DiffView};
use crate::{Approval, Trusted};

fn diff() -> DiffView {
    serde_json::from_value(json!({
        "environment": {
            "id": "00000000-0000-4000-8000-000000000002",
            "project": "shop",
            "name": "production",
            "revision": 3,
        },
        "version": "3:2:0.1",
        "saved": 2,
        "holds_staged": false,
        "changes": [],
        "total_count": 1,
    }))
    .unwrap()
}

fn effects() -> BTreeSet<DestructiveEffect> {
    BTreeSet::from([serde_json::from_value(json!({
        "kind": "removes_service",
        "name": "web",
        "node": "00000000-0000-4000-8000-000000000003",
        "path": "web",
    }))
    .unwrap()])
}

fn asked(protected: bool, pruning: &Pruning) -> ployz_core::RpcError {
    let trusted = Trusted {
        approval: Approval::Required,
        ..Trusted::default()
    };
    let gate = Gate {
        trusted: &trusted,
        protected,
        exempt: false,
        spares_harmless: false,
    };
    let effects = effects();
    gate.ask(
        json!({ "command": "deploy" }),
        "A human must approve this first".to_owned(),
        (&effects, &diff()),
    )
    .map_err(|asking| pruning.unnamed(asking).refusal())
    .unwrap_err()
}

const DIGEST: &str = "3:2:0.1:cc839b623689e205";

/// Cloud keeps the details it stored and the CLI reads them back as they were sent, so the
/// keys of what the Store refuses with are frozen.
#[test]
fn an_ask_keeps_its_details() {
    let ask = asked(false, &Pruning::Off);
    assert_eq!(ask.code.as_str(), "approval_required");
    assert_eq!(ask.message, "A human must approve this first");
    assert_eq!(
        ask.details.to_string(),
        format!(
            r#"{{"approval":"{DIGEST}","diff":{DIFF},"effects":[{EFFECT}]}}"#,
            DIFF = DIFF_TEXT,
            EFFECT = EFFECT_TEXT,
        ),
    );
}

#[test]
fn a_protected_ask_says_so() {
    let ask = asked(true, &Pruning::Off);
    let details = ask.details.as_object().unwrap();
    assert_eq!(
        details.keys().collect::<Vec<_>>(),
        ["approval", "diff", "effects", "protected"]
    );
    assert_eq!(details["protected"], json!(true));
    assert_ne!(details["approval"], json!(DIGEST));
}

#[test]
fn a_prune_that_may_remove_what_nothing_names_says_so() {
    let unseen = Pruning::On {
        removes: BTreeSet::new(),
        unseen: true,
        dropped: Vec::new(),
    };
    let ask = asked(false, &unseen);
    let details = ask.details.as_object().unwrap();
    assert_eq!(
        details.keys().collect::<Vec<_>>(),
        ["approval", "diff", "effects", "prunes"]
    );
    assert_eq!(details["prunes"], json!(true));

    let dropped = Pruning::On {
        removes: BTreeSet::new(),
        unseen: true,
        dropped: vec![MachineName::parse("box-1").unwrap()],
    };
    let ask = asked(true, &dropped);
    let details = ask.details.as_object().unwrap();
    assert_eq!(
        details.keys().collect::<Vec<_>>(),
        [
            "approval",
            "diff",
            "dropped",
            "effects",
            "protected",
            "prunes"
        ]
    );
    assert_eq!(details["dropped"], json!(["box-1"]));

    let named = Pruning::On {
        removes: effects(),
        unseen: false,
        dropped: Vec::new(),
    };
    assert_eq!(asked(false, &named).details.get("prunes"), None);
}

#[test]
fn the_digest_a_human_approves_keeps_its_text() {
    let ask = asked(false, &Pruning::Off);
    let digest: ApprovalDigest =
        serde_json::from_value(ask.details.get("approval").cloned().unwrap()).unwrap();
    assert_eq!(digest.as_str(), DIGEST);
}

const EFFECT_TEXT: &str = r#"{"kind":"removes_service","name":"web","node":"00000000-0000-4000-8000-000000000003","path":"web"}"#;
const DIFF_TEXT: &str = r#"{"broken":[],"changes":[],"draft_changes":[],"draft_count":0,"effects":[],"environment":{"id":"00000000-0000-4000-8000-000000000002","name":"production","project":"shop","revision":3},"follow_hints":[],"holds_staged":false,"included":[],"incoming":[],"saved":2,"setup":[],"total_count":1,"version":"3:2:0.1"}"#;
