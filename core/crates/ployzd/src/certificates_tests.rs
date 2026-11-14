use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ployz_core::{
    CertificateKeyType, CertificatePolicy, ContainerAddress, ContainerId, ContainerKind,
    ContainerObservation, ContainerRuntimeObservation, DEFAULT_RENEW_AT_LIFETIME_FRACTION,
    HealthObservation, HttpProtocol, IngressHost, IssuanceClock, IssuanceFailure, MACHINE_API_PORT,
    Machine, MachineId, Namespace, PortPublication, ResolvedServiceSpec, ServiceId, ServiceName,
    resolve_certificate_policy,
};
use ployz_decide::issuance::{
    Held, Issuance, IssuanceAction, Pass, RANK_STEP, RETRY_INTERVAL, action_due, issuance_action,
    issued, wake,
};
use serde_json::json;

use super::{
    DoorStanding, Host, advance, challenge_probe_addresses, directory_from_env, door_standing,
    held, ingress_challenge_ips, issue_wanted, machine_jitter, material_validity,
    order_certificate, ordering_machines, renewal_window, run, unwritten, verify_answers,
    wait_for_http01, wanted_certificate_hosts,
};
use crate::corrosion::{
    AcmeWrite, CertificateChallenge, CertificateMaterial, CertificateRow, ReplicatedStore,
    fake_cluster::FakeCluster,
    store_tests::{corrosion_db, identity_store},
};

#[test]
fn a_write_that_moved_says_what_it_did_not_record() {
    let host = IngressHost::parse("app.example.com").unwrap();
    for (what, write, tried, line) in [
        (
            "record the certificate policy refusal",
            AcmeWrite::Moved,
            false,
            Some(
                "did not record the certificate policy refusal for app.example.com: its row changed on every attempt",
            ),
        ),
        (
            "record why no certificate is ordered",
            AcmeWrite::Moved,
            false,
            Some(
                "did not record why no certificate is ordered for app.example.com: its row changed on every attempt",
            ),
        ),
        (
            "publish the HTTP-01 challenge",
            AcmeWrite::Moved,
            false,
            Some(
                "did not publish the HTTP-01 challenge for app.example.com: its row changed on every attempt",
            ),
        ),
        (
            "record the certificate issued",
            AcmeWrite::Moved,
            false,
            Some(
                "did not record the certificate issued for app.example.com: its row changed on every attempt",
            ),
        ),
        (
            "record certificate refusal",
            AcmeWrite::Moved,
            false,
            Some(
                "did not record certificate refusal for app.example.com: its row changed on every attempt",
            ),
        ),
        (
            "record certificate refusal",
            AcmeWrite::Moved,
            true,
            Some(
                "did not record certificate refusal for app.example.com: its row no longer holds the failed material as ACME material",
            ),
        ),
        (
            "record certificate authority failure",
            AcmeWrite::Moved,
            false,
            Some(
                "did not record certificate authority failure for app.example.com: its row changed on every attempt",
            ),
        ),
        (
            "record certificate authority failure",
            AcmeWrite::Moved,
            true,
            Some(
                "did not record certificate authority failure for app.example.com: its row no longer holds the failed material as ACME material",
            ),
        ),
        ("record certificate refusal", AcmeWrite::Changed, true, None),
        ("record certificate refusal", AcmeWrite::Kept, false, None),
    ] {
        assert_eq!(
            unwritten(&host, write, what, tried).as_deref(),
            line,
            "{what} {write:?} {tried}"
        );
    }
}

fn policy_for(directory: &str) -> CertificatePolicy {
    CertificatePolicy::built_in(Some(directory.to_owned()))
}

fn default_policy() -> CertificatePolicy {
    CertificatePolicy::built_in(None)
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
fn unparseable_stored_material_does_not_suppress_repair() {
    let row = CertificateRow::decode(r#"{"certificate":"CERT","private_key":"KEY"}"#).unwrap();
    assert!(row.material().is_none());
    assert!(row.last_error().unwrap().contains("invalid"));
    assert_eq!(
        action(Some(&row), 0, Duration::from_secs(60)),
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
            action_at(&row, 0, window - Duration::from_secs(1), &machine, &policy),
            IssuanceAction::Nothing,
            "lifetime {lifetime:?}"
        );
        assert_eq!(
            action_at(&row, 0, not_after, &machine, &policy),
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
        action_at(&row, 1, due, &machine, &policy),
        IssuanceAction::Nothing
    );
    assert_eq!(
        action_at(
            &row,
            1,
            due + RANK_STEP - Duration::from_millis(1),
            &machine,
            &policy
        ),
        IssuanceAction::Nothing
    );
    assert_eq!(
        action_at(&row, 1, due + RANK_STEP, &machine, &policy),
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
        action_at(&row, 0, earlier, later_machine, &policy),
        IssuanceAction::Nothing
    );
}

#[test]
fn the_wait_follows_each_certificate_lifetime() {
    let day = Duration::from_secs(86_400);
    let machine = machine_id("a");
    let policy = default_policy();
    let long = row_with_lifetime(UNIX_EPOCH, UNIX_EPOCH + day * 90);
    let short = row_with_lifetime(UNIX_EPOCH, UNIX_EPOCH + day * 6);
    let wait = |row, now| {
        action_due(
            held(Some(row), &machine, &policy),
            0,
            Duration::ZERO,
            now,
            &policy,
        )
        .1
        .min(RETRY_INTERVAL)
    };
    let now = UNIX_EPOCH + Duration::from_secs(60);
    assert_eq!(wait(&long, now), Duration::from_secs(60));
    assert_eq!(wait(&short, now), Duration::from_secs(60));
    let near_short = super::renew_at(
        short.material().unwrap(),
        &machine,
        policy.renew_at_lifetime_fraction(),
    )
    .unwrap()
        - Duration::from_secs(10);
    assert_eq!(wait(&short, near_short), Duration::from_secs(10));
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

fn door_on(machine: &Machine, spec: &ResolvedServiceSpec) -> ContainerObservation {
    let mut door = observation(4, spec.name.as_str(), Vec::new());
    door.try_update(|parts| {
        parts.machine_id = machine.id;
        parts.namespace = Namespace::system();
        parts.resolved_spec = spec.clone();
    })
    .unwrap();
    door
}

#[test]
fn a_server_with_the_public_door_closed_is_never_ranked_or_probed() {
    let open = machine_with_endpoint("a", "192.0.2.1");
    let closed = machine_with_endpoint("b", "192.0.2.2");
    let proxy = ServiceId::parse("c".repeat(32)).unwrap();
    let token = ployz_core::Ciphertext::from_bytes(vec![7; 48]);
    let public = crate::images::DoorSpecs::public(proxy);
    let tunnel =
        crate::images::DoorSpecs::new(proxy, ployz_core::IngressPorts::Closed, Some(&token));
    let observations = [
        door_on(&open, &public.holder),
        door_on(&open, &public.proxy),
        door_on(&closed, &tunnel.holder),
        door_on(&closed, &tunnel.proxy),
        door_on(&closed, &tunnel.tunnel.as_ref().unwrap().connector),
    ];
    let standing = door_standing(&observations);
    assert_eq!(
        standing,
        DoorStanding {
            public: BTreeSet::from([open.id]),
            closed: BTreeSet::from([closed.id]),
        }
    );
    let machines = [open.clone(), closed.clone()];
    assert_eq!(
        ordering_machines(&machines, &standing).collect::<Vec<_>>(),
        [&open.id]
    );
    assert_eq!(
        ingress_challenge_ips(&machines, &observations),
        BTreeSet::from([ip("192.0.2.1")]),
        "a closed Server answers no challenge"
    );

    let both = [
        door_on(&closed, &tunnel.holder),
        door_on(&closed, &public.holder),
    ];
    assert!(
        door_standing(&both).closed.is_empty(),
        "a Server mid-switch, with a public holder still up, is open"
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
        HTTP01_WAIT,
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
    let client = ployz_core::HostnameProbe::new(Duration::from_secs(2)).unwrap();

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
            HTTP01_WAIT,
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

/// What the loop's scheduling answers for `row` as Machine "a" under the built-in policy.
fn action(row: Option<&CertificateRow>, rank: usize, elapsed: Duration) -> IssuanceAction {
    let policy = default_policy();
    issuance_action(
        held(row, &machine_id("a"), &policy),
        rank,
        elapsed,
        UNIX_EPOCH,
        &policy,
    )
}

/// As `action` at `now`, with no rank delay run.
fn action_at(
    row: &CertificateRow,
    rank: usize,
    now: SystemTime,
    machine: &MachineId,
    policy: &CertificatePolicy,
) -> IssuanceAction {
    issuance_action(
        held(Some(row), machine, policy),
        rank,
        Duration::ZERO,
        now,
        policy,
    )
}

/// How long the tests give a challenge to be served.
const HTTP01_WAIT: Duration = Duration::from_secs(30);

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

/// Only rank 0 records a refusal. Were rank 1 to record one, it would replace a passed
/// Authority clock at one failure, so rank 0's next failed order would count its backoff from
/// one again.
#[tokio::test]
async fn only_rank_0_records_a_refusal() {
    let hostname = host(UNREACHED);
    let (store, task) = replica(&["a", "b"], UNREACHED).await;
    let backed_off = IssuanceClock::new(
        3,
        SystemTime::now() - Duration::from_secs(1),
        IssuanceFailure::Authority,
    );
    store
        .record_certificate_failure(
            &hostname,
            None,
            "order for the hostname is invalid",
            backed_off,
        )
        .await
        .unwrap();
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-refused-{}", MachineId::random()));
    let directory = Some(NO_DIRECTORY_SERVER.to_owned());
    let pass = |seed| {
        let (store, directory, account_dir) = (&store, &directory, &account_dir);
        async move {
            issue_wanted(
                store,
                directory,
                account_dir,
                &machine_id(seed),
                &mut BTreeMap::new(),
            )
            .await
            .unwrap();
            store.certificate_row(&host(UNREACHED)).await.unwrap()
        }
    };

    let seeded = store.certificate_row(&hostname).await.unwrap();
    let row = pass("b").await;
    assert_eq!(row, seeded, "rank 1 recorded");
    let row = pass("a").await;
    assert!(
        row.last_error()
            .is_some_and(|reason| reason.contains(UNREACHED)),
        "rank 0 recorded no refusal: {row:?}"
    );
    task.abort();
}

/// A pass that stops before the gate because the policy refuses or names no directory is
/// blocked, as a held pass is: it ends every hostname's rank delay, so the first pass through
/// the gate after it starts the delay again.
#[tokio::test]
async fn a_pass_that_stops_before_the_gate_ends_the_rank_delay() {
    let account_dir =
        std::env::temp_dir().join(format!("ployzd-acme-early-{}", MachineId::random()));
    let directory = Some(NO_DIRECTORY_SERVER.to_owned());
    for (policy, stops) in [
        (r#"{"challenge_kind":"dns-01"}"#, "the policy refusal"),
        (r#"{"directory_url":""}"#, "no directory"),
    ] {
        let (store, task) = replica_under(Some(policy), &["a", "b"], UNREACHED).await;
        let mut kept = BTreeMap::from([(
            host(UNREACHED),
            Host {
                issuance: Issuance::Waiting,
                since: tokio::time::Instant::now(),
            },
        )]);
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
}

/// A read that fails keeps the delay a hostname has earned, so passes that alternate between
/// an open gate and an Err still order once the delay has passed.
#[tokio::test]
async fn a_read_error_keeps_the_rank_delay_earned() {
    let account_dir = std::env::temp_dir().join(format!("ployzd-acme-err-{}", MachineId::random()));
    let (store, task) = replica(&["a", "b"], UNREACHED).await;
    task.abort();
    let _ = task.await;
    let start = tokio::time::Instant::now();
    let wall = SystemTime::now();
    let mut kept = BTreeMap::new();
    for pass in 1..=3_u32 {
        let now = start + RANK_STEP / 2 * pass;
        let prev = kept.get(&host(UNREACHED)).copied();
        kept.insert(host(UNREACHED), stepped(prev, 1, now, wall));
        let before = kept.clone();
        issue_wanted(
            &store,
            &Some(NO_DIRECTORY_SERVER.to_owned()),
            &account_dir,
            &machine_id("b"),
            &mut kept,
        )
        .await
        .unwrap_err();
        assert_eq!(kept, before, "an Err before the gate changed the states");
    }
    assert_eq!(
        kept.get(&host(UNREACHED)).copied(),
        Some(Host {
            issuance: Issuance::Order(ployz_core::ClusterRoute::Direct),
            since: start + RANK_STEP / 2,
        }),
        "rank 1 orders a step after its first open pass, though every other pass failed"
    );
}

/// One open pass for the Machine of `rank` that holds no material, under the built-in policy.
fn stepped(prev: Option<Host>, rank: usize, now: tokio::time::Instant, wall: SystemTime) -> Host {
    let policy = default_policy();
    advance(
        prev,
        now,
        Pass {
            gate: ployz_core::IssuanceGate::Order(ployz_core::ClusterRoute::Direct),
            held: Held::Nothing,
            rank,
            wall,
            policy: &policy,
        },
    )
}

/// The loop's own clock: a rank delay that began a step ago has passed, and a hold between
/// two passes starts it over.
#[test]
fn the_rank_delay_runs_on_the_loops_instants() {
    let start = tokio::time::Instant::now();
    let wall = UNIX_EPOCH + Duration::from_secs(1_000_000);
    let first = stepped(None, 1, start, wall);
    assert_eq!(
        first,
        Host {
            issuance: Issuance::Waiting,
            since: start
        }
    );
    let policy = default_policy();
    assert_eq!(
        wake(first.issuance, 1, first.orderable_for(start), wall, &policy),
        RANK_STEP
    );
    let later = start + RANK_STEP;
    assert_eq!(
        stepped(Some(first), 1, later, wall),
        Host {
            issuance: Issuance::Order(ployz_core::ClusterRoute::Direct),
            since: start
        },
        "an order keeps the start of its delay"
    );
    let held = Host {
        issuance: Issuance::Held(IssuanceClock::new(1, wall, IssuanceFailure::Authority)),
        since: later,
    };
    assert_eq!(
        stepped(Some(held), 1, later + RANK_STEP * 2, wall),
        Host {
            issuance: Issuance::Waiting,
            since: later + RANK_STEP * 2
        },
        "the first open pass after a hold starts the delay again"
    );
}

#[test]
fn material_reaches_scheduling_with_this_machines_renewal() {
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
    assert_eq!(held(None, &machine, &policy), Held::Nothing);
    assert_eq!(
        held(Some(&CertificateRow::default()), &machine, &policy),
        Held::Nothing
    );
    let held = held(Some(&row), &machine, &policy);
    assert_eq!(
        held,
        Held::Material {
            renew_at: Some(renew_at)
        }
    );
    assert_eq!(
        issued(held, 0, renew_at - Duration::from_secs(10), &policy),
        Some(Issuance::Issued {
            renew_due: renew_at
        })
    );
    assert_eq!(issued(held, 0, renew_at, &policy), None);
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
