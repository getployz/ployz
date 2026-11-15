use super::{message, public_error};

#[test]
fn a_refusal_sentence_reads_the_same_from_every_envelope_cloud_uses() {
    for (body, said) in [
        (
            r#"{"_tag":"PublicError","code":"CONFLICT","message":"Taken."}"#,
            Some("Taken."),
        ),
        (
            r#"{"_tag":"PublicError","code":"x","message":""}"#,
            Some(""),
        ),
        (r#"{"_tag":"PublicError","message":"m"}"#, None),
        (r#"{"_tag":"PublicError","code":"c"}"#, None),
        (r#"{"_tag":"PublicError","code":7,"message":"m"}"#, None),
        (r#"{"_tag":"PublicError","code":"c","message":7}"#, None),
        (
            r#"{"_tag":"PublicError","code":"c","message":"m","error":"e"}"#,
            Some("m"),
        ),
        (r#"{"_tag":"PublicError","error":"e"}"#, None),
        (r#"{"_tag":"Other","error":"e"}"#, Some("e")),
        (r#"{"_tag":7,"error":"e"}"#, Some("e")),
        (r#"{"error":"Busy, retry."}"#, Some("Busy, retry.")),
        (r#"{"error":""}"#, Some("")),
        (r#"{"error":"e","code":"busy"}"#, Some("e")),
        (r#"{"error":{"code":"c","message":"m"}}"#, Some("m")),
        (
            r#"{"error":{"code":"c","message":"m","details":null}}"#,
            Some("m"),
        ),
        (r#"{"error":{"message":"m"}}"#, None),
        (r#"{"error":{"code":"c"}}"#, None),
        (r#"{"error":{"code":1,"message":"m"}}"#, None),
        (r#"{"error":{"code":"c","message":null}}"#, None),
        (r#"{"error":null}"#, None),
        (r#"{"error":7}"#, None),
        (r#"{"error":["e"]}"#, None),
        (r#"{"message":"m"}"#, None),
        ("{}", None),
        ("[]", None),
        (r#""e""#, None),
        ("", None),
        ("not json", None),
        (r#"{"error":"e"} trailing"#, None),
        ("\u{feff}{\"error\":\"e\"}", None),
        (" {\"error\":\"e\"}\n", Some("e")),
    ] {
        assert_eq!(message(body.as_bytes()).as_deref(), said, "{body}");
    }
}

#[test]
fn a_route_s_public_error_reads_as_the_code_it_stands_for() {
    let public = |code: &str| {
        format!(r#"{{"_tag":"PublicError","code":"{code}","message":"Said."}}"#).into_bytes()
    };
    for (code, rpc) in [
        ("CONFLICT", "conflict"),
        ("NOT_FOUND", "not_found"),
        ("VALIDATION_FAILED", "invalid_argument"),
        ("UNAUTHORIZED", "unauthenticated"),
        ("FORBIDDEN", "unauthenticated"),
        ("RATE_LIMITED", "unavailable"),
        ("INTERNAL", "internal"),
        ("", "internal"),
        ("conflict", "internal"),
        ("forbidden", "internal"),
        ("Conflict", "internal"),
        ("CONFLICT ", "internal"),
        ("busy", "internal"),
    ] {
        let refused = public_error(&public(code)).unwrap();
        assert_eq!(refused.code.as_str(), rpc, "{code}");
        assert_eq!(refused.message, "Said.", "{code}");
    }
    for body in [
        r#"{"_tag":"PublicError","message":"m"}"#,
        r#"{"_tag":"PublicError","code":"CONFLICT"}"#,
        r#"{"_tag":"PublicError","code":null,"message":"m"}"#,
        r#"{"_tag":"PublicError","code":"CONFLICT","message":7}"#,
        r#"{"_tag":"Other","code":"CONFLICT","message":"m"}"#,
        r#"{"code":"CONFLICT","message":"m"}"#,
        r#"{"error":"m"}"#,
        r#"{"error":{"code":"CONFLICT","message":"m"}}"#,
        "[]",
        "",
        "not json",
    ] {
        assert!(public_error(body.as_bytes()).is_none(), "{body}");
    }
    let extra = public_error(
        br#"{"_tag":"PublicError","code":"CONFLICT","message":"m","error":"e","details":{"a":1}}"#,
    )
    .unwrap();
    assert_eq!(
        (extra.code.as_str(), extra.message.as_str()),
        ("conflict", "m")
    );
}
