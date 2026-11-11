use ployz_core::authority::AuthorityRefusal;
use serde_json::json;

use super::*;

fn build(service: &str, commit: Option<&str>) -> BuildView {
    serde_json::from_value(json!({
        "service": service,
        "commit": commit,
        "status": "built",
        "message": null,
    }))
    .unwrap()
}

fn redeploy(commits: &[(&str, &str)]) -> Redeploy {
    Redeploy {
        signer: Signer::from_seed(&[7; 32]),
        terms: Terms {
            cluster: SealingPublicKey::from_bytes([3; 32]),
            rule: RuleId::from_bytes([9; 16]),
            generation: 4,
        },
        commits: commits
            .iter()
            .map(|(service, commit)| {
                (
                    ServiceName::parse(*service).unwrap(),
                    CommitSha::parse(commit).unwrap(),
                )
            })
            .collect(),
        approvals: Vec::new(),
    }
}

#[test]
fn each_commit_is_approved_under_the_terms_and_for_no_longer_than_a_server_accepts() {
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let redeploy = redeploy(&[("web", &a), ("api", &b)]);
    let bodies = redeploy.bodies(Timestamp::now());
    let named: Vec<_> = bodies
        .iter()
        .map(|body| (body.service.as_str().to_owned(), body.commit.to_string()))
        .collect();
    assert_eq!(named, [("api".to_owned(), b), ("web".to_owned(), a)]);
    for body in &bodies {
        assert_eq!(body.cluster, SealingPublicKey::from_bytes([3; 32]));
        assert_eq!(
            (body.rule, body.generation),
            (RuleId::from_bytes([9; 16]), 4)
        );
        assert_eq!(
            body.expires_at.as_millis() - body.issued_at.as_millis(),
            validity::MAX_LIFETIME_MILLIS
        );
        assert_eq!(
            validity::verify(body.issued_at, body.expires_at, body.issued_at),
            Ok::<(), AuthorityRefusal>(())
        );
        assert_eq!(
            body.signer,
            SignerKey::Device(Signer::from_seed(&[7; 32]).public().into())
        );
    }
}

#[test]
fn an_uploaded_build_has_no_commit_to_approve() {
    let error = built_from(&build("web", None)).unwrap_err();
    assert_eq!(error.report().code, RpcErrorCode::InvalidArgument);
    let commit = "c".repeat(40);
    assert_eq!(
        built_from(&build("web", Some(&commit))).unwrap(),
        CommitSha::parse(&commit).unwrap()
    );
}

#[test]
fn the_person_sees_every_term_of_what_this_device_signs() {
    let a = "a".repeat(40);
    let b = "0123456789abcdef0123456789abcdef01234567".to_owned();
    let redeploy = redeploy(&[("web", &a), ("api", &b)]);
    // 2026-11-11T03:19:09.250Z
    let bodies = redeploy.bodies(Timestamp::from_millis(1_794_367_149_250).unwrap());
    let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public()).code();
    let cluster = SealingPublicKey::from_bytes([3; 32]);
    let lines: Vec<String> = shown(&bodies).into_iter().map(String::from).collect();
    assert_eq!(
        lines,
        [
            "Approving commits for this redeploy:".to_owned(),
            format!("  api at {b}"),
            format!("  web at {a}"),
            "  rule 09090909090909090909090909090909, generation 4".to_owned(),
            // Base64 ends in `=`, which a plain token never holds, so it shows quoted.
            format!("  cluster \"{cluster}\""),
            format!("  signer device {device}"),
            "  valid 2026-11-11T03:19:09Z to 2026-11-11T03:34:09Z".to_owned(),
        ]
    );
    // What it signs is what it showed, with nothing added.
    let signed = redeploy.sign(&bodies).unwrap();
    let opened: Vec<_> = signed.iter().map(|signed| signed.open().unwrap()).collect();
    assert_eq!(opened, bodies);
}

#[test]
fn an_answer_after_the_approval_expired_signs_nothing() {
    let redeploy = redeploy(&[("web", &"a".repeat(40))]);
    let issued_at = Timestamp::from_millis(1_794_367_149_250).unwrap();
    let bodies = redeploy.bodies(issued_at);
    assert!(live(&bodies, issued_at.plus(validity::MAX_LIFETIME_MILLIS - 1)).is_ok());
    let late = live(&bodies, issued_at.plus(validity::MAX_LIFETIME_MILLIS)).unwrap_err();
    assert_eq!(late.report().code, RpcErrorCode::Conflict);
}
