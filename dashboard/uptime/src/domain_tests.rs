use std::net::IpAddr;

use super::*;

/// Catches a parser that lets through a row the probe must not dial (other ports, userinfo,
/// fragments, other schemes) or that mangles the host and path the probe sends.
#[test]
fn target_parse_accepts_what_crud_allows_and_refuses_the_rest() {
    let ok = |url: &str| Target::parse(url).unwrap_or_else(|e| panic!("{url}: {e}"));
    let name = |n: &str| Host::Name(n.to_owned());
    let ip = |a: &str| Host::Ip(a.parse::<IpAddr>().unwrap());
    let cases = [
        (
            "https://Example.COM",
            Scheme::Https,
            name("example.com"),
            443,
            "/",
        ),
        (
            "http://example.com/a?b=1",
            Scheme::Http,
            name("example.com"),
            80,
            "/a?b=1",
        ),
        (
            "http://example.com:443/",
            Scheme::Http,
            name("example.com"),
            443,
            "/",
        ),
        (
            "https://example.com:/",
            Scheme::Https,
            name("example.com"),
            443,
            "/",
        ),
        (
            "https://example.com:80/",
            Scheme::Https,
            name("example.com"),
            80,
            "/",
        ),
        (
            "https://203.0.113.9/",
            Scheme::Https,
            ip("203.0.113.9"),
            443,
            "/",
        ),
        (
            "https://[2001:db8::1]/x",
            Scheme::Https,
            ip("2001:db8::1"),
            443,
            "/x",
        ),
    ];
    for (url, scheme, host, port, path) in cases {
        let target = ok(url);
        assert_eq!(
            (
                target.scheme,
                target.host,
                target.port,
                target.path_and_query.as_str()
            ),
            (scheme, host, port, path),
            "{url}"
        );
    }
    let long = format!("https://example.com/{}", "a".repeat(2048));
    for url in [
        "https://example.com:8443/",
        "https://example.com:65536/",
        "http://example.com:99999999/",
        "https://user:pw@example.com/",
        "https://example.com/#top",
        "ftp://example.com/",
        "example.com/",
        "https://[2001:db8::1/",
        long.as_str(),
    ] {
        assert!(Target::parse(url).is_err(), "{url} parsed");
    }
}

#[test]
fn expect_below_400_judges_every_outcome_kind() {
    let cases: [(Outcome, Verdict); 6] = [
        (Outcome::Http { status: 200 }, Verdict::Pass),
        (Outcome::Http { status: 399 }, Verdict::Pass),
        (Outcome::Http { status: 400 }, Verdict::Fail),
        (Outcome::Http { status: 503 }, Verdict::Fail),
        (
            Outcome::Unreachable {
                phase: Phase::Connect,
                error: HttpCheckError::ConnectionRefused,
                detail: Detail::new("refused"),
            },
            Verdict::Fail,
        ),
        (
            Outcome::NotProbed(NotProbed::Saturated),
            Verdict::Inconclusive,
        ),
    ];
    for (outcome, want) in cases {
        assert_eq!(Expect::Below400.judge(&outcome), want, "{outcome:?}");
    }
}
