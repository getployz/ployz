use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

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
            let out = joined(&LogExcerpt::scrub(
                frames,
                &set,
                LineCap::new(LineCap::MAX).unwrap(),
            ));
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

// Catches a set covering several Services that keeps only one value per name.
#[test]
fn every_sealed_value_under_a_shared_name_is_scrubbed() {
    let set = secrets(&[("KEY", "first-service-key"), ("KEY", "second-service-key")]);
    let out = scrub_text("a first-service-key\nb second-service-key\n", &set);
    assert_eq!(out.lines(), ["a [secret KEY]", "b [secret KEY]"]);
}

fn stream(frames: &[(Channel, &[u8])], forms: &SealedForms) -> Vec<(Channel, Vec<u8>)> {
    let mut scrub = FrameScrub::new();
    let mut out: BTreeMap<Channel, Vec<u8>> = BTreeMap::new();
    for (channel, frame) in frames {
        let released = scrub.push(*channel, frame, forms);
        out.entry(*channel).or_default().extend(released);
    }
    for (channel, held) in scrub.finish(forms) {
        out.entry(channel).or_default().extend(held);
    }
    out.into_iter().collect()
}

fn stdout_of(frames: &[&[u8]], forms: &SealedForms) -> Vec<u8> {
    let frames: Vec<(Channel, &[u8])> = frames.iter().map(|f| (Channel::Stdout, *f)).collect();
    stream(&frames, forms).swap_remove(0).1
}

#[test]
fn a_value_split_across_frames_is_scrubbed() {
    for value in ["abcdefghij0123456789", "héllo-日本-secret"] {
        let forms = SealedForms::compile(&secrets(&[("TOKEN", value)]));
        let text = format!("start {value} end");
        for at in 0..=text.len() {
            let (head, tail) = split_at_byte(&text, at);
            let out = stdout_of(&[&head, &tail], &forms);
            let shown = String::from_utf8(out).expect("released bytes keep UTF-8 whole");
            assert!(shown.contains("[secret TOKEN]"), "split at {at}: {shown}");
            assert!(!shown.contains(value), "split at {at}: {shown}");
        }
    }
}

#[test]
fn random_frames_release_the_batch_scrub() {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for _ in 0..300 {
        let value = random_secret(&mut rng);
        let forms = SealedForms::compile(&secrets(&[("API_KEY", &value)]));
        let encoded = [
            value.clone(),
            STANDARD.encode(&value),
            STANDARD_NO_PAD.encode(&value),
            percent_encoded(&value, b"0123456789ABCDEF"),
            percent_encoded(&value, b"0123456789abcdef"),
        ];
        let mut text = random_noise(&mut rng);
        for _ in 0..1 + rng.below(4) {
            text.push_str(encoded.get(rng.below(encoded.len())).unwrap());
            text.push_str(&random_noise(&mut rng));
        }
        // A complete stream ends its line; a trailing form prefix is hidden instead.
        text.push('\n');
        let mut frames: Vec<Vec<u8>> = Vec::new();
        let mut rest = text.as_bytes();
        while !rest.is_empty() {
            let (frame, tail) = rest.split_at(rest.len().min(1 + rng.below(12)));
            frames.push(frame.to_vec());
            rest = tail;
        }
        let frames: Vec<&[u8]> = frames.iter().map(Vec::as_slice).collect();
        let out = stdout_of(&frames, &forms);
        let batch = forms.scrub(&text, &mut 0);
        assert_eq!(String::from_utf8(out).unwrap(), batch, "frames {frames:?}");
    }
}

#[test]
fn channels_do_not_mix() {
    let value = "abcdefghij0123456789";
    let forms = SealedForms::compile(&secrets(&[("TOKEN", value)]));
    let (head, tail) = split_at_byte(value, 10);
    let out = stream(
        &[
            (Channel::Stdout, b"x "),
            (Channel::Stdout, &head),
            (Channel::Stderr, &tail),
        ],
        &forms,
    );
    let stdout = String::from_utf8(out.first().unwrap().1.clone()).unwrap();
    let stderr = String::from_utf8(out.get(1).unwrap().1.clone()).unwrap();
    assert_eq!(stdout, "x [secret TOKEN]");
    assert_eq!(stderr, "0123456789");
}

#[test]
fn finish_hides_a_proper_prefix_of_a_form() {
    let value = "abcdefghij0123456789";
    let forms = SealedForms::compile(&secrets(&[("TOKEN", value)]));
    let out = stdout_of(&[b"plain abcdefghij"], &forms);
    assert_eq!(out, b"plain [secret TOKEN]");
    let out = stdout_of(&[b"b64 YWJjZGVm"], &forms);
    assert_eq!(out, b"b64 [secret TOKEN]");
    let out = stdout_of(&[b"plain text"], &forms);
    assert_eq!(out, b"plain text");
}

// A label keeps its text held until the value arrives, since `\s*` between them is
// unbounded; the cap is what stops that hold from growing without end.
#[test]
fn a_label_far_from_its_value_releases_at_the_cap() {
    let forms = SealedForms::compile(&secrets(&[("TOKEN", "ab12")]));
    let mut scrub = FrameScrub::new();
    let mut frame = b"TOKEN=".to_vec();
    frame.extend(vec![b' '; CARRY_CAP + 500]);
    let released = scrub.push(Channel::Stdout, &frame, &forms);
    assert_eq!(released.len(), frame.len() - CARRY_CAP);
    assert!(released.starts_with(b"TOKEN="));
    let released = scrub.push(Channel::Stdout, b"ab12 tail\n", &forms);
    assert_eq!(released.len(), CARRY_CAP + 10);
    assert!(scrub.finish(&forms).is_empty());
    let mut scrub = FrameScrub::new();
    let near = scrub.push(Channel::Stdout, b"TOKEN=      ", &forms);
    assert!(near.is_empty());
    let out = scrub.push(Channel::Stdout, b"  ab12 tail\n", &forms);
    assert_eq!(out, b"TOKEN=        [secret TOKEN] tail\n");
}

#[test]
fn each_line_of_a_multiline_value_is_scrubbed() {
    let value = "line1-secret\nline2-secret";
    let forms = SealedForms::compile(&secrets(&[("CERT", value)]));
    assert!(forms.multiline());
    let text = "a line1-secret b\nc line2-secret d\n";
    let want = "a [secret CERT] b\nc [secret CERT] d\n";
    assert_eq!(forms.scrub(text, &mut 0), want);
    let out = stdout_of(&[b"a line1-sec", b"ret b\nc line2-", b"secret d\n"], &forms);
    assert_eq!(String::from_utf8(out).unwrap(), want);
}

#[test]
fn a_line_end_releases_beside_a_multiline_value() {
    let forms = SealedForms::compile(&secrets(&[("CERT", "line1-secret\nline2-secret")]));
    let mut scrub = FrameScrub::new();
    assert_eq!(
        scrub.push(Channel::Stdout, b"info line\n", &forms),
        b"info line\n"
    );
    assert!(scrub.finish(&forms).is_empty());
}

#[test]
fn a_multiline_value_split_across_frames_is_one_marker() {
    let forms = SealedForms::compile(&secrets(&[("CERT", "line1-secret\nline2-secret")]));
    // The second head ends at the value's own line break, which must not release its first line.
    for (head, shown, tail) in [
        (&b"a line1-sec"[..], &b""[..], &b"ret\nline2-secret b\n"[..]),
        (b"a line1-secret\n", b"a ", b"line2-secret b\n"),
    ] {
        let mut scrub = FrameScrub::new();
        let mut out = scrub.push(Channel::Stdout, head, &forms);
        assert_eq!(out, shown);
        out.extend(scrub.push(Channel::Stdout, tail, &forms));
        assert_eq!(String::from_utf8(out).unwrap(), "a [secret CERT] b\n");
        assert!(scrub.finish(&forms).is_empty());
    }
}
