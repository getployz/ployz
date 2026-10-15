//! Trace page facts in the projection and their rendering.

use super::{ingress, observation, projection};
use crate::docker::LABEL_DEPLOYMENT;
use crate::ingress::{IngressProjection, PublicationTrace};
use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, DeploymentLogId,
    HealthObservation, HttpProtocol, MachineId, Namespace, service_containers,
};
use std::collections::BTreeMap;
use std::num::NonZeroUsize;

fn machine(digit: char) -> MachineId {
    MachineId::parse(digit.to_string().repeat(32)).unwrap()
}

fn api(suffix: u8, machine_id: &MachineId, created_at: i64) -> ContainerObservation {
    let mut container = observation(
        suffix,
        machine_id,
        "api",
        Some([10, 210, 1, suffix]),
        vec![ingress("api.example", 80, HttpProtocol::Http)],
    );
    container
        .try_update(|parts| parts.created_at_unix_nanos = created_at)
        .unwrap();
    container
}

fn with(
    mut container: ContainerObservation,
    update: impl FnOnce(&mut ployz_core::ContainerObservationParts),
) -> ContainerObservation {
    container.try_update(update).unwrap();
    container
}

fn labeled(container: ContainerObservation, deploy: &str) -> ContainerObservation {
    with(container, |parts| {
        parts
            .labels
            .insert(LABEL_DEPLOYMENT.to_owned(), deploy.to_owned());
    })
}

fn derive(containers: Vec<ContainerObservation>) -> IngressProjection {
    projection(
        &machine('a'),
        "node-a",
        &service_containers(containers),
        &BTreeMap::new(),
    )
}

fn trace(projection: &IngressProjection) -> &PublicationTrace {
    &projection
        .sites
        .iter()
        .find(|site| site.hostname.as_str() == "api.example")
        .and_then(|site| site.publication.as_ref())
        .expect("api.example is published")
        .trace
}

fn placement(entries: &[(char, usize)]) -> BTreeMap<MachineId, NonZeroUsize> {
    entries
        .iter()
        .map(|(digit, count)| (machine(*digit), NonZeroUsize::new(*count).unwrap()))
        .collect()
}

fn running(health: HealthObservation) -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running { health }
}

#[test]
fn placement_counts_every_owner_container_in_any_state_and_excludes_hooks() {
    let projection = derive(vec![
        api(1, &machine('a'), 1),
        with(api(2, &machine('a'), 2), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 137 };
        }),
        with(api(3, &machine('b'), 3), |parts| {
            parts.runtime = running(HealthObservation::Unhealthy);
        }),
        with(api(4, &machine('c'), 4), |parts| {
            parts.runtime = running(HealthObservation::Starting);
        }),
        with(api(5, &machine('a'), 5), |parts| {
            parts.kind = ContainerKind::PreDeployHook;
        }),
    ]);

    assert_eq!(
        trace(&projection).placement,
        placement(&[('a', 2), ('b', 1), ('c', 1)])
    );
}

#[test]
fn placement_counts_rollout_overlap_and_stopped_leftovers() {
    let projection = derive(vec![
        labeled(api(1, &machine('a'), 1), "old"),
        labeled(api(2, &machine('a'), 2), "new"),
        with(labeled(api(3, &machine('b'), 0), "older"), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 0 };
        }),
    ]);

    let trace = trace(&projection);
    assert_eq!(trace.placement, placement(&[('a', 2), ('b', 1)]));
    assert_eq!(trace.deploy, Some(DeploymentLogId::parse("new").unwrap()));
}

#[test]
fn placement_counts_only_the_hostname_owner() {
    // The oldest publisher owns a contested hostname.
    let blog = with(api(9, &machine('b'), 9), |parts| {
        parts.namespace = Namespace::parse("blog").unwrap();
    });
    let projection = derive(vec![api(1, &machine('a'), 1), blog]);

    assert_eq!(trace(&projection).placement, placement(&[('a', 1)]));
}

#[test]
fn deploy_names_the_newest_container_even_when_it_is_stopped() {
    let projection = derive(vec![
        labeled(api(1, &machine('a'), 1), "running"),
        with(labeled(api(2, &machine('b'), 2), "crashed"), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
        }),
    ]);

    assert_eq!(
        trace(&projection).deploy,
        Some(DeploymentLogId::parse("crashed").unwrap())
    );
}

#[test]
fn deploy_never_borrows_an_older_sibling_s_label() {
    let unlabeled = derive(vec![
        labeled(api(1, &machine('a'), 1), "old"),
        api(2, &machine('a'), 2),
    ]);
    let malformed = derive(vec![
        labeled(api(1, &machine('a'), 1), "old"),
        labeled(api(2, &machine('a'), 2), "not a deploy id"),
    ]);

    assert_eq!(trace(&unlabeled).deploy, None);
    assert_eq!(trace(&malformed).deploy, None);
}

#[test]
fn deploy_breaks_creation_ties_by_container_id() {
    let containers = vec![
        labeled(api(2, &machine('a'), 7), "second"),
        labeled(api(1, &machine('a'), 7), "first"),
    ];
    let mut reversed = containers.clone();
    reversed.reverse();

    assert_eq!(
        trace(&derive(containers)).deploy,
        Some(DeploymentLogId::parse("second").unwrap())
    );
    assert_eq!(
        trace(&derive(reversed)).deploy,
        Some(DeploymentLogId::parse("second").unwrap())
    );
}

#[test]
fn health_changes_that_keep_endpoints_leave_the_projection_unchanged() {
    let before = derive(vec![
        api(1, &machine('a'), 1),
        with(api(2, &machine('b'), 2), |parts| {
            parts.runtime = running(HealthObservation::Starting);
        }),
        with(api(3, &machine('b'), 3), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 137 };
        }),
    ]);
    let after = derive(vec![
        with(api(1, &machine('a'), 1), |parts| {
            parts.runtime = running(HealthObservation::Failing);
        }),
        with(api(2, &machine('b'), 2), |parts| {
            parts.runtime = running(HealthObservation::Unhealthy);
        }),
        with(api(3, &machine('b'), 3), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
        }),
    ]);

    assert_eq!(before, after);
}

#[test]
fn count_and_label_changes_change_the_projection() {
    let base = || vec![labeled(api(1, &machine('a'), 1), "one")];
    let mut leftover = base();
    leftover.push(with(api(2, &machine('b'), 0), |parts| {
        parts.runtime = ContainerRuntimeObservation::Exited { code: 0 };
    }));
    let relabeled = vec![labeled(api(1, &machine('a'), 1), "two")];

    assert_ne!(derive(base()), derive(leftover));
    assert_ne!(derive(base()), derive(relabeled));
}

/// One site block exactly as rendered, from its address line to its closing brace.
fn site_text(caddyfile: &str, address: &str) -> String {
    let start = caddyfile
        .find(&format!("\n{address} {{\n"))
        .unwrap_or_else(|| panic!("missing site {address}"))
        + 1;
    let end = start + caddyfile[start..].find("\n}\n").unwrap() + 3;
    caddyfile[start..end].to_owned()
}

#[test]
fn sites_import_the_trace_page_with_their_facts() {
    let caddyfile = super::render_caddyfile(&super::renderer_projection(), "TIMESTAMP");
    let a = "a".repeat(32);
    let b = "b".repeat(32);

    assert_eq!(
        site_text(&caddyfile, "http://example.com"),
        format!(
            "http://example.com {{
\timport ployz_headers {a}
\timport ployz_trace {a} dep_7 {a}:2,{b}:1 ok
\thandle /.well-known/acme-challenge/ployz-verify {{
\t\trespond \"{a}\" 200
\t}}
\thandle /.well-known/acme-challenge/LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0 {{
\t\trespond \"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\" 200
\t}}
\treverse_proxy 10.210.1.2:8080 10.210.2.2:8080 {{
\t\timport common_proxy
\t}}
\tlog_append upstream {{http.reverse_proxy.upstream.hostport}}
\timport error_page {a}
\tlog
}}
"
        )
    );
    assert_eq!(
        site_text(&caddyfile, "http://empty.example.com"),
        format!(
            "http://empty.example.com {{
\timport ployz_headers {a}
\timport ployz_trace {a} dep_7 {a}:2,{b}:1 NOT_RUNNING
\thandle /.well-known/acme-challenge/ployz-verify {{
\t\trespond \"{a}\" 200
\t}}
\terror NOT_RUNNING 503
\timport error_page {a}
\tlog
}}
"
        )
    );
    assert_eq!(
        site_text(&caddyfile, "http://"),
        format!(
            "http:// {{
\timport ployz_headers {a}
\timport ployz_trace {a} \"\" \"\" NOT_DEPLOYED
\thandle /.ployz-verify {{
\t\trespond \"{a}\" 200
\t}}
\thandle /.well-known/acme-challenge/ployz-verify {{
\t\trespond \"{a}\" 200
\t}}
\terror NOT_DEPLOYED 404
\timport error_page {a}
\tlog
}}
"
        )
    );
    assert_eq!(
        site_text(&caddyfile, "https://"),
        format!(
            "https:// {{
\timport ployz_headers {a}
\timport ployz_trace {a} \"\" \"\" NOT_DEPLOYED
\terror NOT_DEPLOYED 404
\timport error_page {a}
\tlog
}}
"
        )
    );
    assert!(
        site_text(&caddyfile, "http://secure.example.com")
            .contains(&format!("\timport ployz_trace {a} dep_7 {a}:2,{b}:1 ok\n"))
    );
}

#[test]
fn trace_bodies_list_the_fixed_keys_and_end_with_a_newline() {
    let caddyfile = super::render_caddyfile(&super::renderer_projection(), "TIMESTAMP");
    let direct = "server={args[0]}
deploy={args[1]}
replicas={args[2]}
status={args[3]}
ip={http.request.remote.host}
http={http.request.proto}
tls={ployz_trace_tls}
sni={ployz_trace_sni}
request_id={http.request.uuid}
ts={time.now.unix}
";
    let cloudflare = format!(
        "{direct}via=cloudflare
cf_ray={{ployz_trace_ray}}
client_ip={{client_ip}}
"
    );

    assert!(caddyfile.contains(&format!(
        "\t\t\trespond <<TRACE_DIRECT\n{direct}\nTRACE_DIRECT 200\n"
    )));
    assert!(caddyfile.contains(&format!(
        "\t\t\trespond <<TRACE_CF\n{cloudflare}\nTRACE_CF 200\n"
    )));
    assert!(caddyfile.contains(
        "\t@ployz_trace_request {\n\t\tmethod GET\n\t\tpath /.well-known/ployz/trace\n\t}\n"
    ));
    assert!(caddyfile.contains("\t\t@ployz_trace_cloudflare remote_ip 173.245.48.0/20 "));
    assert!(caddyfile.contains(
        "(ployz_headers) {
\theader {
\t\t>ployz-server {args[0]}
\t\t>ployz-request-id {http.request.uuid}
\t}
\tlog_append request_id {http.request.uuid}
}
"
    ));
}

#[test]
fn trace_reports_the_client_address_caddy_trusts() {
    let caddyfile = super::render_caddyfile(&super::renderer_projection(), "TIMESTAMP");

    let snippet = site_text(&caddyfile, "(ployz_trace)");
    assert!(snippet.contains("\nclient_ip={client_ip}\n"), "{snippet}");
    assert!(
        !snippet.contains("CF-Connecting-IP"),
        "only the trusted_proxies check may read the header: {snippet}"
    );
}

#[test]
fn every_site_and_the_error_page_set_the_ployz_headers() {
    let caddyfile = super::render_caddyfile(&super::renderer_projection(), "TIMESTAMP");
    let a = "a".repeat(32);
    let sites = caddyfile
        .lines()
        .filter(|line| line.starts_with("http") && line.ends_with(" {"))
        .map(|line| line.trim_end_matches(" {"))
        .collect::<Vec<_>>();

    assert_eq!(sites.len(), 6, "{sites:?}");
    for address in sites {
        let site = site_text(&caddyfile, address);
        assert!(
            site.contains(&format!("\timport ployz_headers {a}\n")),
            "{site}"
        );
        assert!(
            site.contains(&format!("\timport ployz_trace {a} ")),
            "{site}"
        );
    }
    let error_page = &caddyfile[caddyfile.find("(error_page) {").unwrap()..];
    assert!(error_page.contains("\t\timport ployz_headers {args[0]}\n"));
}

/// Every whitespace token that reads as `ip:port`.
fn upstream_tokens(caddyfile: &str) -> std::collections::BTreeSet<String> {
    caddyfile
        .split_whitespace()
        .filter(|token| token.parse::<std::net::SocketAddr>().is_ok())
        .map(ToOwned::to_owned)
        .collect()
}

#[test]
fn a_withdrawn_container_leaves_the_file_but_stays_in_the_placement() {
    let a = machine('a');
    let b = machine('b');
    let serving = vec![api(1, &a, 1), api(2, &b, 2)];
    let withdrawn = vec![
        api(1, &a, 1),
        with(api(2, &b, 2), |parts| {
            parts.runtime = ContainerRuntimeObservation::Exited { code: 0 };
        }),
    ];

    let before = super::render_caddyfile(&derive(serving), "TIMESTAMP");
    let projection = derive(withdrawn);
    let after = super::render_caddyfile(&projection, "TIMESTAMP");

    assert!(upstream_tokens(&before).contains("10.210.1.2:80"));
    assert_eq!(upstream_tokens(&after), ["10.210.1.1:80".to_owned()].into());
    assert_eq!(
        trace(&projection).placement,
        placement(&[('a', 1), ('b', 1)])
    );
    assert!(after.contains(&format!(" {a}:1,{b}:1 ok\n")));
}
