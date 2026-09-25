use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ployz_core::{
    CertificateKeyType, CertificatePolicy, ContainerAddress, ContainerId, ContainerKind,
    ContainerObservation, ContainerRuntimeObservation, DEFAULT_RENEW_AT_LIFETIME_FRACTION,
    HealthObservation, HttpProtocol, IngressHost, MACHINE_API_PORT, Machine, MachineId,
    PortPublication, ProjectName, ResolvedServiceSpec, ServiceId, ServiceName,
    resolve_certificate_policy,
};
use serde_json::json;

use super::{
    CHALLENGE_WAIT, IssuanceAction, RANK_STEP, challenge_probe_addresses, directory_from_env,
    ingress_challenge_ips, issuance_action, machine_jitter, machine_rank, material_validity,
    order_certificate, poll_wait, probe_client, renewal_window, verify_answers, wait_for_http01,
    wanted_certificate_hosts,
};
use crate::corrosion::{CertificateChallenge, CertificateMaterial, CertificateRow};

fn policy_for(directory: &str) -> CertificatePolicy {
    CertificatePolicy::built_in(Some(directory.to_owned()))
}

fn default_policy() -> CertificatePolicy {
    CertificatePolicy::built_in(None)
}

fn policy_probe(seconds: u64) -> CertificatePolicy {
    resolve_certificate_policy(
        Some(&format!(r#"{{"probe_timeout":{seconds}}}"#)),
        &CertificatePolicy::built_in(None),
    )
    .unwrap()
}

#[test]
fn directory_empty_disables_issuance() {
    assert_eq!(directory_from_env(Some("")), None);
    assert_eq!(
        directory_from_env(Some("http://ca.test/directory")).as_deref(),
        Some("http://ca.test/directory")
    );
    assert_eq!(
        directory_from_env(None).as_deref(),
        Some("https://acme-v02.api.letsencrypt.org/directory")
    );
}

#[test]
fn wanted_hosts_are_https_ingress_only() {
    let observations = [
        observation(
            1,
            "api",
            vec![
                ingress("app.example.com", HttpProtocol::Https),
                ingress("plain.example.com", HttpProtocol::Http),
                ingress("web.opaque.ployz.example", HttpProtocol::Https),
            ],
        ),
        {
            let mut hook = observation(
                3,
                "api",
                vec![ingress("hook.example.com", HttpProtocol::Https)],
            );
            hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
                .unwrap();
            hook
        },
    ];

    assert_eq!(
        wanted_certificate_hosts(observations.iter(), &BTreeMap::new()),
        BTreeSet::from([host("app.example.com"), host("web.opaque.ployz.example"),])
    );
    assert_eq!(
        wanted_certificate_hosts(
            [observation(
                1,
                "api",
                vec![ingress("plain.example.com", HttpProtocol::Http)]
            )]
            .iter(),
            &BTreeMap::new(),
        ),
        BTreeSet::new()
    );
    assert_eq!(
        wanted_certificate_hosts([observation(1, "api", Vec::new())].iter(), &BTreeMap::new()),
        BTreeSet::new()
    );
}

#[test]
fn published_material_and_published_wildcards_are_not_wanted() {
    let observations = [observation(
        1,
        "api",
        vec![
            ingress("pinned.example.com", HttpProtocol::Https),
            ingress("web.apps.example.com", HttpProtocol::Https),
            ingress("deep.web.apps.example.com", HttpProtocol::Https),
            ingress("apps.example.com", HttpProtocol::Https),
            ingress("acme.example.com", HttpProtocol::Https),
        ],
    )];
    let acme = self_signed(&["acme.example.com"]);
    let rows = BTreeMap::from([
        (
            certificate_host("pinned.example.com"),
            CertificateRow::Published(self_signed(&["pinned.example.com"])),
        ),
        (
            certificate_host("*.apps.example.com"),
            CertificateRow::Published(self_signed(&["*.apps.example.com"])),
        ),
        (
            certificate_host("acme.example.com"),
            CertificateRow::issued(acme, ployz_core::ClusterRoute::Direct),
        ),
    ]);
    assert_eq!(
        wanted_certificate_hosts(observations.iter(), &rows),
        BTreeSet::from([
            host("acme.example.com"),
            host("apps.example.com"),
            host("deep.web.apps.example.com"),
        ])
    );
}

fn certificate_host(name: &str) -> ployz_core::CertificateHost {
    ployz_core::CertificateHost::parse(name).unwrap()
}

fn self_signed(names: &[&str]) -> CertificateMaterial {
    let pair = rcgen::generate_simple_self_signed(
        names
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
}

#[test]
fn lowest_machine_id_is_rank_zero() {
    let low = MachineId::parse("a".repeat(32)).unwrap();
    let high = MachineId::parse("f".repeat(32)).unwrap();
    assert_eq!(machine_rank(&low, &[high, low]), 0);
    assert_eq!(machine_rank(&high, &[low]), 1);
    assert_eq!(machine_rank(&low, &[]), 0);
}

#[test]
fn only_rank_zero_orders_immediately() {
    assert_eq!(RANK_STEP, Duration::from_secs(30));
    assert_eq!(decide(None, 0, Duration::ZERO), IssuanceAction::Order);
    assert_eq!(
        decide(None, 1, Duration::from_secs(30) - Duration::from_millis(1)),
        IssuanceAction::Nothing
    );
    assert_eq!(
        decide(None, 1, Duration::from_secs(30)),
        IssuanceAction::Order
    );
    assert_eq!(
        decide(None, 2, Duration::from_secs(30)),
        IssuanceAction::Nothing
    );
    assert_eq!(
        decide(None, 2, Duration::from_secs(60)),
        IssuanceAction::Order
    );
    assert_eq!(
        issuance_action(
            None,
            1,
            Duration::from_secs(30),
            UNIX_EPOCH,
            &machine_id("a"),
            &policy_probe(120)
        ),
        IssuanceAction::Nothing
    );
}

#[test]
fn unparseable_stored_material_does_not_suppress_repair() {
    let row = CertificateRow::decode(r#"{"certificate":"CERT","private_key":"KEY"}"#).unwrap();
    assert!(row.material().is_none());
    assert!(row.last_error().unwrap().contains("invalid"));
    assert_eq!(
        decide(Some(&row), 0, Duration::from_secs(60)),
        IssuanceAction::Order
    );
}

#[test]
fn renewal_window_is_two_thirds_of_the_certificate_lifetime() {
    let day = Duration::from_secs(86_400);
    let start = UNIX_EPOCH;
    assert_eq!(
        renewal_window(start, start + day * 90, DEFAULT_RENEW_AT_LIFETIME_FRACTION).unwrap(),
        start + day * 60
    );
    assert_eq!(
        renewal_window(start, start + day * 6, DEFAULT_RENEW_AT_LIFETIME_FRACTION).unwrap(),
        start + day * 4
    );
}

#[test]
fn renewal_window_uses_the_policy_fraction() {
    let start = UNIX_EPOCH;
    let life = Duration::from_secs(100);
    assert_eq!(
        renewal_window(start, start + life, 0.5).unwrap(),
        start + Duration::from_secs(50)
    );
}

#[test]
fn machine_jitter_stays_inside_the_remaining_third() {
    let remaining = Duration::from_secs(30 * 86_400);
    let first = machine_jitter(&machine_id("a"), remaining);
    let second = machine_jitter(&machine_id("f"), remaining);
    assert!(first < remaining, "{first:?}");
    assert!(second < remaining, "{second:?}");
    assert_ne!(first, second);
    assert_eq!(machine_jitter(&machine_id("a"), remaining), first);
}

#[test]
fn same_certificate_renews_at_two_thirds_for_long_and_short_lifetimes() {
    let day = Duration::from_secs(86_400);
    let machine = machine_id("a");
    let policy = default_policy();
    for lifetime in [day * 90, day * 6] {
        let not_before = UNIX_EPOCH;
        let not_after = not_before + lifetime;
        let row = row_with_lifetime(not_before, not_after);
        let window =
            renewal_window(not_before, not_after, policy.renew_at_lifetime_fraction()).unwrap();
        assert_eq!(
            issuance_action(
                Some(&row),
                0,
                Duration::ZERO,
                window - Duration::from_secs(1),
                &machine,
                &policy
            ),
            IssuanceAction::Nothing,
            "lifetime {lifetime:?}"
        );
        assert_eq!(
            issuance_action(Some(&row), 0, Duration::ZERO, not_after, &machine, &policy),
            IssuanceAction::Renew,
            "lifetime {lifetime:?}"
        );
    }
}

#[test]
fn renewal_uses_rank_delay_after_jitter() {
    let day = Duration::from_secs(86_400);
    let not_before = UNIX_EPOCH;
    let not_after = not_before + day * 90;
    let row = row_with_lifetime(not_before, not_after);
    let machine = machine_id("a");
    let policy = default_policy();
    let due = super::renew_at(
        row.material().unwrap(),
        &machine,
        policy.renew_at_lifetime_fraction(),
    )
    .unwrap();
    assert_eq!(
        issuance_action(Some(&row), 1, Duration::ZERO, due, &machine, &policy),
        IssuanceAction::Nothing
    );
    assert_eq!(
        issuance_action(
            Some(&row),
            1,
            Duration::ZERO,
            due + RANK_STEP - Duration::from_millis(1),
            &machine,
            &policy
        ),
        IssuanceAction::Nothing
    );
    assert_eq!(
        issuance_action(
            Some(&row),
            1,
            Duration::ZERO,
            due + RANK_STEP,
            &machine,
            &policy
        ),
        IssuanceAction::Renew
    );
}

#[test]
fn machines_holding_the_same_certificate_do_not_renew_together() {
    let day = Duration::from_secs(86_400);
    let not_before = UNIX_EPOCH;
    let not_after = not_before + day * 90;
    let row = row_with_lifetime(not_before, not_after);
    let first = machine_id("a");
    let second = machine_id("f");
    let policy = default_policy();
    let fraction = policy.renew_at_lifetime_fraction();
    let first_due = super::renew_at(row.material().unwrap(), &first, fraction).unwrap();
    let second_due = super::renew_at(row.material().unwrap(), &second, fraction).unwrap();
    assert_ne!(first_due, second_due);
    let earlier = first_due.min(second_due);
    let later_machine = if first_due < second_due {
        &second
    } else {
        &first
    };
    assert_eq!(
        issuance_action(
            Some(&row),
            0,
            Duration::ZERO,
            earlier,
            later_machine,
            &policy
        ),
        IssuanceAction::Nothing
    );
}

#[test]
fn poll_wait_follows_each_certificate_lifetime() {
    let day = Duration::from_secs(86_400);
    let machine = machine_id("a");
    let policy = default_policy();
    let long = row_with_lifetime(UNIX_EPOCH, UNIX_EPOCH + day * 90);
    let short = row_with_lifetime(UNIX_EPOCH, UNIX_EPOCH + day * 6);
    let now = UNIX_EPOCH + Duration::from_secs(60);
    assert_eq!(
        poll_wait(Some(&long), 0, Duration::ZERO, now, &machine, &policy),
        Duration::from_secs(60)
    );
    assert_eq!(
        poll_wait(Some(&short), 0, Duration::ZERO, now, &machine, &policy),
        Duration::from_secs(60)
    );
    let near_short = super::renew_at(
        short.material().unwrap(),
        &machine,
        policy.renew_at_lifetime_fraction(),
    )
    .unwrap()
        - Duration::from_secs(10);
    assert_eq!(
        poll_wait(
            Some(&short),
            0,
            Duration::ZERO,
            near_short,
            &machine,
            &policy
        ),
        Duration::from_secs(10)
    );
}

#[test]
fn probe_addresses_are_the_ingress_intersection_or_every_proxy_address() {
    let ingress_ips = BTreeSet::from([ip("192.0.2.1"), ip("192.0.2.2")]);
    let direct = ployz_core::ClusterRoute::Direct;
    let proxy = ployz_core::ClusterRoute::ViaProxy;
    assert_eq!(
        challenge_probe_addresses(
            &[ip("192.0.2.2"), ip("198.51.100.10")],
            &ingress_ips,
            direct
        ),
        vec![socket("192.0.2.2")]
    );
    assert_eq!(
        challenge_probe_addresses(&[ip("198.51.100.10")], &ingress_ips, direct),
        Vec::<SocketAddr>::new()
    );
    assert_eq!(
        challenge_probe_addresses(
            &[ip("198.51.100.10"), ip("198.51.100.11")],
            &ingress_ips,
            proxy
        ),
        vec![socket("198.51.100.10"), socket("198.51.100.11")]
    );
    assert_eq!(
        challenge_probe_addresses(&[], &ingress_ips, proxy),
        Vec::<SocketAddr>::new()
    );
}

#[test]
fn ingress_challenge_ips_come_from_running_ingress_machines() {
    let local = machine_with_endpoint("a", "192.0.2.1");
    let remote = machine_with_endpoint("b", "192.0.2.2");
    let mut ingress = observation(1, "ingress", Vec::new());
    ingress
        .try_update(|parts| {
            parts.machine_id = local.id;
            parts.resolved_spec.name = ServiceName::parse("ingress").unwrap();
            parts.project_name = ProjectName::system();
        })
        .unwrap();
    let mut down = observation(2, "ingress", Vec::new());
    down.try_update(|parts| {
        parts.machine_id = remote.id;
        parts.resolved_spec.name = ServiceName::parse("ingress").unwrap();
        parts.project_name = ProjectName::system();
        parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
    })
    .unwrap();
    let mut user = observation(3, "caddy", Vec::new());
    user.try_update(|parts| parts.machine_id = remote.id)
        .unwrap();
    assert_eq!(
        ingress_challenge_ips(&[local, remote], &[ingress, down, user]),
        BTreeSet::from([ip("192.0.2.1")])
    );
}

#[tokio::test]
async fn challenge_must_be_answerable_on_every_probe_address() {
    let hostname = host("app.example.com");
    let challenge = CertificateChallenge::parse(
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0",
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::from([(
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0".to_owned(),
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            .to_owned(),
    )])));
    let (first_stop, first_port) =
        ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
    let (second_stop, second_port) = ployz_testkit::fake_acme::serve_http01(answers);
    wait_for_http01(
        &hostname,
        &challenge,
        &[
            SocketAddr::from(([127, 0, 0, 1], first_port)),
            SocketAddr::from(([127, 0, 0, 1], second_port)),
        ],
        ployz_core::ClusterRoute::Direct,
        CHALLENGE_WAIT,
    )
    .await
    .unwrap();
    drop((first_stop, second_stop));
}

#[tokio::test]
async fn a_proxied_challenge_needs_one_serving_address_and_a_direct_one_needs_all() {
    let token = "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0";
    let response = format!("{token}.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    let challenge = CertificateChallenge::parse(token, &response).unwrap();
    let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::from([(
        token.to_owned(),
        response.clone(),
    )])));
    let (serving, port) = ployz_testkit::fake_acme::serve_http01(answers);
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let addresses = [SocketAddr::from(([127, 0, 0, 1], port)), closed];
    let hostname = host("app.example.com");
    let probe = |route| {
        wait_for_http01(
            &hostname,
            &challenge,
            &addresses,
            route,
            Duration::from_millis(300),
        )
    };

    assert!(probe(ployz_core::ClusterRoute::ViaProxy).await.is_ok());
    assert!(matches!(
        probe(ployz_core::ClusterRoute::Direct).await,
        Err(super::Error::ChallengeNotServed)
    ));
    drop(serving);
}

#[tokio::test]
async fn verify_answers_report_status_location_and_body_or_no_answer() {
    let body = serve_once("200 OK", "", "0123456789abcdef0123456789abcdef");
    let redirect = serve_once(
        "301 Moved Permanently",
        "Location: https://app.example.com/.ployz-verify\r\n",
        "",
    );
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let client = probe_client(Duration::from_secs(2)).unwrap();

    let answers =
        verify_answers(&client, &host("app.example.com"), &[body, redirect, closed]).await;

    let loopback = ip("127.0.0.1");
    assert_eq!(
        answers,
        vec![
            (
                loopback,
                ployz_core::VerifyAnswer::Answered {
                    status: 200,
                    location: String::new(),
                    body: "0123456789abcdef0123456789abcdef".into(),
                }
            ),
            (
                loopback,
                ployz_core::VerifyAnswer::Answered {
                    status: 301,
                    location: "https://app.example.com/.ployz-verify".into(),
                    body: String::new(),
                }
            ),
            (loopback, ployz_core::VerifyAnswer::NoAnswer),
        ]
    );
}

/// One canned HTTP/1.1 response on a fresh loopback port.
fn serve_once(status: &'static str, headers: &'static str, body: &'static str) -> SocketAddr {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 1024];
        let _ = stream.read(&mut request);
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
    });
    address
}

#[tokio::test]
async fn empty_probe_addresses_fail_without_waiting() {
    let hostname = host("app.example.com");
    let challenge = CertificateChallenge::parse(
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0",
        "LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    )
    .unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        wait_for_http01(
            &hostname,
            &challenge,
            &[],
            ployz_core::ClusterRoute::ViaProxy,
            CHALLENGE_WAIT,
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(matches!(error, super::Error::ChallengeNotServed));
}

#[tokio::test]
async fn custom_https_hostname_obtains_a_certificate_from_a_fake_ca() {
    let hostname = host("app.example.com");
    let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new()));
    let (http01, validation_port) =
        ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
    let ca = ployz_testkit::fake_acme::FakeCa::bind("127.0.0.1:0")
        .await
        .unwrap();
    ca.set_validation("127.0.0.1", validation_port);
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-{}-{}", std::process::id(), hostname));
    let policy = resolve_certificate_policy(
        Some(&format!(r#"{{"directory_url":"{}"}}"#, ca.directory_url())),
        &CertificatePolicy::built_in(None),
    )
    .unwrap();
    let material = order_certificate(&hostname, &policy, &account_dir, |challenge| {
        let answers = std::sync::Arc::clone(&answers);
        async move {
            answers.lock().unwrap().insert(
                challenge.token().to_owned(),
                challenge.response().to_owned(),
            );
            Ok(())
        }
    })
    .await
    .unwrap();

    assert!(material.certificate().contains("BEGIN CERTIFICATE"));
    assert!(material.private_key().contains("BEGIN"));
    assert_eq!(ca.ordered(), vec!["app.example.com".to_owned()]);
    drop(http01);
    let _ = std::fs::remove_dir_all(account_dir);
}

#[tokio::test]
async fn policy_key_type_and_eab_issue_against_a_fake_ca() {
    let hostname = host("rsa.example.com");
    let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new()));
    let (http01, validation_port) =
        ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
    let ca = ployz_testkit::fake_acme::FakeCa::bind("127.0.0.1:0")
        .await
        .unwrap();
    ca.set_validation("127.0.0.1", validation_port);
    let account_dir = std::env::temp_dir().join(format!(
        "ployzd-acme-policy-{}-{}",
        std::process::id(),
        hostname
    ));
    let policy = resolve_certificate_policy(
        Some(&format!(
            r#"{{
                    "directory_url":"{}",
                    "eab":{{"kid":"kid-1","hmac_key":"dGVzdA"}},
                    "key_type":"ecdsa-p384",
                    "probe_timeout":5
                }}"#,
            ca.directory_url()
        )),
        &CertificatePolicy::built_in(None),
    )
    .unwrap();
    assert_eq!(policy.key_type(), &CertificateKeyType::EcdsaP384);
    assert_eq!(policy.eab().unwrap().kid(), "kid-1");
    let material = order_certificate(&hostname, &policy, &account_dir, |challenge| {
        let answers = std::sync::Arc::clone(&answers);
        async move {
            answers.lock().unwrap().insert(
                challenge.token().to_owned(),
                challenge.response().to_owned(),
            );
            Ok(())
        }
    })
    .await
    .unwrap();

    assert!(material.certificate().contains("BEGIN CERTIFICATE"));
    assert!(material.private_key().contains("BEGIN"));
    assert_eq!(ca.ordered(), vec!["rsa.example.com".to_owned()]);
    drop(http01);
    let _ = std::fs::remove_dir_all(account_dir);
}

#[tokio::test]
async fn fake_ca_lifetime_is_the_certificate_lifetime() {
    for lifetime in [
        Duration::from_secs(90 * 86_400),
        Duration::from_secs(6 * 86_400),
    ] {
        let hostname = host("app.example.com");
        let answers = std::sync::Arc::new(std::sync::Mutex::new(BTreeMap::new()));
        let (http01, validation_port) =
            ployz_testkit::fake_acme::serve_http01(std::sync::Arc::clone(&answers));
        let ca = ployz_testkit::fake_acme::FakeCa::bind("127.0.0.1:0")
            .await
            .unwrap();
        ca.set_validation("127.0.0.1", validation_port);
        ca.set_certificate_lifetime(lifetime);
        let account_dir = std::env::temp_dir().join(format!(
            "ployzd-acme-life-{}-{}-{}",
            std::process::id(),
            lifetime.as_secs(),
            hostname
        ));
        let policy = policy_for(&ca.directory_url());
        let material = order_certificate(&hostname, &policy, &account_dir, |challenge| {
            let answers = std::sync::Arc::clone(&answers);
            async move {
                answers.lock().unwrap().insert(
                    challenge.token().to_owned(),
                    challenge.response().to_owned(),
                );
                Ok(())
            }
        })
        .await
        .unwrap();
        let (not_before, not_after) = material_validity(material.certificate()).unwrap();
        let got = not_after.duration_since(not_before).unwrap();
        assert!(
            got >= lifetime - Duration::from_secs(2) && got <= lifetime + Duration::from_secs(2),
            "lifetime {lifetime:?} got {got:?}"
        );
        drop(http01);
        let _ = std::fs::remove_dir_all(account_dir);
    }
}

#[tokio::test]
async fn order_fails_when_the_directory_is_unreachable() {
    let hostname = host("app.example.com");
    let account_dir = std::env::temp_dir().join(format!(
        "ployzd-acme-unreachable-{}-{}",
        std::process::id(),
        hostname
    ));
    let error = order_certificate(
        &hostname,
        &policy_for("http://127.0.0.1:1/directory"),
        &account_dir,
        |_| async { Ok(()) },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, super::Error::Acme(_) | super::Error::Http(_)),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(account_dir);
}

fn decide(row: Option<&CertificateRow>, rank: usize, elapsed: Duration) -> IssuanceAction {
    issuance_action(
        row,
        rank,
        elapsed,
        UNIX_EPOCH,
        &machine_id("a"),
        &default_policy(),
    )
}

fn machine_id(seed: &str) -> MachineId {
    MachineId::parse(seed.repeat(32)).unwrap()
}

fn row_with_lifetime(not_before: SystemTime, not_after: SystemTime) -> CertificateRow {
    let (certificate, private_key) =
        ployz_testkit::fake_acme::self_signed_material("app.example.com", not_before, not_after);
    let material = CertificateMaterial::parse(certificate, private_key).unwrap();
    CertificateRow::from_parts(Some((material, ployz_core::ClusterRoute::Direct)), None)
}

fn host(name: &str) -> IngressHost {
    IngressHost::parse(name).unwrap()
}

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

fn socket(value: &str) -> SocketAddr {
    SocketAddr::new(ip(value), 80)
}

fn machine_with_endpoint(seed: &str, address: &str) -> Machine {
    serde_json::from_value(json!({
        "id": seed.repeat(32),
        "name": format!("machine-{seed}"),
        "subnet": "10.210.1.0/24",
        "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
        "public_key": vec![3; 32],
        "advertised_endpoints": [format!("{address}:{MACHINE_API_PORT}")],
    }))
    .unwrap()
}

fn ingress(hostname: &str, http_protocol: HttpProtocol) -> PortPublication {
    PortPublication::Ingress {
        hostname: IngressHost::parse(hostname).unwrap(),
        load_balancer_port: 443.try_into().unwrap(),
        container_port: 8080.try_into().unwrap(),
        http_protocol,
    }
}

fn observation(
    suffix: u8,
    service_name: &str,
    ports: Vec<PortPublication>,
) -> ContainerObservation {
    let service_id = ServiceId::parse(format!("{suffix:x}").repeat(32)).unwrap();
    let service_name = ServiceName::parse(service_name).unwrap();
    let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/image", "pull_policy": "missing" },
        "ports": ports,
    }))
    .unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
        display_name: format!("{service_name}-{suffix}"),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse("a".repeat(32)).unwrap(),
        project_name: ProjectName::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec,
        address: Some(ContainerAddress([10, 210, 1, 2].into())),
        labels: BTreeMap::new(),
    })
    .unwrap()
}

#[test]
fn certificate_status_errors_use_plain_status_and_hostname() {
    let hostname = IngressHost::parse("app.example.com").unwrap();
    let authorization = super::Error::Authorization {
        hostname: hostname.clone(),
        status: instant_acme::AuthorizationStatus::Expired,
    }
    .to_string();
    assert!(
        authorization.contains("app.example.com is expired"),
        "{authorization}"
    );
    let order = super::Error::Order {
        hostname,
        status: instant_acme::OrderStatus::Invalid,
    }
    .to_string();
    assert!(order.contains("app.example.com is invalid"), "{order}");
}
