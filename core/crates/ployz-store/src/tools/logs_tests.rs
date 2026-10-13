use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

use super::*;

fn secrets(pairs: &[(&str, &str)]) -> SecretSet {
    let mut set = SecretSet::from_store(
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        [],
    );
    set.add_machine_env(Some(&BTreeMap::new()));
    set
}

fn shown(text: LogText) -> LogExcerpt {
    match text {
        LogText::Shown(excerpt) => excerpt,
        LogText::Withheld { why } => panic!("expected shown text, got withheld {why:?}"),
    }
}

fn joined(excerpt: &LogExcerpt) -> String {
    excerpt.lines().join("\n")
}

fn scrub_text(text: &str, set: &SecretSet) -> LogExcerpt {
    shown(LogExcerpt::scrub(
        [text.to_owned()],
        set,
        LineCap::default(),
    ))
}

/// xorshift64*: a fixed seed keeps failures reproducible without a property-test crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }

    fn pick<'a>(&mut self, from: &'a [&'a str]) -> &'a str {
        from.get(self.below(from.len())).unwrap()
    }
}

const SECRET_PIECES: &[&str] = &[
    "a", "Z", "7", "/", "+", "=", "&", "%", " ", "?", "#", "é", "日", "x9", "Q", "_", "-", "~",
];
const NOISE: &[&str] = &[
    "GET /health 200",
    "could not resolve host",
    "token refreshed",
    "user=",
    "\n",
    " ",
    "; ",
    "%",
    "ok",
];

fn random_secret(rng: &mut Rng) -> String {
    let mut value = String::new();
    while value.len() < SHORT_VALUE + rng.below(20) {
        value.push_str(rng.pick(SECRET_PIECES));
    }
    value
}

fn random_noise(rng: &mut Rng) -> String {
    (0..rng.below(5)).map(|_| rng.pick(NOISE)).collect()
}

fn split_at_byte(text: &str, at: usize) -> (Vec<u8>, Vec<u8>) {
    let (head, tail) = text.as_bytes().split_at(at.min(text.len()));
    (head.to_vec(), tail.to_vec())
}

#[test]
fn no_secret_form_survives_random_logs() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..500 {
        let value = random_secret(&mut rng);
        let set = secrets(&[("API_KEY", &value)]);
        let forms = [
            value.clone(),
            STANDARD.encode(&value),
            STANDARD_NO_PAD.encode(&value),
            percent_encoded(&value, b"0123456789ABCDEF"),
        ];
        for form in &forms {
            let text = format!(
                "{}{form}{}\n",
                random_noise(&mut rng),
                random_noise(&mut rng)
            );
            // Frames are byte chunks of a stream; split only where both halves stay UTF-8,
            // the way the log decoder hands them over.
            let mut at = rng.below(text.len() + 1);
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            let (head, tail) = split_at_byte(&text, at);
            let frames = [
                String::from_utf8(head).unwrap(),
                String::from_utf8(tail).unwrap(),
            ];
            let out = joined(&shown(LogExcerpt::scrub(
                frames,
                &set,
                LineCap::new(LineCap::MAX).unwrap(),
            )));
            for leaked in &forms {
                assert!(
                    !out.contains(leaked.as_str()),
                    "{leaked:?} leaked in {out:?}"
                );
            }
            assert!(out.contains("[secret API_KEY]"), "{out:?}");
        }
    }
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
fn incomplete_secret_set_withholds_the_text() {
    let raw = "DATABASE_URL=postgres://app:s3cr3t-pass@db/app";
    let mut machine_unread = SecretSet::from_store([], []);
    machine_unread.add_machine_env(None);
    let mut no_store = SecretSet::unknown();
    no_store.add_machine_env(Some(&BTreeMap::new()));
    let mut one_unread = SecretSet::from_store([], []);
    one_unread.add_machine_env(Some(&BTreeMap::new()));
    one_unread.add_machine_env(None);
    one_unread.add_machine_env(Some(&BTreeMap::new()));
    for set in [machine_unread, no_store, one_unread, SecretSet::unknown()] {
        let text = LogExcerpt::scrub([raw.to_owned()], &set, LineCap::default());
        assert_eq!(
            text,
            LogText::Withheld {
                why: Withheld::NoSecretContext
            }
        );
        let json = serde_json::to_string(&text).unwrap();
        assert!(!json.contains("s3cr3t"), "{json}");
        assert!(!json.contains("DATABASE_URL"), "{json}");
    }
}

#[test]
fn a_cut_line_never_keeps_a_secret_prefix() {
    let value = "Zq8-straddling-secret-value-Zq8";
    let line = format!("{}{value}{}", "x".repeat(490), "y".repeat(100));
    let out = shown(LogExcerpt::scrub(
        [line],
        &secrets(&[("TOKEN_A", value)]),
        LineCap::default(),
    ));
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
    let out = shown(LogExcerpt::scrub(
        [text],
        &secrets(&[("CERT", value)]),
        LineCap::new(1).unwrap(),
    ));
    assert_eq!(out.lines(), ["could not resolve host"]);
    assert!(out.cut());
    let out = shown(LogExcerpt::scrub(
        [format!("{value}\ncould not resolve host")],
        &secrets(&[("CERT", value)]),
        LineCap::new(2).unwrap(),
    ));
    let text = joined(&out);
    assert!(!text.contains("second-half"), "{text:?}");
    assert!(!text.contains("multi-line"), "{text:?}");
    assert!(text.contains("could not resolve host"));
}

#[test]
fn byte_cap_drops_oldest_lines() {
    let line = "z".repeat(400);
    let text: String = (0..100).map(|i| format!("{i:03}{line}\n")).collect();
    let out = shown(LogExcerpt::scrub(
        [text],
        &secrets(&[]),
        LineCap::new(LineCap::MAX).unwrap(),
    ));
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

#[test]
fn machine_env_scrubs_what_the_store_does_not_know_as_plain() {
    let mut set = SecretSet::from_store([], [("HOST".to_owned(), "db.internal".to_owned())]);
    set.add_machine_env(Some(&BTreeMap::from([
        ("HOST".to_owned(), "db.internal".to_owned()),
        ("INJECTED".to_owned(), "machine-only-value".to_owned()),
    ])));
    let out = scrub_text("dial db.internal with machine-only-value", &set);
    assert_eq!(joined(&out), "dial db.internal with [secret INJECTED]");
}

#[test]
fn machine_env_overriding_a_plain_value_is_a_secret() {
    let mut set = SecretSet::from_store([], [("HOST".to_owned(), "db.internal".to_owned())]);
    set.add_machine_env(Some(&BTreeMap::from([(
        "HOST".to_owned(),
        "other.internal".to_owned(),
    )])));
    let out = scrub_text("dial other.internal", &set);
    assert_eq!(joined(&out), "dial [secret HOST]");
}

#[test]
fn debug_prints_names_only() {
    let set = secrets(&[("API_KEY", "visible-would-be-bad")]);
    let debug = format!("{set:?}");
    assert!(debug.contains("API_KEY"));
    assert!(!debug.contains("visible-would-be-bad"));
}

#[test]
fn line_cap_bounds() {
    assert_eq!(LineCap::new(0), None);
    assert_eq!(LineCap::new(201), None);
    assert_eq!(LineCap::new(200).map(LineCap::get), Some(200));
}

// Catches a set covering several Containers that keeps only one value per name.
#[test]
fn every_containers_value_under_a_shared_name_is_scrubbed() {
    let mut set = SecretSet::from_store([], []);
    for value in ["first-container-key", "second-container-key"] {
        set.add_machine_env(Some(&BTreeMap::from([(
            "KEY".to_owned(),
            value.to_owned(),
        )])));
    }
    let text = LogExcerpt::scrub(
        ["a first-container-key\nb second-container-key\n".to_owned()],
        &set,
        LineCap::default(),
    );
    let LogText::Shown(excerpt) = text else {
        panic!("withheld: {text:?}");
    };
    assert_eq!(excerpt.lines(), ["a [secret KEY]", "b [secret KEY]"]);
}
