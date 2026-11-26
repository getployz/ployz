use std::{borrow::Cow, io::Write as _};

use super::{mask, mask_bytes, mask_json};
use crate::ui::stream::Masked;

#[test]
fn a_urls_user_info_is_masked_whole_however_it_is_written() {
    for (text, masked) in [
        // A review's finds: one slash, a password holding `/` after a port-like start,
        // and a password holding `@` and `/`.
        (
            "https:/demo:sample@example.invalid",
            "https:/<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "https://demo:12/sample@example.invalid",
            "https://<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "https://demo:sa@m/ple@example.invalid",
            "https://<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "HTTPS://demo:sample@example.invalid",
            "HTTPS://<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "Https:demo:sample@example.invalid",
            "Https:<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "https:\\\\demo:sample@example.invalid",
            "https:<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "https://demo%40mail:sa%40mple@example.invalid",
            "https://<USER>:<PASSWORD>@example.invalid",
        ),
        (
            "https://demo:sample@[2001:db8::1]:8443/x",
            "https://<USER>:<PASSWORD>@[2001:db8::1]:8443/x",
        ),
        (
            "https://user:sample@example.invalid:99999/config.yaml",
            "https://<USER>:<PASSWORD>@example.invalid:99999/config.yaml",
        ),
        (
            "connection \"ssh://user:sample@x.example\" not found",
            "connection \"ssh://<USER>:<PASSWORD>@x.example\" not found",
        ),
        (
            "Could not read https://sample@github.com/acme/web.git",
            "Could not read https://<USER>@github.com/acme/web.git",
        ),
        (
            "git+ssh://git@github.com:acme/web",
            "git+ssh://<USER>@github.com:acme/web",
        ),
        (
            "'https://user:sam'\\''ple@host'",
            "'https://<USER>:<PASSWORD>@host'",
        ),
        (
            "https://user:sa\"mple@host",
            "https://<USER>:<PASSWORD>@host",
        ),
        // A color code inside the URL.
        (
            "\u{1b}[1mhttps://\u{1b}[2muser:sample\u{1b}[0m@host\u{1b}[0m",
            "\u{1b}[1mhttps://<USER>:<PASSWORD>@host\u{1b}[0m",
        ),
        // A URL whose scheme follows other text, as a regex that finds URLs would read it.
        (
            "url=xhttps:demo:sample@host",
            "url=xhttps:<USER>:<PASSWORD>@host",
        ),
        (
            "https://demo:sample@host/x?img=a@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "https://<USER>:<PASSWORD>@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        (
            "http:demo:sample@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "http:<USER>:<PASSWORD>@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        (
            "ghcr.io/acme/my-http:demo:sample@host/a@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "ghcr.io/acme/my-http:<USER>:<PASSWORD>@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        // An image reference has the shape of a URL whose host is a digest when its repository
        // ends in a special scheme's name, so its tag is masked too.
        (
            "ghcr.io/acme/my-http:v1@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "ghcr.io/acme/my-http:<USER>@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ),
        (
            "\u{1b}[1mftp:user:sample@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/path\u{1b}[0m",
            "\u{1b}[1mftp:<USER>:<PASSWORD>@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef/path\u{1b}[0m",
        ),
        (
            "a https://u:sample@a.example and b ssh://sample@b.example",
            "a https://<USER>:<PASSWORD>@a.example and b ssh://<USER>@b.example",
        ),
    ] {
        assert_eq!(mask(text), masked, "{text}");
        assert_eq!(
            mask(masked),
            masked,
            "masking again changes nothing: {text}"
        );
    }
}

#[test]
fn text_without_user_info_is_left_as_it_is() {
    for text in [
        "https://cloud.example:8443/api",
        "nginx@sha256:abc and user@host and https://",
        "docker.io/library/nginx:1.25@sha256:0123abcd",
        "ghcr.io/acme/web:v1@sha256:0123abcd",
        "git@github.com:acme/web.git",
        "mailto:a@b.example",
        "See https://example.com for help, or mail a@b.example.",
        "{\"a\":\"https://host/x\"} c@d",
        "https://<USER>:<PASSWORD>@host",
        "",
    ] {
        assert!(matches!(mask(text), Cow::Borrowed(_)), "{text}");
    }
}

#[test]
fn a_url_never_has_user_info_before_an_image_digest() {
    let hex = "0123456789abcdef".repeat(4);
    for text in [
        format!("https://user:pw@sha256:{hex}"),
        format!("https://user:pw@SHA512:{}", "9".repeat(32)),
        format!("ftp:user:pw@sha256:{hex}/x"),
    ] {
        assert!(url::Url::parse(&text).is_err(), "{text}");
    }
}

#[test]
fn every_string_in_a_json_document_is_masked_keys_too() {
    let mut value = serde_json::json!({
        "query": "https://user:sample@x.example/a",
        "list": ["ssh://sample@host", 3, null],
        "https://sample@key.example": {"deep": "x://u:sample@y"},
    });
    mask_json(&mut value);
    assert!(!value.to_string().contains("sample"), "{value}");
    assert_eq!(value.pointer("/list/0").unwrap(), "ssh://<USER>@host");
    assert_eq!(
        value
            .get("https://<USER>@key.example")
            .and_then(|key| key.get("deep"))
            .unwrap(),
        "x://<USER>:<PASSWORD>@y"
    );
}

#[test]
fn keys_that_mask_alike_stay_separate_fields() {
    let mut value = serde_json::json!({
        "https://first@x.example": 1,
        "https://second@x.example": 2,
        "https://<USER>@x.example": 3,
    });
    mask_json(&mut value);
    let fields = value.as_object().unwrap();
    assert_eq!(fields.len(), 3, "{value}");
    let mut kept: Vec<_> = fields
        .values()
        .filter_map(serde_json::Value::as_i64)
        .collect();
    kept.sort_unstable();
    assert_eq!(kept, [1, 2, 3], "{value}");
    assert!(!value.to_string().contains("first") && !value.to_string().contains("second"));
}

#[test]
fn a_port_with_no_digits_before_a_query_holding_an_at_masks_and_stays_valid_json() {
    let text = "https://example.invalid:/path?email=a@b";
    assert!(
        !mask(text).contains("example.invalid:/path"),
        "{}",
        mask(text)
    );
    let mut value = serde_json::json!({ text: text, "list": [text] });
    mask_json(&mut value);
    let printed = value.to_string();
    let parsed: serde_json::Value = serde_json::from_str(&printed).unwrap();
    assert_eq!(parsed, value);
    assert_eq!(parsed.as_object().unwrap().len(), 2, "{printed}");
}

/// A held trailing token waits for its boundary, so a flush between `…:sample` and
/// `@host` leaks nothing, and a line ending in `\r` still goes out at once.
#[test]
fn a_flush_holds_back_a_token_that_may_still_become_user_info() {
    let mut out = Vec::new();
    {
        let mut masked = Masked::new(&mut out);
        write!(masked, "fetching https://demo:sample").unwrap();
        masked.flush().unwrap();
        assert_eq!(masked.written(), b"fetching ");
        write!(masked, "@host done\r").unwrap();
        masked.flush().unwrap();
        assert_eq!(
            masked.written(),
            b"fetching https://<USER>:<PASSWORD>@host done\r"
        );
        write!(masked, "next ssh://sample").unwrap();
    }
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "fetching https://<USER>:<PASSWORD>@host done\rnext ssh://sample"
    );
}

// The guarantee, as a property over generated text: no byte of a URL's user info is
// printed, whether `url::Url`, the parser this CLI uses, reads it as user info or the
// user wrote it between the scheme and the last `@`. A user-info byte is a marker, a
// non-ASCII letter used once per text, so finding it in the output finds that byte.
// Whitespace inside a password is out of scope (#4592), so none is generated there.

const MARKERS: &str = "ΑΒΓΔΕΖΗΘΙΚΛΜΝΞΟΠΡΣΤΥΦΧΨΩАБВГДЕЖЗИЙКЛМНОПРСТУФХЦЧШЩЪЫЬЭЮЯ";

const SCHEMES: &[&str] = &[
    "https",
    "http",
    "HTTPS",
    "Http",
    "wss",
    "ftp",
    "file",
    "ssh",
    "postgres",
    "redis",
    "git+ssh",
    "mongodb+srv",
    "s3",
    "x",
    "1a",
    "my-http",
    "xhttps",
];
const SLASHES: &[&str] = &["//", "/", "", "///", "\\\\", "/\\", "//\\", "////"];
const INFO: &[&str] = &[
    ":",
    ":",
    "/",
    "@",
    "?",
    "#",
    "%40",
    "%2F",
    "%3A",
    "%",
    "[",
    "]",
    "::1",
    "\\",
    "\"",
    "'",
    "!",
    "$",
    "&",
    "(",
    ")",
    "*",
    "+",
    ",",
    ";",
    "=",
    "~",
    "<",
    ">",
    "{",
    "}",
    "|",
    "^",
    "`",
    "\u{1b}[2m",
    "\u{1b}[0m",
    "\u{1}",
    "\u{7f}",
    "é",
    "\u{a0}",
    "user",
    "12",
    "demo",
    "-",
    ".",
    "_",
];
const HOSTS: &[&str] = &[
    "example.invalid",
    "EXAMPLE.invalid",
    "h",
    "127.0.0.1:9",
    "host:99999",
    "host:",
    "[::1]",
    "[2001:db8::1]:8443",
    "xn--bcher-kva.example",
    "",
];
const RESTS: &[&str] = &[
    "",
    "/",
    "/path",
    "/a@b",
    "?email=a@b",
    "#frag",
    ":/path?email=a@b",
    "/@scope/pkg",
    "/a@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "?img=a@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "@sha256:012340123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    ":12",
    "\"",
    ")",
];
const PREFIXES: &[&str] = &[
    "",
    "",
    "(",
    "\"",
    "'",
    "<",
    "url=",
    "x:",
    "a@b:",
    "{\"a\":\"",
    "\u{1b}[1m",
    "1",
    "[",
    "see ",
    "tab\t",
];
const SUFFIXES: &[&str] = &[
    "",
    "",
    ")",
    "\"",
    "'",
    ">",
    ".",
    ",",
    "\u{1b}[0m",
    " tail",
    "\"}",
    "@",
    " and https://q@r",
];
const JOINS: &[&str] = &[" ", "\n", "", "\t", "\r\n", "\r"];

/// splitmix64: the same text every run, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn pick(&mut self, items: &[&'static str]) -> &'static str {
        items
            .get(self.below(items.len()))
            .copied()
            .unwrap_or_default()
    }
}

/// One generated text, its URLs as written, and the markers written as user info.
struct Case {
    text: String,
    urls: Vec<String>,
    written: Vec<char>,
}

fn case(rng: &mut Rng) -> Case {
    let mut markers = MARKERS.chars();
    let mut case = Case {
        text: String::new(),
        urls: Vec::new(),
        written: Vec::new(),
    };
    for _ in 0..=rng.below(3) {
        case.text.push_str(rng.pick(PREFIXES));
        for _ in 0..=usize::from(rng.one_in(4)) {
            let (scheme, slashes) = (rng.pick(SCHEMES), rng.pick(SLASHES));
            // Without slashes, only a special scheme's URL has an authority.
            let authority = slashes.starts_with(['/', '\\'])
                || ["http", "https", "wss", "ftp", "file"]
                    .iter()
                    .any(|special| scheme.eq_ignore_ascii_case(special));
            let mut url = format!("{scheme}:{slashes}");
            if !rng.one_in(8) {
                for _ in 0..=rng.below(8) {
                    match markers.next().filter(|_| rng.one_in(2)) {
                        Some(marker) => {
                            url.push(marker);
                            if authority {
                                case.written.push(marker);
                            }
                        }
                        None => url.push_str(rng.pick(INFO)),
                    }
                }
                url.push('@');
            }
            url.push_str(rng.pick(HOSTS));
            url.push_str(rng.pick(RESTS));
            case.text.push_str(&url);
            case.urls.push(url);
        }
        case.text.push_str(rng.pick(SUFFIXES));
        case.text.push_str(rng.pick(JOINS));
    }
    case
}

/// The markers in user info as `url::Url` reads it from `url`.
fn parsed_user_info(url: &str) -> Vec<char> {
    let Ok(parsed) = url::Url::parse(url) else {
        return Vec::new();
    };
    let decoded = |encoded: &str| {
        let bytes = encoded.as_bytes();
        let mut out = Vec::new();
        let mut index = 0;
        while let Some(&byte) = bytes.get(index) {
            let hex = bytes
                .get(index + 1..index + 3)
                .and_then(|hex| std::str::from_utf8(hex).ok())
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            match (byte, hex) {
                (b'%', Some(decoded)) => {
                    out.push(decoded);
                    index += 3;
                }
                _ => {
                    out.push(byte);
                    index += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    };
    decoded(parsed.username())
        .chars()
        .chain(decoded(parsed.password().unwrap_or_default()).chars())
        .filter(|c| MARKERS.contains(*c))
        .collect()
}

/// Every way the property fails on `case`, empty when it holds.
fn failures(case: &Case) -> Vec<String> {
    let masked = mask(&case.text);
    let mut failures = Vec::new();
    for url in &case.urls {
        for marker in parsed_user_info(url) {
            if masked.contains(marker) {
                failures.push(format!("url::Url reads {marker:?} in {url:?} as user info"));
            }
        }
    }
    for &marker in &case.written {
        if masked.contains(marker) {
            failures.push(format!("{marker:?} was written as user info"));
        }
    }
    if mask(&masked) != masked {
        failures.push("masking again changes it".into());
    }
    if mask_bytes(case.text.as_bytes()) != masked.as_bytes() {
        failures.push("masking bytes differs from masking text".into());
    }
    failures
}

#[test]
fn no_byte_of_a_urls_user_info_is_ever_printed() {
    let mut rng = Rng(0x5eed);
    let mut parsed = 0;
    for _ in 0..20_000 {
        let case = case(&mut rng);
        parsed += usize::from(
            case.urls
                .iter()
                .any(|url| !parsed_user_info(url).is_empty()),
        );
        let failures = failures(&case);
        assert!(
            failures.is_empty(),
            "{:?}\n  masked: {:?}\n  {}",
            case.text,
            mask(&case.text),
            failures.join("\n  ")
        );
    }
    assert!(
        parsed > 2_000,
        "too few texts url::Url reads user info in: {parsed}"
    );
}

/// Text written in pieces, flushed after each, never prints a byte the whole text
/// masked at once wouldn't, and prints all of it once dropped.
#[test]
fn a_stream_never_prints_what_masking_the_whole_text_would_not() {
    let mut rng = Rng(0xf105);
    for _ in 0..5_000 {
        let case = case(&mut rng);
        let whole = mask_bytes(case.text.as_bytes()).into_owned();
        let mut out = Vec::new();
        {
            let mut masked = Masked::new(&mut out);
            let mut rest = case.text.as_bytes();
            while !rest.is_empty() {
                let (piece, after) = rest.split_at(1 + rng.below(rest.len()));
                masked.write_all(piece).unwrap();
                masked.flush().unwrap();
                assert!(
                    whole.starts_with(masked.written()),
                    "{:?}\n  printed: {:?}",
                    case.text,
                    String::from_utf8_lossy(masked.written())
                );
                rest = after;
            }
        }
        assert_eq!(out, whole, "{:?}", case.text);
    }
}
