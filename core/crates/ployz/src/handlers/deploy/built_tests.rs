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
    let signed = redeploy.signed().unwrap();
    let bodies: Vec<_> = signed.iter().map(|signed| signed.open().unwrap()).collect();
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
