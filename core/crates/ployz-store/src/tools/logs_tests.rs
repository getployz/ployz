use ployz_core::scrub::SHORT_VALUE;

use super::*;

fn secrets(pairs: &[(&str, &str)]) -> SealedValues {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
}

fn joined(excerpt: &LogExcerpt) -> String {
    excerpt.lines().join("\n")
}

fn scrub_text(text: &str, set: &SealedValues) -> LogExcerpt {
    LogExcerpt::scrub([text.to_owned()], set, LineCap::default())
}

#[test]
fn short_value_is_scrubbed_only_where_its_name_labels_it() {
    let out = scrub_text("PIN=4821 then 4821 again", &secrets(&[("PIN", "4821")]));
    assert_eq!(joined(&out), "PIN=[secret PIN] then 4821 again");
    assert_eq!(out.redacted(), 1);
}

#[test]
fn longer_secret_containing_a_shorter_one_is_replaced_whole() {
    let out = scrub_text(
        "dsn hunter2-extended-value end",
        &secrets(&[("SHORT", "hunter2"), ("LONG", "hunter2-extended-value")]),
    );
    assert_eq!(joined(&out), "dsn [secret LONG] end");
}

#[test]
fn a_cut_line_never_keeps_a_secret_prefix() {
    let value = "Zq8-straddling-secret-value-Zq8";
    let line = format!("{}{value}{}", "x".repeat(490), "y".repeat(100));
    let out = LogExcerpt::scrub([line], &secrets(&[("TOKEN_A", value)]), LineCap::default());
    let text = joined(&out);
    for end in SHORT_VALUE..=value.len() {
        let prefix = value.get(..end).unwrap();
        assert!(!text.contains(prefix), "{prefix:?} survived in {text:?}");
    }
    assert!(out.cut());
    assert!(text.ends_with(CUT_MARKER), "{text:?}");
}

#[test]
fn dropped_lines_take_their_secrets_and_kept_lines_stay_readable() {
    let value = "multi-line-secret\nsecond-half-of-it";
    let text = format!("boot\n{value}\ncould not resolve host\n");
    let out = LogExcerpt::scrub(
        [text],
        &secrets(&[("CERT", value)]),
        LineCap::new(1).unwrap(),
    );
    assert_eq!(out.lines(), ["could not resolve host"]);
    assert!(out.cut());
    let out = LogExcerpt::scrub(
        [format!("{value}\ncould not resolve host")],
        &secrets(&[("CERT", value)]),
        LineCap::new(2).unwrap(),
    );
    let text = joined(&out);
    assert!(!text.contains("second-half"), "{text:?}");
    assert!(!text.contains("multi-line"), "{text:?}");
    assert!(text.contains("could not resolve host"));
}

#[test]
fn byte_cap_drops_oldest_lines() {
    let line = "z".repeat(400);
    let text: String = (0..100).map(|i| format!("{i:03}{line}\n")).collect();
    let out = LogExcerpt::scrub([text], &secrets(&[]), LineCap::new(LineCap::MAX).unwrap());
    let total: usize = out.lines().iter().map(String::len).sum();
    assert!(total <= TOTAL_BYTES);
    assert!(out.cut());
    assert!(out.lines().last().unwrap().starts_with("099"));
}

#[test]
fn uncapped_harmless_text_is_untouched() {
    let out = scrub_text("could not resolve host\nretrying", &secrets(&[]));
    assert_eq!(out.lines(), ["could not resolve host", "retrying"]);
    assert!(!out.cut());
    assert_eq!(out.redacted(), 0);
}

#[test]
fn secret_shaped_patterns_are_redacted() {
    let cases = [
        (
            "Authorization: Bearer abc.DEF-123_xyz",
            "Authorization: Bearer [redacted]",
        ),
        (
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl end",
            "jwt [redacted] end",
        ),
        ("key AKIAIOSFODNN7EXAMPLE used", "key [redacted] used"),
        ("PASSWORD=hunter22 user=bob", "PASSWORD=[redacted] user=bob"),
        ("api_token: abcdef123", "api_token: [redacted]"),
        (
            r#"{"client_secret": "s3cr3t"}"#,
            r#"{"client_secret": [redacted]}"#,
        ),
        (
            "connect postgres://app:pa55word@db:5432/app",
            "connect postgres://[redacted]@db:5432/app",
        ),
    ];
    for (raw, expected) in cases {
        let out = scrub_text(raw, &secrets(&[]));
        assert_eq!(joined(&out), expected, "{raw}");
        assert!(out.redacted() >= 1);
    }
}

#[test]
fn pem_block_is_redacted_across_lines() {
    let raw = "loading\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\nqwerty\n-----END RSA PRIVATE KEY-----\ndone";
    let out = scrub_text(raw, &secrets(&[]));
    assert_eq!(out.lines(), ["loading", "[redacted]", "done"]);
}

// Catches a value that is not Sealed, as image env like NODE_ENV, scrubbed from the text.
#[test]
fn only_sealed_values_are_scrubbed() {
    let out = scrub_text(
        "npm warn config production Use `--omit=dev` instead. token sk-live-123456",
        &secrets(&[("API_KEY", "sk-live-123456")]),
    );
    assert_eq!(
        joined(&out),
        "npm warn config production Use `--omit=dev` instead. token [secret API_KEY]"
    );
    assert_eq!(out.redacted(), 1);
}

#[test]
fn line_cap_bounds() {
    assert_eq!(LineCap::new(0), None);
    assert_eq!(LineCap::new(201), None);
    assert_eq!(LineCap::new(200).map(LineCap::get), Some(200));
}

// Catches a set covering several Services that keeps only one value per name.
#[test]
fn every_sealed_value_under_a_shared_name_is_scrubbed() {
    let set = secrets(&[("KEY", "first-service-key"), ("KEY", "second-service-key")]);
    let out = scrub_text("a first-service-key\nb second-service-key\n", &set);
    assert_eq!(out.lines(), ["a [secret KEY]", "b [secret KEY]"]);
}
