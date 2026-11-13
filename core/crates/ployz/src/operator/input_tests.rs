use ployz_core::{ContainerSelector, ServiceSelector};

use super::{
    ExecMode, InputError, ProxyPorts, ServiceArg, TIME_FORMATS, parse_log_time, parse_proxy_ports,
    parse_service_args, parse_tail,
};

#[test]
fn service_args_tail_and_proxy_ports_cover_the_argument_tables() {
    assert_eq!(
        parse_service_args(&strings([
            "api:one",
            "api:two",
            "worker:x",
            "api",
            "shop-staging/web:one",
        ]))
        .unwrap(),
        [
            ServiceArg {
                service: service_selector("api"),
                containers: vec![],
            },
            ServiceArg {
                service: service_selector("worker"),
                containers: vec![container_selector("x")],
            },
            ServiceArg {
                service: service_selector("shop-staging/web"),
                containers: vec![container_selector("one")],
            },
        ]
    );
    assert_eq!(parse_tail("all").unwrap(), -1);
    assert_eq!(parse_tail("0").unwrap(), 0);
    assert!(parse_tail("-1").is_err());
    assert_eq!(
        parse_proxy_ports("0:65535").unwrap(),
        ProxyPorts {
            local: 0,
            remote: 65535
        }
    );
    assert_eq!(parse_proxy_ports("80").unwrap().local, 0);
    for invalid in ["-1:80", "65536:80", "0", "1:0", "1:65536", "1:2:3"] {
        assert!(parse_proxy_ports(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn parse_log_time_accepts_documented_formats_and_rejects_garbage() {
    use chrono::TimeZone as _;

    let now = 1_800_000_000;
    assert_eq!(parse_log_time("since", "", now).unwrap(), None);
    assert_eq!(
        parse_log_time("since", "1763953966", now).unwrap(),
        Some(1_763_953_966)
    );
    assert_eq!(
        parse_log_time("since", "2024-01-31T10:30:00+02:00", now).unwrap(),
        Some(1_706_689_800)
    );
    assert_eq!(
        parse_log_time("since", "2024-05-14T22:50:00", now).unwrap(),
        Some(
            chrono::Local
                .with_ymd_and_hms(2024, 5, 14, 22, 50, 0)
                .single()
                .unwrap()
                .timestamp()
        )
    );
    assert_eq!(
        parse_log_time("since", "2m30s", now).unwrap(),
        Some(1_799_999_850)
    );
    assert_eq!(
        parse_log_time("since", "30d", now).unwrap(),
        Some(1_797_408_000)
    );
    // A duration past the timestamp range is refused, not wrapped into the future.
    assert!(parse_log_time("since", "10000000000000000000s", now).is_err());
    assert_eq!(
        parse_log_time("since", "notatime", now)
            .unwrap_err()
            .to_string(),
        "invalid time \"notatime\" for --since: expected a relative duration, RFC 3339 date, or Unix timestamp"
    );
    assert_eq!(
        parse_tail("abc").unwrap_err().to_string(),
        "invalid log tail \"abc\": expected a non-negative integer or all"
    );
}

#[test]
fn every_time_format_the_help_names_parses() {
    use chrono::TimeZone as _;

    let now = 1_800_000_000;
    let day = chrono::Local
        .with_ymd_and_hms(2026, 1, 31, 0, 0, 0)
        .single()
        .unwrap()
        .timestamp();
    for (value, expected) in [
        ("10m", now - 600),
        ("2h", now - 7200),
        ("7d", now - 7 * 86_400),
        ("2026-01-31", day),
        ("2026-01-31T10:30:00+00:00", 1_769_855_400),
        ("1769855400", 1_769_855_400),
    ] {
        assert_eq!(
            parse_log_time("since", value, now).unwrap(),
            Some(expected),
            "{value}"
        );
    }
    assert!(parse_log_time("since", "2026-01-31T10:30:00", now).is_ok());
    assert!(TIME_FORMATS.contains("Unix seconds"));
}

#[test]
fn input_refusals_say_what_they_said_and_carry_their_codes() {
    use crate::failure::Failure;
    let named = |value: &str| parse_service_args(&strings([value])).unwrap_err();
    let refusals: Vec<(InputError, ployz_core::RpcErrorCode)> = vec![
        (named("api:"), ployz_core::RpcErrorCode::InvalidArgument),
        (named("a/b/c"), ployz_core::RpcErrorCode::InvalidArgument),
        (
            parse_proxy_ports("1:2:3").unwrap_err(),
            ployz_core::RpcErrorCode::InvalidArgument,
        ),
        (
            parse_proxy_ports("-1:80").unwrap_err(),
            ployz_core::RpcErrorCode::InvalidArgument,
        ),
        (
            parse_proxy_ports("1:0").unwrap_err(),
            ployz_core::RpcErrorCode::InvalidArgument,
        ),
        (
            ExecMode::resolve(false, false, true, false).unwrap_err(),
            ployz_core::RpcErrorCode::InvalidArgument,
        ),
    ];
    let shown: Vec<_> = refusals
        .into_iter()
        .map(|(error, code)| {
            let failure = Failure::from(error);
            let report = failure.report();
            assert_eq!(report.code, code);
            report.message
        })
        .collect();
    assert_eq!(
        shown,
        [
            "invalid Service log selector \"api:\"",
            "invalid Service Selector \"a/b/c\": a Service ID, Qualified Service (namespace/name), or Service Name",
            "invalid proxy port: expected [LOCAL_PORT:]REMOTE_PORT",
            "invalid local port \"-1\": expected 0-65535",
            "invalid remote port \"0\": expected 1-65535",
            "cannot attach a terminal exec to non-terminal stdin; use -T",
        ]
    );
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(ToOwned::to_owned).collect()
}

fn service_selector(value: &str) -> ServiceSelector {
    ServiceSelector::parse(value).unwrap()
}

fn container_selector(value: &str) -> ContainerSelector {
    ContainerSelector::parse(value).unwrap()
}
