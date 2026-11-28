#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use serde_json::{Value, json};

use super::*;
use crate::authority::Timestamp;
use crate::authority::keys::Signer;
use crate::authority::rule::{Builder, RuleId};
use crate::authority::rule_draft::RuleTerms;
use crate::{MachineId, SealingPublicKey};

const PARENT: &str = "11111111-1111-4111-8111-111111111111";
const SAMPLE: &str = "pv-0000000000000000-01020304-00000000";

fn spec(name: &str, image: &str) -> Value {
    json!({
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": image, "pull_policy": "never"},
    })
}

fn view(db_image: &str) -> Value {
    json!({
        "parent": PARENT,
        "parent_name": "staging",
        "namespace": "shop-staging",
        "sample": SAMPLE,
        "rule": null,
        "sources": [{
            "service": "web",
            "repository": "acme/web",
            "repository_id": 11,
            "branch": "staging",
            "root_dir": ".",
            "build": {"buildMethod": "dockerfile", "dockerfilePath": "Dockerfile"}
        }],
        "images": [{"service": "db", "image": db_image}],
        "target": [spec("web", "ployz-build/web"), spec("db", db_image)],
    })
}

fn basis(db_image: &str) -> PreviewBasis {
    serde_json::from_value(view(db_image)).unwrap()
}

const PINNED: &str =
    "postgres@sha256:0000000000000000000000000000000000000000000000000000000000000001";

fn terms(new_rule: u8) -> RuleTerms {
    let signer = Signer::from_seed(&[7; 32]);
    RuleTerms {
        cluster: SealingPublicKey::from_bytes([3; 32]),
        machines: vec![MachineId::parse("a".repeat(32)).unwrap()],
        owner_id: 1,
        builder: Builder::pinned(),
        platform: "linux/amd64".to_owned(),
        new_rule: RuleId::from_bytes([new_rule; 16]),
        issued_at: Timestamp::from_millis(1_800_000_000_000).unwrap(),
        signer: crate::authority::SignerKey::Device(crate::DeviceKey::from_bytes(
            *signer.public().as_bytes(),
        )),
    }
}

#[test]
fn an_image_a_preview_runs_must_name_its_digest_before_anyone_signs() {
    let refused = AutoDeployRule::draft_preview(&basis("postgres:16"), terms(9)).unwrap_err();
    assert!(
        matches!(refused, RuleDraftError::Unpinned { ref service, .. } if service.as_str() == "db"),
        "{refused}"
    );
    let rule = AutoDeployRule::draft_preview(&basis(PINNED), terms(9)).unwrap();
    let preview = rule.preview.as_ref().unwrap();
    assert_eq!(preview.parent.to_string(), PARENT);
    assert!(preview.pushers.is_empty());
    assert_eq!(preview.pinned[0].name.as_str(), "db");
    assert_eq!(rule.services[0].name.as_str(), "web");
    assert_eq!(
        rule.build_policy(&rule.services[0].name).unwrap().cache,
        PolicyCache::Off
    );
}

#[test]
fn a_kept_preview_rule_for_the_same_parent_signs_its_next_generation() {
    let signer = Signer::from_seed(&[7; 32]);
    let first = AutoDeployRule::draft_preview(&basis(PINNED), terms(9)).unwrap();
    let kept = Signed::sign(&first, &signer).unwrap();
    let mut with = view(PINNED);
    with["rule"] = serde_json::to_value(&kept).unwrap();
    let next =
        AutoDeployRule::draft_preview(&serde_json::from_value(with.clone()).unwrap(), terms(5))
            .unwrap();
    assert_eq!((next.rule, next.generation), (first.rule, 2));
    with["parent"] = json!("22222222-2222-4222-8222-222222222222");
    let other =
        AutoDeployRule::draft_preview(&serde_json::from_value(with).unwrap(), terms(5)).unwrap();
    assert_eq!(
        (other.rule, other.generation),
        (RuleId::from_bytes([5; 16]), 1)
    );
}
