use super::*;

#[test]
fn a_request_key_is_8_to_64_safe_characters() {
    for key in ["abcdefgh", "A-b_9012", &"k".repeat(64)] {
        assert!(RequestKey::parse(key).is_ok(), "{key} is a key");
    }
    for key in [
        "abcdefg",
        &"k".repeat(65),
        "has space",
        "slash/es-1",
        "dots.are.out",
    ] {
        assert!(RequestKey::parse(key).is_err(), "{key} is not a key");
    }
}

#[test]
fn a_request_key_decodes_only_when_it_parses() {
    assert!(serde_json::from_value::<RequestKey>(serde_json::json!("short")).is_err());
    let key = serde_json::from_value::<RequestKey>(serde_json::json!("deploy-0001")).unwrap();
    assert_eq!(key.as_str(), "deploy-0001");
}

#[test]
fn the_effective_key_is_the_given_one_else_stable_per_call_key() {
    let given = RequestKey::parse("mine-0001").unwrap();
    assert_eq!(
        RequestKey::effective(Some(given), "call-1").as_str(),
        "mine-0001"
    );
    let first = RequestKey::effective(None, "call-1");
    assert_eq!(first, RequestKey::effective(None, "call-1"));
    assert_ne!(first, RequestKey::effective(None, "call-2"));
    assert!(RequestKey::parse(first.as_str()).is_ok());
}

#[test]
fn accepted_loss_needs_volumes_and_the_version() {
    assert!(matches!(
        AcceptedLoss::from_input(Vec::new(), None),
        Ok(None)
    ));
    assert!(matches!(
        AcceptedLoss::from_input(Vec::new(), Some("v1".to_owned())),
        Ok(None)
    ));
    let Err(Stop::Refuse(refusal)) = AcceptedLoss::from_input(vec!["data".to_owned()], None) else {
        panic!("loss without a version is refused");
    };
    assert_eq!(refusal.field.as_deref(), Some("version"));
    let Ok(Some(loss)) = AcceptedLoss::from_input(vec!["data".to_owned()], Some("v1".to_owned()))
    else {
        panic!("loss with a version is accepted");
    };
    assert_eq!(
        (loss.volumes, loss.version),
        (vec!["data".to_owned()], "v1".to_owned())
    );
}

#[test]
fn only_a_gated_tool_says_it_may_ask_a_human() {
    assert_eq!(Gate::Never.sentence(), "Never asks a human to approve.");
    assert_eq!(
        Gate::Store.sentence(),
        "May stop to ask a human to approve."
    );
    assert_eq!(Gate::Store.sentence(), Gate::Cloud.sentence());
}

#[test]
fn a_body_digest_ignores_key_order_wait_and_request_key() {
    let one = BodyDigest::of(
        "deploy",
        &serde_json::json!({ "a": 1, "b": { "y": 2, "x": 3 } }),
    );
    let same = BodyDigest::of(
        "deploy",
        &serde_json::json!({ "b": { "x": 3, "y": 2 }, "a": 1, "wait_seconds": 9, "request_key": "deploy-0001" }),
    );
    assert_eq!(one, same);
    assert_ne!(
        one,
        BodyDigest::of(
            "deploy",
            &serde_json::json!({ "a": 2, "b": { "x": 3, "y": 2 } })
        )
    );
    assert_ne!(
        one,
        BodyDigest::of(
            "review",
            &serde_json::json!({ "a": 1, "b": { "x": 3, "y": 2 } })
        )
    );
}

#[test]
fn an_attempt_is_derived_from_tool_and_key() {
    let key = RequestKey::parse("deploy-0001").unwrap();
    let other = RequestKey::parse("deploy-0002").unwrap();
    assert_eq!(
        AttemptId::derive("deploy", &key),
        AttemptId::derive("deploy", &key)
    );
    assert_ne!(
        AttemptId::derive("deploy", &key),
        AttemptId::derive("deploy", &other)
    );
    assert_ne!(
        AttemptId::derive("deploy", &key),
        AttemptId::derive("review", &key)
    );
}

#[test]
fn a_cloud_call_refuses_unknown_fields_at_any_depth() {
    let key = RequestKey::parse("drain-0001").unwrap();
    let act = serde_json::json!({
        "attempt": AttemptId::derive("server_drain", &key).to_string(),
        "digest": "00".repeat(32),
        "action": { "action": "drain", "server": { "machine": "a".repeat(32), "name": "alpha" } },
    });
    assert!(serde_json::from_value::<Act>(act.clone()).is_ok());
    for path in ["", "/action", "/action/server"] {
        let mut extra = act.clone();
        extra
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("dry_run".into(), true.into());
        assert!(
            serde_json::from_value::<Act>(extra).is_err(),
            "unknown field at `{path}`"
        );
    }
    let query = serde_json::json!({ "query": "servers", "only": null, "dry_run": true });
    assert!(serde_json::from_value::<CloudQuery>(query).is_err());
}
