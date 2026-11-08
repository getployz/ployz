use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ployz_core::{
    CertificateKeyType, CertificatePolicy, ContainerAddress, ContainerId, ContainerKind,
    ContainerObservation, ContainerRuntimeObservation, DEFAULT_RENEW_AT_LIFETIME_FRACTION,
    HealthObservation, HostnameVerdict, HttpProtocol, IngressHost, IssuanceClock, IssuanceFailure,
    MACHINE_API_PORT, Machine, MachineId, Namespace, PortPublication, Refusal, ResolvedServiceSpec,
    ServiceId, ServiceName, resolve_certificate_policy,
};
use serde_json::json;

use super::{
    CHALLENGE_WAIT, HostIssuance, IssuanceAction, RANK_STEP, RETRY_INTERVAL,
    challenge_probe_addresses, directory_from_env, ingress_challenge_ips, issuance_action,
    issue_wanted, issued, machine_jitter, machine_rank, material_validity, order_certificate,
    poll_wait, probe_client, renewal_window, run, step, verify_answers, wait_for_http01, wake,
    wanted_certificate_hosts,
};
use crate::corrosion::{
    CertificateChallenge, CertificateMaterial, CertificateRow, ReplicatedStore,
    fake_cluster::FakeCluster,
    store_tests::{corrosion_db, identity_store},
};

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

#[tokio::test]
async fn certificate_loop_outlives_corrosion_outages() {
    let mut cluster = FakeCluster::start();
    cluster.stop();
    // While Corrosion is down, its address only accepts and drops, so the loop's first
    // attempt provably fails before Corrosion comes back.
    let down = tokio::net::TcpListener::bind(cluster.address())
        .await
        .unwrap();
    let shutdown = tokio_util::sync::CancellationToken::new();
    let runner = tokio::spawn(run(
        cluster.store(),
        std::env::temp_dir().join(format!("ployz-cert-loop-{}", MachineId::random())),
        None,
        machine_id("a"),
        shutdown.clone(),
    ));

    let (attempt, _) = tokio::time::timeout(Duration::from_secs(10), down.accept())
        .await
        .expect("the certificate loop never tried to subscribe")
        .unwrap();
    drop(attempt);
    drop(down);
    cluster.restart();
    subscribed(
        &cluster,
        1,
        "Corrosion down at the first subscribe stopped the loop",
    )
    .await;
    // Stopping severs the open subscription stream.
    cluster.stop();
    cluster.restart();
    subscribed(&cluster, 2, "a closed subscription stopped the loop").await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), runner)
        .await
        .expect("certificate loop ignored shutdown")
        .unwrap();
}

async fn subscribed(cluster: &FakeCluster, count: usize, stopped: &str) {
    tokio::time::timeout(Duration::from_secs(10), cluster.subscriptions(count))
        .await
        .expect(stopped);
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
fn machine_jitter_stays_inside_its_span() {
    let span = Duration::from_secs(3 * 86_400);
    let first = machine_jitter(&machine_id("a"), span);
    let second = machine_jitter(&machine_id("f"), span);
    assert!(first < span, "{first:?}");
    assert!(second < span, "{second:?}");
    assert_ne!(first, second);
    assert_eq!(machine_jitter(&machine_id("a"), span), first);
}

#[test]
fn every_machine_first_tries_to_renew_with_most_of_the_last_third_left() {
    let day = Duration::from_secs(86_400);
    let fraction = default_policy().renew_at_lifetime_fraction();
    for lifetime in [day * 90, day * 6] {
        let not_before = UNIX_EPOCH;
        let not_after = not_before + lifetime;
        let row = row_with_lifetime(not_before, not_after);
        let window = renewal_window(not_before, not_after, fraction).unwrap();
        let left_at_window = not_after.duration_since(window).unwrap();
        let dues: Vec<_> = (0..1_000_u64)
            .map(|n| {
                // Spread ids over the whole hash range, as random v4 ids are.
                let mixed = n.wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_left(29);
                let machine = MachineId::parse(format!("{mixed:016x}{n:016x}")).unwrap();
                super::renew_at(row.material().unwrap(), &machine, fraction).unwrap()
            })
            .collect();
        let first = *dues.iter().min().unwrap();
        let last = *dues.iter().max().unwrap();
        assert!(first >= window, "lifetime {lifetime:?}");
        let left = not_after.duration_since(last).unwrap();
        assert!(
            left >= left_at_window.mul_f64(0.9),
            "lifetime {lifetime:?}: {left:?} left"
        );
        let spread = last.duration_since(first).unwrap();
        assert!(
            spread >= left_at_window.mul_f64(0.09),
            "lifetime {lifetime:?}: Machines spread over only {spread:?}"
        );
    }
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
            parts.namespace = Namespace::system();
        })
        .unwrap();
    let mut down = observation(2, "ingress", Vec::new());
    down.try_update(|parts| {
        parts.machine_id = remote.id;
        parts.resolved_spec.name = ServiceName::parse("ingress").unwrap();
        parts.namespace = Namespace::system();
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
        rotation: None,
        r#gen: None,
        container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
        display_name: format!("{service_name}-{suffix}"),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse("a".repeat(32)).unwrap(),
        namespace: Namespace::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec,
        address: Some(ContainerAddress([10, 210, 1, 2].into())),
        labels: BTreeMap::new(),
        stopped: false,
        run: None,
    })
    .unwrap()
}

/// A hostname whose probe reaches no Machine of the replica: an IP literal, so `resolve_host`
/// asks no DNS server, and the verify probe gets no Machine id from `127.0.0.1:80`.
const UNREACHED: &str = "127.0.0.1";

/// A directory nothing serves, so a pass that pushes a hostname fails at `account` without
/// leaving the host.
const NO_DIRECTORY_SERVER: &str = "http://127.0.0.1:1/directory";

/// A replica whose `machines` rows are one per seed, and whose one Service Container serves
/// `hostname` over https.
async fn replica(seeds: &[&str], hostname: &str) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    replica_under(None, seeds, hostname).await
}

/// `replica`, with `policy` as its Certificate Policy row when given.
async fn replica_under(
    policy: Option<&str>,
    seeds: &[&str],
    hostname: &str,
) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    let db = corrosion_db();
    if let Some(policy) = policy {
        db.execute(
            "INSERT INTO cluster (key, value) VALUES (?, ?)",
            rusqlite::params![ployz_core::CERTIFICATE_POLICY_CLUSTER_KEY, policy],
        )
        .unwrap();
    }
    for (n, seed) in seeds.iter().enumerate() {
        let machine = machine_with_endpoint(seed, &format!("192.0.2.{}", n + 1));
        db.execute(
            "INSERT INTO machines (id, info) VALUES (?, ?)",
            rusqlite::params![
                machine.id.as_str(),
                serde_json::to_string(&machine).unwrap()
            ],
        )
        .unwrap();
    }
    let container = observation(1, "api", vec![ingress(hostname, HttpProtocol::Https)]);
    db.execute(
        "INSERT INTO containers (id, machine_id, container) VALUES (?, ?, ?)",
        rusqlite::params![
            container.container_id.to_string(),
            container.machine_id.to_string(),
            serde_json::to_string(&container).unwrap()
        ],
    )
    .unwrap();
    identity_store(db).await
}

/// The row holds a failed order's Authority clock, which passes `left` from now.
async fn authority_clock(store: &ReplicatedStore, hostname: &IngressHost, left: Duration) {
    store
        .record_certificate_failure(
            hostname,
            None,
            "order for the hostname is invalid",
            IssuanceClock::new(1, SystemTime::now() + left, IssuanceFailure::Authority),
        )
        .await
        .unwrap();
}

/// A shared hold is not this Machine's rank delay. Rank 2's delay is 60 s. Its first pass the
/// clock holds asks for a 60 s sleep. Sixty seconds on, the clock still holds, so the pass has
/// nothing to order and the hostname still owes the delay from the pass after the hold.
#[tokio::test]
async fn a_pass_the_shared_clock_holds_does_not_count_toward_the_rank_delay() {
    let hostname = host(UNREACHED);
    let (store, task) = replica(&["a", "b", "c"], UNREACHED).await;
    authority_clock(&store, &hostname, Duration::from_secs(3600)).await;
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-held-{}", MachineId::random()));
    let directory = Some(NO_DIRECTORY_SERVER.to_owned());
    let mut kept = BTreeMap::new();

    let first = issue_wanted(
        &store,
        &directory,
        &account_dir,
        &machine_id("c"),
        &mut kept,
    )
    .await
    .unwrap();
    assert!(
        first > Duration::from_secs(50) && first <= Duration::from_secs(60),
        "{first:?}"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    let second = issue_wanted(
        &store,
        &directory,
        &account_dir,
        &machine_id("c"),
        &mut kept,
    )
    .await
    .unwrap();
    assert_eq!(
        second, RETRY_INTERVAL,
        "rank 2 counted 60 s of the hold toward its rank delay: poll_wait answered RANK_STEP, its answer for an Order-due hostname"
    );
    task.abort();
}

/// A pass the shared clock holds wakes when the clock passes, not one rank step later.
#[tokio::test]
async fn a_held_pass_wakes_when_the_shared_clock_passes() {
    let hostname = host(UNREACHED);
    let (store, task) = replica(&["a", "b"], UNREACHED).await;
    authority_clock(&store, &hostname, Duration::from_secs(10)).await;
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-wake-{}", MachineId::random()));
    let mut kept = BTreeMap::new();

    let wait = issue_wanted(
        &store,
        &Some(NO_DIRECTORY_SERVER.to_owned()),
        &account_dir,
        &machine_id("a"),
        &mut kept,
    )
    .await
    .unwrap();
    assert!(
        wait <= Duration::from_secs(10) && wait > Duration::from_secs(5),
        "rank 0 is held 10 s, and asked to sleep {wait:?}"
    );
    task.abort();
}

/// Whichever rank's probe refuses a hostname records the refusal, so the row says why and
/// when the Cluster tries again, even when the lowest Machine id runs no certificate loop.
#[tokio::test]
async fn a_refusal_is_recorded_by_whichever_rank_sees_it() {
    let hostname = host(UNREACHED);
    // Machine "a" is rank 0 and runs no loop here, as a Down or leaving Machine does.
    let (store, task) = replica(&["a", "b"], UNREACHED).await;
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-refused-{}", MachineId::random()));

    issue_wanted(
        &store,
        &Some(NO_DIRECTORY_SERVER.to_owned()),
        &account_dir,
        &machine_id("b"),
        &mut BTreeMap::new(),
    )
    .await
    .unwrap();
    let row = store.certificate_row(&hostname).await.unwrap();
    assert!(
        row.last_error()
            .is_some_and(|reason| reason.contains(UNREACHED)),
        "rank 1 recorded no refusal: {row:?}"
    );
    assert!(
        row.clock()
            .is_some_and(|clock| matches!(clock.last_failure(), IssuanceFailure::Refused(_))),
        "rank 1 recorded no clock: {row:?}"
    );
    task.abort();
}

/// A pass that stops before the gate is blocked, as a held pass is: the policy refusal, a
/// policy with no directory, and an Err from a read before the gate each end every hostname's
/// rank delay, so the first pass through the gate after them starts it again.
#[tokio::test]
async fn a_pass_that_stops_before_the_gate_ends_the_rank_delay() {
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-early-{}", MachineId::random()));
    let directory = Some(NO_DIRECTORY_SERVER.to_owned());
    let waiting = || {
        BTreeMap::from([(
            host(UNREACHED),
            HostIssuance::Waiting {
                orderable_since: tokio::time::Instant::now(),
            },
        )])
    };
    for (policy, stops) in [
        (r#"{"challenge_kind":"dns-01"}"#, "the policy refusal"),
        (r#"{"directory_url":""}"#, "no directory"),
    ] {
        let (store, task) = replica_under(Some(policy), &["a", "b"], UNREACHED).await;
        let mut kept = waiting();
        let wait = issue_wanted(
            &store,
            &directory,
            &account_dir,
            &machine_id("b"),
            &mut kept,
        )
        .await
        .unwrap();
        assert_eq!(wait, RETRY_INTERVAL, "{stops}");
        assert!(kept.is_empty(), "{stops} kept {kept:?}");
        task.abort();
    }
    let (store, task) = replica(&["a", "b"], UNREACHED).await;
    task.abort();
    let _ = task.await;
    let mut kept = waiting();
    issue_wanted(
        &store,
        &directory,
        &account_dir,
        &machine_id("b"),
        &mut kept,
    )
    .await
    .unwrap_err();
    assert!(kept.is_empty(), "an Err before the gate kept {kept:?}");
}

fn reaches() -> HostnameVerdict {
    HostnameVerdict::ReachesCluster(ployz_core::ClusterRoute::Direct)
}

fn does_not_resolve() -> HostnameVerdict {
    HostnameVerdict::Refused(Refusal::DoesNotResolve)
}

/// A row with no material and a backoff clock of `last_failure` that passes at `next_attempt_at`.
fn clocked(last_failure: IssuanceFailure, next_attempt_at: SystemTime) -> CertificateRow {
    CertificateRow::default().with_backoff(
        "an earlier attempt failed",
        IssuanceClock::new(1, next_attempt_at, last_failure),
    )
}

/// One pass's `step` for the Machine of `rank` under the built-in policy.
fn stepped(
    prev: Option<HostIssuance>,
    row: Option<&CertificateRow>,
    verdict: HostnameVerdict,
    rank: usize,
    now: tokio::time::Instant,
    wall: SystemTime,
) -> HostIssuance {
    step(
        prev,
        row,
        verdict,
        rank,
        now,
        wall,
        &machine_id("a"),
        &default_policy(),
    )
}

#[test]
fn a_hold_or_a_refusal_starts_the_rank_delay_again_at_the_next_open_gate() {
    let now = tokio::time::Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let long_ago = now.checked_sub(Duration::from_secs(600)).unwrap();
    let passed = clocked(IssuanceFailure::Authority, wall - Duration::from_secs(1));
    let held = IssuanceClock::new(1, wall, IssuanceFailure::Authority);
    let refused = HostIssuance::Refused {
        refusal: Refusal::DoesNotResolve,
        clock: IssuanceClock::new(1, wall, IssuanceFailure::Refused(Refusal::DoesNotResolve)),
    };
    for prev in [None, Some(HostIssuance::Held(held)), Some(refused)] {
        assert_eq!(
            stepped(prev, Some(&passed), reaches(), 1, now, wall),
            HostIssuance::Waiting {
                orderable_since: now
            },
            "rank 1 after {prev:?}"
        );
        assert_eq!(
            stepped(prev, Some(&passed), reaches(), 0, now, wall),
            HostIssuance::Order(ployz_core::ClusterRoute::Direct),
            "rank 0 owes no delay after {prev:?}"
        );
    }
    let waiting = Some(HostIssuance::Waiting {
        orderable_since: long_ago,
    });
    assert_eq!(
        stepped(waiting, Some(&passed), reaches(), 1, now, wall),
        HostIssuance::Order(ployz_core::ClusterRoute::Direct),
        "a Waiting state keeps its start while the gate stays open"
    );
}

#[test]
fn the_rank_delay_runs_from_the_first_open_pass() {
    let start = tokio::time::Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let waiting = Some(HostIssuance::Waiting {
        orderable_since: start,
    });
    assert_eq!(
        stepped(
            waiting,
            None,
            reaches(),
            1,
            start + RANK_STEP - Duration::from_millis(1),
            wall
        ),
        HostIssuance::Waiting {
            orderable_since: start
        }
    );
    assert_eq!(
        stepped(waiting, None, reaches(), 1, start + RANK_STEP, wall),
        HostIssuance::Order(ployz_core::ClusterRoute::Direct)
    );
    assert_eq!(
        stepped(
            Some(HostIssuance::Order(ployz_core::ClusterRoute::Direct)),
            None,
            reaches(),
            1,
            start,
            wall
        ),
        HostIssuance::Order(ployz_core::ClusterRoute::Direct),
        "an order whose failure left no clock is retried at once, its delay spent"
    );
}

#[test]
fn a_clock_that_holds_ends_the_rank_delay() {
    let now = tokio::time::Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let waiting = Some(HostIssuance::Waiting {
        orderable_since: now.checked_sub(Duration::from_secs(600)).unwrap(),
    });
    let later = wall + Duration::from_secs(60);
    let authority = clocked(IssuanceFailure::Authority, later);
    let unresolved = clocked(IssuanceFailure::Refused(Refusal::DoesNotResolve), later);
    for verdict in [reaches(), does_not_resolve()] {
        assert_eq!(
            stepped(waiting, Some(&authority), verdict, 1, now, wall),
            HostIssuance::Held(authority.clock().unwrap()),
            "{verdict:?}"
        );
    }
    assert_eq!(
        stepped(waiting, Some(&unresolved), does_not_resolve(), 1, now, wall),
        HostIssuance::Held(unresolved.clock().unwrap())
    );
    assert_eq!(
        stepped(
            Some(HostIssuance::Held(unresolved.clock().unwrap())),
            Some(&unresolved),
            reaches(),
            1,
            now,
            wall
        ),
        HostIssuance::Waiting {
            orderable_since: now
        },
        "a changed verdict lifts a refusal's clock early, and the delay starts then"
    );
}

#[test]
fn a_refused_verdict_carries_the_clock_to_record() {
    let now = tokio::time::Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let policy = default_policy();
    for rank in [0, 1, 2] {
        assert_eq!(
            stepped(None, None, does_not_resolve(), rank, now, wall),
            HostIssuance::Refused {
                refusal: Refusal::DoesNotResolve,
                clock: IssuanceClock::new(
                    1,
                    wall + policy.backoff_base(),
                    IssuanceFailure::Refused(Refusal::DoesNotResolve)
                ),
            },
            "rank {rank}"
        );
    }
}

#[test]
fn material_is_issued_until_its_renewal_is_due_and_then_renewed_through_the_gate() {
    let day = Duration::from_secs(86_400);
    let row = row_with_lifetime(UNIX_EPOCH, UNIX_EPOCH + day * 90);
    let machine = machine_id("a");
    let policy = default_policy();
    let renew_at = super::renew_at(
        row.material().unwrap(),
        &machine,
        policy.renew_at_lifetime_fraction(),
    )
    .unwrap();
    let early = renew_at - Duration::from_secs(10);
    assert_eq!(
        issued(Some(&row), 0, early, &machine, &policy),
        Some(HostIssuance::Issued {
            renew_due: renew_at
        })
    );
    assert_eq!(issued(Some(&row), 0, renew_at, &machine, &policy), None);
    assert_eq!(issued(None, 0, early, &machine, &policy), None);
    let now = tokio::time::Instant::now();
    assert_eq!(
        stepped(None, Some(&row), reaches(), 0, now, renew_at),
        HostIssuance::Renew(ployz_core::ClusterRoute::Direct)
    );
    let clock = IssuanceClock::new(
        1,
        renew_at + Duration::from_secs(60),
        IssuanceFailure::Authority,
    );
    let backed_off = row.with_backoff("the renewal failed", clock);
    assert_eq!(
        stepped(None, Some(&backed_off), reaches(), 0, now, renew_at),
        HostIssuance::Held(clock)
    );
}

#[test]
fn a_held_hostname_wakes_when_its_clock_passes_or_at_its_next_probe() {
    let now = tokio::time::Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let machine = machine_id("a");
    let policy = default_policy();
    let wake_of = |state, rank| wake(state, rank, now, wall, &machine, &policy);
    let authority = |left| {
        HostIssuance::Held(IssuanceClock::new(
            1,
            wall + left,
            IssuanceFailure::Authority,
        ))
    };
    let unresolved = |left| {
        IssuanceClock::new(
            1,
            wall + left,
            IssuanceFailure::Refused(Refusal::DoesNotResolve),
        )
    };
    assert_eq!(
        wake_of(authority(Duration::from_secs(10)), 1),
        Duration::from_secs(10)
    );
    assert_eq!(
        wake_of(authority(Duration::from_secs(3600)), 1),
        RETRY_INTERVAL
    );
    assert_eq!(wake_of(authority(Duration::ZERO), 1), Duration::ZERO);
    assert_eq!(
        wake_of(HostIssuance::Held(unresolved(Duration::from_secs(10))), 1),
        Duration::from_secs(10)
    );
    assert_eq!(
        wake_of(HostIssuance::Held(unresolved(Duration::from_secs(3600))), 2),
        RANK_STEP,
        "a changed verdict lifts a refusal's clock, so it is probed as often as before"
    );
    assert_eq!(
        wake_of(
            HostIssuance::Refused {
                refusal: Refusal::DoesNotResolve,
                clock: unresolved(Duration::from_secs(3600)),
            },
            2
        ),
        RANK_STEP
    );
    assert_eq!(
        wake_of(
            HostIssuance::Waiting {
                orderable_since: now
            },
            2
        ),
        Duration::from_secs(60)
    );
    assert_eq!(
        wake_of(HostIssuance::Order(ployz_core::ClusterRoute::Direct), 1),
        RANK_STEP
    );
    assert_eq!(
        wake_of(
            HostIssuance::Issued {
                renew_due: wall + Duration::from_secs(10)
            },
            0
        ),
        Duration::from_secs(10)
    );
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
