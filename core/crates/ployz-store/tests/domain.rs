#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
//! Public domains through the Store's interface: adding and removing generated and
//! custom ones, the custom-domain capability, Cluster Domain expansion at admission,
//! and each domain's status from Cloud's evidence. On SQLite and on Postgres (see
//! `backend`).

use ployz_core::{DeployOutcome, DeployPreview, RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, AddDomain, Admit, Cancel, ClusterDomain, ClusterDomainStatus, Command, ConfigStore,
    CreateEnvironment, CreateProject, CreateService, Deploy, DeploymentId, DnsLookup, DomainAction,
    DomainEvidence, DomainQuery, DomainRow, DomainStatus, DomainsQuery, EnvironmentId,
    EnvironmentName, EnvironmentRef, Hostname, OrganizationId, PlanQuery, ProjectId, ProjectName,
    Query, RemoveDomain, Retry, RunEvidence, RunnerId, ServiceLineageId, Trusted, View, Written,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// Project `shop` with Service `web` in `production`, and Service `web` in `staging`.
fn shop() -> (ConfigStore, Actor) {
    let store = backend::open();
    let who = Actor::system(OrganizationId::parse("org").unwrap());
    store
        .write(
            &who,
            &CreateProject {
                id: ProjectId::parse(uuid(1)).unwrap(),
                name: ProjectName::parse("shop").unwrap(),
                default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &CreateEnvironment {
                id: EnvironmentId::parse(uuid(3)).unwrap(),
                project: None,
                name: EnvironmentName::parse("staging").unwrap(),
            },
        )
        .unwrap();
    for (n, environment) in [(4, None), (5, Some("staging"))] {
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse(uuid(n)).unwrap(),
                    environment: at(environment),
                    name: ServiceName::parse("web").unwrap(),
                    image: Some("nginx:1".into()),
                },
            )
            .unwrap();
    }
    (store, who)
}

fn at(environment: Option<&str>) -> EnvironmentRef {
    EnvironmentRef {
        project: None,
        environment: environment.map(|name| EnvironmentName::parse(name).unwrap()),
    }
}

fn host(name: &str) -> Hostname {
    Hostname::parse(name).unwrap()
}

fn add(hostname: Option<&str>, port: Option<u16>) -> AddDomain {
    AddDomain {
        environment: EnvironmentRef::default(),
        service: ServiceName::parse("web").unwrap(),
        hostname: hostname.map(host),
        port,
    }
}

/// Cloud's evidence: whether the Organization has Pro, and a ready Cluster Domain.
fn cloud(pro: bool) -> Trusted {
    Trusted {
        domains: DomainEvidence {
            custom_domains: pro,
            cluster_domain: Some(ClusterDomain {
                name: host("acme.ployz.app"),
                status: ClusterDomainStatus::Ready,
            }),
            certificates: Some(Vec::new()),
            ingress_addresses: vec!["203.0.113.7".into()],
            lookups: Vec::new(),
        },
        ..Trusted::default()
    }
}

fn rows(store: &ConfigStore, who: &Actor, trusted: &Trusted) -> Vec<DomainRow> {
    store
        .read_trusted(who, &DomainsQuery::default(), trusted)
        .unwrap()
        .domains
}

#[test]
fn a_generated_domain_is_one_per_service_and_unique_in_the_organization() {
    let (store, who) = shop();
    let added = store
        .write_trusted(&who, &add(None, None), &cloud(false))
        .unwrap();
    assert_eq!(added.domain.shown(), "web.acme.ployz.app");
    assert_eq!(added.staged.len(), 1);
    // Adding it again keeps it; a port retargets it.
    let again = store
        .write_trusted(&who, &add(None, None), &cloud(false))
        .unwrap();
    assert!(again.staged.is_empty());
    assert_eq!(again.environment.revision, added.environment.revision);
    let retargeted = store
        .write_trusted(&who, &add(None, Some(8080)), &cloud(false))
        .unwrap();
    assert_eq!(retargeted.domain.port, Some(8080));

    // Staging's web shares the Cluster Domain, so it takes the next prefix.
    let staging = store
        .write_trusted(
            &who,
            &AddDomain {
                environment: at(Some("staging")),
                ..add(None, None)
            },
            &Trusted::default(),
        )
        .unwrap();
    // Without a reserved Cluster Domain it has a prefix but no hostname yet.
    assert_eq!(
        serde_json::to_value(&staging.domain).unwrap(),
        json!({ "service": "web", "kind": "generated", "prefix": "web-2", "hostname": null, "port": null })
    );
}

#[test]
fn custom_domains_need_the_capability_to_add_or_retarget() {
    let (store, who) = shop();
    let refused = store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud(false))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Unsupported);
    assert_eq!(refused.details["next"], "ployz billing upgrade");

    let added = store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud(true))
        .unwrap();
    assert_eq!(added.domain.shown(), "app.example.com");
    // The same domain again needs nothing; a new port is a retarget, which does.
    let same = store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud(false))
        .unwrap();
    assert!(same.staged.is_empty());
    let retarget = store
        .write_trusted(
            &who,
            &add(Some("app.example.com"), Some(3000)),
            &cloud(false),
        )
        .unwrap_err();
    assert_eq!(retarget.code, RpcErrorCode::Unsupported);

    // Another Service, in any Environment, can't take it.
    let taken = store
        .write_trusted(
            &who,
            &AddDomain {
                environment: at(Some("staging")),
                ..add(Some("app.example.com"), None)
            },
            &cloud(true),
        )
        .unwrap_err();
    assert_eq!(taken.code, RpcErrorCode::Conflict);
}

#[test]
fn a_domain_is_removed_by_hostname_or_prefix() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud(true))
        .unwrap();
    store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud(true))
        .unwrap();
    let remove = |domain: &str, trusted: &Trusted| {
        store.write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: domain.into(),
            },
            trusted,
        )
    };
    let missing = remove("nope.example.com", &cloud(true)).unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert_eq!(
        missing.details["valid_children"],
        json!(["app.example.com", "web.acme.ployz.app"])
    );
    assert!(!missing.message.contains("nope"));
    remove("App.Example.com", &cloud(true)).unwrap();
    // Without the Cluster Domain, a generated domain goes by its prefix.
    let removed = remove("web", &Trusted::default()).unwrap();
    assert_eq!(removed.staged.len(), 1);
    assert!(rows(&store, &who, &cloud(true)).is_empty());
}

fn deployment(n: u8) -> DeploymentId {
    DeploymentId::parse(uuid(90 + n)).unwrap()
}

fn admit(
    store: &ConfigStore,
    who: &Actor,
    n: u8,
    trusted: &Trusted,
) -> Result<Written, ployz_core::RpcError> {
    store.write_trusted(
        who,
        &Command::Admit(Admit::Deploy(Deploy {
            id: deployment(n),
            environment: EnvironmentRef::default(),
            services: Vec::new(),
            version: None,
            upload: None,
            accept_volume_loss: Vec::new(),
            message: None,
        })),
        trusted,
    )
}

/// Run Deployment `n` to a success that confirms `web`.
fn apply(store: &ConfigStore, n: u8) -> ployz_core::DeployIntent {
    let runner = RunnerId::parse("runner").unwrap();
    let claimed = store.claim(&deployment(n), &runner).unwrap();
    let operation = json!({"type": "remove_container", "machine_id": "a".repeat(32), "container_id": "a".repeat(64)});
    let preview: DeployPreview = serde_json::from_value(json!({
        "namespace": "shop-production",
        "operations": [{ "index": 0, "machine_id": "a".repeat(32), "service_name": "web",
            "operation": operation, "status": {"type": "pending"} }],
        "warnings": [], "would_remove": [], "preserved_volumes": []
    }))
    .unwrap();
    store
        .record(&deployment(n), &runner, RunEvidence::Prepared(preview))
        .unwrap();
    let outcome: DeployOutcome<ployz_core::ExecutionError> =
        serde_json::from_value(json!({ "type": "success", "completed": [operation] })).unwrap();
    store
        .record(
            &deployment(n),
            &runner,
            RunEvidence::Executed {
                outcome: Box::new(outcome),
                removed: Vec::new(),
            },
        )
        .unwrap();
    claimed.intent
}

#[test]
fn a_generated_domain_deploys_under_the_cluster_domain_frozen_at_admission() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud(false))
        .unwrap();
    // A plan checks everything but the Cluster Domain, which only Cloud holds.
    store.read(&who, &PlanQuery::default()).unwrap();
    let refused = admit(&store, &who, 1, &Trusted::default()).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Unsupported);

    admit(&store, &who, 1, &cloud(false)).unwrap();
    assert_eq!(
        rows(&store, &who, &cloud(false))[0].reason.as_deref(),
        Some("Deploying")
    );
    let intent = serde_json::to_value(apply(&store, 1)).unwrap();
    assert!(
        intent.to_string().contains("\"web.acme.ployz.app\""),
        "{intent}"
    );
    let row = &rows(&store, &who, &cloud(false))[0];
    assert_eq!(
        (row.status, row.action.clone()),
        (DomainStatus::Ready, None)
    );
}

#[test]
fn a_check_sees_fixed_dns_before_the_certificate_retries() {
    let (store, who) = shop();
    let staged = rows(&store, &who, &cloud(true));
    assert!(staged.is_empty());
    store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud(true))
        .unwrap();
    let before = &rows(&store, &who, &cloud(true))[0];
    assert_eq!(
        (before.status, before.action.clone()),
        (DomainStatus::SettingUp, Some(DomainAction::Deploy))
    );
    admit(&store, &who, 1, &cloud(true)).unwrap();
    apply(&store, 1);

    let mut observed = cloud(true);
    observed.domains.certificates = Some(vec![
        serde_json::from_value(json!({
            "hostname": "app.example.com", "status": "failure",
            "backoff": { "failure_kind": "does_not_resolve", "next_attempt_at": "2026-09-29T12:00:00Z", "failures": 2 },
        }))
        .unwrap(),
    ]);
    let waiting = &rows(&store, &who, &observed)[0];
    assert_eq!(waiting.status, DomainStatus::NeedsAttention);
    assert_eq!(
        serde_json::to_value(&waiting.action).unwrap(),
        json!({ "type": "dns", "records": [{ "type": "CNAME", "name": "app", "value": "acme.ployz.app" }] })
    );

    // `domain check` looks DNS up afresh, through the generic read.
    observed.domains.lookups = vec![DnsLookup {
        hostname: host("app.example.com"),
        cname: Some("acme.ployz.app.".into()),
        addresses: vec!["203.0.113.7".into()],
    }];
    let query = Query::Domain(DomainQuery {
        environment: EnvironmentRef::default(),
        domain: "app.example.com".into(),
    });
    let View::Domain(checked) = store.read_trusted(&who, &query, &observed).unwrap() else {
        panic!("a domain query answers a domain view");
    };
    assert_eq!(checked.domain.status, DomainStatus::SettingUp);
    assert_eq!(
        checked.domain.reason.as_deref(),
        Some("DNS points here now; the certificate is retried at 2026-09-29T12:00:00Z")
    );
    let wire: Value = serde_json::to_value(&checked).unwrap();
    assert_eq!(wire["domain"]["kind"], "custom");
    assert_eq!(wire["domain"]["status"], "setting_up");
}

#[test]
fn a_retry_ships_the_cluster_domain_its_source_froze() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud(false))
        .unwrap();
    admit(&store, &who, 1, &cloud(false)).unwrap();
    store
        .write(
            &who,
            &Cancel {
                deployment: deployment(1),
            },
        )
        .unwrap();
    // The retry carries no evidence: it inherits what Deployment 1 froze.
    store
        .write_trusted(
            &who,
            &Command::Admit(Admit::Retry(Retry {
                id: deployment(2),
                deployment: deployment(1),
            })),
            &Trusted::default(),
        )
        .unwrap();
    let intent = serde_json::to_value(apply(&store, 2)).unwrap();
    assert!(
        intent.to_string().contains("\"web.acme.ployz.app\""),
        "{intent}"
    );
}

#[test]
fn a_custom_domain_under_the_cluster_domain_is_refused() {
    let (store, who) = shop();
    for hostname in [
        "acme.ployz.app",
        "web-shop.acme.ployz.app",
        "a.b.acme.ployz.app",
    ] {
        let refused = store
            .write_trusted(&who, &add(Some(hostname), None), &cloud(true))
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{hostname}");
    }
    store
        .write_trusted(&who, &add(Some("notacme.ployz.app"), None), &cloud(true))
        .unwrap();
}
