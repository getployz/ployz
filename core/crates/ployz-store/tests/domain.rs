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
    Actor, AddDomain, Admit, Approval, Cancel, ClusterDomain, ClusterDomainStatus, Command,
    ConfigStore, CreateEnvironment, CreateProject, CreateService, Deploy, DeploymentId,
    DestructiveKind, DiffQuery, DiffView, DnsLookup, DomainAction, DomainChange, DomainEvidence,
    DomainQuery, DomainRow, DomainStatus, DomainsQuery, EnvironmentId, EnvironmentName,
    EnvironmentRef, Hostname, OrganizationId, PlanQuery, ProjectId, ProjectName, Publish,
    PublishedHostname, Query, RemoveDomain, RenameProject, Retry, RunEvidence, RunnerId,
    ServiceLineageId, SetGeneratedDomain, Trusted, View, Written,
};
use serde_json::{Value, json};

mod backend;

fn uuid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02}")
}

/// Project `shop` with Service `web` in `production`, and Service `web` in `staging`.
fn shop() -> (ConfigStore, Actor) {
    shop_in(backend::open())
}

fn shop_in(store: ConfigStore) -> (ConfigStore, Actor) {
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
                    template: None,
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
        prefix: None,
        port,
    }
}

fn add_prefix(prefix: &str) -> AddDomain {
    AddDomain {
        prefix: Some(ployz_core::DomainPrefix::parse(prefix).unwrap()),
        ..add(None, None)
    }
}

/// Cloud's evidence: a ready Cluster Domain.
fn cloud() -> Trusted {
    Trusted {
        domains: DomainEvidence {
            cluster_domain: Some(ClusterDomain {
                name: host("acme.ployz.app"),
                status: ClusterDomainStatus::Ready,
            }),
            certificates: Some(Vec::new()),
            ingress_addresses: vec!["203.0.113.7".into()],
            lookups: Vec::new(),
            published: Vec::new(),
            ..DomainEvidence::default()
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
fn adding_a_generated_domain_again_keeps_it_and_offers_a_free_prefix() {
    let (store, who) = shop();
    let added = store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    assert_eq!(added.written.domain.shown(), "web.acme.ployz.app");
    assert_eq!(added.written.staged.len(), 1);
    assert!(added.added);
    assert_eq!(added.free_prefix, None);
    // Adding it again keeps it and names a prefix free for another; a port
    // retargets it.
    let again = store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    assert!(!again.added);
    assert!(again.written.staged.is_empty());
    assert_eq!(
        again.written.environment.revision,
        added.written.environment.revision
    );
    assert_eq!(
        again.free_prefix.as_ref().map(|p| p.as_str()),
        Some("web-2")
    );
    let retargeted = store
        .write_trusted(&who, &add(None, Some(8080)), &cloud())
        .unwrap();
    assert_eq!(retargeted.written.domain.port, Some(8080));
    assert!(!retargeted.added);

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
        serde_json::to_value(&staging.written.domain).unwrap(),
        json!({ "service": "web", "kind": "generated", "prefix": "web-2", "hostname": null, "port": null })
    );
    // The offer skips what the Organization holds, the Service's own included.
    let offered = store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    assert_eq!(
        offered.free_prefix.as_ref().map(|p| p.as_str()),
        Some("web-3")
    );
}

#[test]
fn a_service_takes_several_generated_domains_by_prefix() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    let second = store
        .write_trusted(&who, &add_prefix("shop"), &cloud())
        .unwrap();
    assert!(second.added);
    assert_eq!(second.written.domain.shown(), "shop.acme.ployz.app");
    let shown = rows(&store, &who, &cloud())
        .iter()
        .map(|row| row.domain.shown().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(shown, ["web.acme.ployz.app", "shop.acme.ployz.app"]);

    // Naming one it has keeps it; a port retargets only that one.
    let again = store
        .write_trusted(&who, &add_prefix("shop"), &cloud())
        .unwrap();
    assert!(!again.added);
    assert!(again.written.staged.is_empty());
    assert_eq!(again.free_prefix, None);
    let retargeted = store
        .write_trusted(
            &who,
            &AddDomain {
                port: Some(3000),
                ..add_prefix("shop")
            },
            &cloud(),
        )
        .unwrap();
    assert_eq!(retargeted.written.domain.port, Some(3000));
    let ports = rows(&store, &who, &cloud())
        .iter()
        .map(|row| row.domain.port)
        .collect::<Vec<_>>();
    assert_eq!(ports, [None, Some(3000)]);

    // A prefix another Service holds, or another Namespace publishes, is refused.
    let taken = store
        .write_trusted(
            &who,
            &AddDomain {
                environment: at(Some("staging")),
                ..add_prefix("shop")
            },
            &cloud(),
        )
        .unwrap_err();
    assert_eq!(taken.code, RpcErrorCode::Conflict);
    let published = store
        .write_trusted(
            &who,
            &add_prefix("old"),
            &publishing("old.acme.ployz.app", "gone-production", "web"),
        )
        .unwrap_err();
    assert_eq!(published.code, RpcErrorCode::Conflict);
    // A domain is custom or generated, never both.
    let both = store
        .write_trusted(
            &who,
            &AddDomain {
                hostname: Some(host("app.example.com")),
                ..add_prefix("app")
            },
            &cloud(),
        )
        .unwrap_err();
    assert_eq!(both.code, RpcErrorCode::InvalidArgument);
    assert_eq!(rows(&store, &who, &cloud()).len(), 2);
}

#[test]
fn changing_one_of_several_generated_domains_names_it() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    store
        .write_trusted(&who, &add_prefix("shop"), &cloud())
        .unwrap();
    let set = |domain: Option<&str>, prefix: &str| SetGeneratedDomain {
        domain: domain.map(str::to_owned),
        ..set_prefix(None, prefix)
    };

    let unnamed = store
        .write_trusted(&who, &set(None, "store"), &cloud())
        .unwrap_err();
    assert_eq!(unnamed.code, RpcErrorCode::Ambiguous);
    assert_eq!(
        unnamed.details["valid_children"],
        json!(["web.acme.ployz.app", "shop.acme.ployz.app"])
    );
    let missing = store
        .write_trusted(&who, &set(Some("api"), "store"), &cloud())
        .unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert_eq!(
        missing.details["valid_children"],
        json!(["web.acme.ployz.app", "shop.acme.ployz.app"])
    );
    // Taking the prefix of its other generated domain is refused.
    let onto = store
        .write_trusted(&who, &set(Some("shop"), "web"), &cloud())
        .unwrap_err();
    assert_eq!(onto.code, RpcErrorCode::Conflict);

    let renamed = store
        .write_trusted(&who, &set(Some("shop.acme.ployz.app"), "store"), &cloud())
        .unwrap();
    assert_eq!(renamed.domain.shown(), "store.acme.ployz.app");
    let shown = rows(&store, &who, &cloud())
        .iter()
        .map(|row| row.domain.shown().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(shown, ["web.acme.ployz.app", "store.acme.ployz.app"]);
}

#[test]
fn a_custom_domain_is_added_retargeted_and_held_by_one_service() {
    let (store, who) = shop();
    let added = store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud())
        .unwrap();
    assert_eq!(added.written.domain.shown(), "app.example.com");
    let same = store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud())
        .unwrap();
    assert!(same.written.staged.is_empty());
    let retarget = store
        .write_trusted(&who, &add(Some("app.example.com"), Some(3000)), &cloud())
        .unwrap();
    assert_eq!(retarget.written.domain.port, Some(3000));

    // Another Service, in any Environment, can't take it.
    let taken = store
        .write_trusted(
            &who,
            &AddDomain {
                environment: at(Some("staging")),
                ..add(Some("app.example.com"), None)
            },
            &cloud(),
        )
        .unwrap_err();
    assert_eq!(taken.code, RpcErrorCode::Conflict);
    assert_eq!(
        taken.message,
        "Another Service already has app.example.com: web in shop/production"
    );

    // Hostnames under the Cluster Domain are Ployz's to generate, never custom.
    for hostname in ["web.acme.ployz.app", "acme.ployz.app"] {
        let generated = store
            .write_trusted(&who, &add(Some(hostname), None), &cloud())
            .unwrap_err();
        assert_eq!(generated.code, RpcErrorCode::InvalidArgument, "{hostname}");
    }
}

#[test]
fn a_domain_is_removed_by_hostname_or_prefix() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud())
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
    let missing = remove("nope.example.com", &cloud()).unwrap_err();
    assert_eq!(missing.code, RpcErrorCode::NotFound);
    assert_eq!(
        missing.details["valid_children"],
        json!(["app.example.com", "web.acme.ployz.app"])
    );
    assert!(!missing.message.contains("nope"));
    store
        .write_trusted(&who, &add(Some("пример.рф"), None), &cloud())
        .unwrap();
    remove("Пример.рф", &cloud()).unwrap();
    store
        .write_trusted(&who, &add_prefix("xn--zz"), &cloud())
        .unwrap();
    remove("xn--zz", &cloud()).unwrap();
    remove("App.Example.com", &cloud()).unwrap();
    // Without the Cluster Domain, a generated domain goes by its prefix.
    let removed = remove("web", &Trusted::default()).unwrap();
    assert_eq!(removed.staged.len(), 1);
    assert!(rows(&store, &who, &cloud()).is_empty());
}

#[test]
fn a_hostname_is_kept_lowercase_and_in_punycode() {
    let kept = |name: &str| Hostname::parse(name).unwrap().as_str().to_owned();
    assert_eq!(kept("пример.рф"), "xn--e1afmkfd.xn--p1ai");
    assert_eq!(kept(" Apple.COM "), "apple.com");
    assert_eq!(kept("xn--pple-43d.com"), "xn--pple-43d.com");
    assert_eq!(kept("аpple.com"), "xn--pple-43d.com");
    assert_eq!(kept("XN--ZZ.example.com"), "xn--zz.example.com");
    let long = format!("{}.com", "a".repeat(64));
    for refused in [
        "has space.com",
        "under_score.com",
        "*.example.com",
        "localhost",
        "при мер.рф",
        long.as_str(),
    ] {
        let error = Hostname::parse(refused).unwrap_err();
        assert_eq!(error.code, RpcErrorCode::InvalidArgument, "{refused}");
        assert_eq!(error.message, "Expected a hostname like app.example.com");
    }
}

#[test]
fn a_unicode_custom_domain_is_listed_in_punycode() {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(Some("Пример.рф"), None), &cloud())
        .unwrap();
    let shown: Vec<String> = rows(&store, &who, &cloud())
        .iter()
        .map(|row| row.domain.shown().to_owned())
        .collect();
    assert_eq!(shown, ["xn--e1afmkfd.xn--p1ai"]);
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
            prune: false,
            no_prune: false,
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
                progress: Vec::new(),
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
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    // A plan checks everything but the Cluster Domain, which only Cloud holds.
    store.read(&who, &PlanQuery::default()).unwrap();
    let refused = admit(&store, &who, 1, &Trusted::default()).unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Unsupported);

    admit(&store, &who, 1, &cloud()).unwrap();
    assert_eq!(
        rows(&store, &who, &cloud())[0].reason.as_deref(),
        Some("Deploying")
    );
    let intent = serde_json::to_value(apply(&store, 1)).unwrap();
    assert!(
        intent.to_string().contains("\"web.acme.ployz.app\""),
        "{intent}"
    );
    let row = &rows(&store, &who, &cloud())[0];
    assert_eq!(
        (row.status, row.action.clone()),
        (DomainStatus::Ready, None)
    );
}

fn deployed_generated_domain() -> (ConfigStore, Actor) {
    let (store, who) = shop();
    store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    admit(&store, &who, 1, &cloud()).unwrap();
    apply(&store, 1);
    (store, who)
}

fn effects(store: &ConfigStore, who: &Actor) -> Vec<(DestructiveKind, String, String)> {
    let diff: DiffView = store.read(who, &DiffQuery::default()).unwrap();
    diff.effects
        .into_iter()
        .map(|effect| (effect.kind, effect.name, effect.path))
        .collect()
}

#[test]
fn removing_a_deployed_generated_domain_is_destructive() {
    let (store, who) = deployed_generated_domain();
    store
        .write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "web".into(),
            },
            &cloud(),
        )
        .unwrap();
    assert_eq!(
        effects(&store, &who),
        [(
            DestructiveKind::RemovesDomain,
            "web.acme.ployz.app".to_owned(),
            "web.managedHostnames".to_owned()
        )]
    );
    let publish = Publish {
        id: None,
        environment: EnvironmentRef::default(),
        version: None,
        message: None,
        accept_volume_loss: Vec::new(),
    };
    let asking = Trusted {
        approval: Approval::Required,
        ..cloud()
    };
    let refused = store.write_trusted(&who, &publish, &asking).unwrap_err();
    assert_eq!(
        refused.message,
        "A human must approve this first: remove domain web.acme.ployz.app"
    );
}

#[test]
fn changing_a_deployed_generated_prefix_removes_the_old_hostname() {
    let (store, who) = deployed_generated_domain();
    store
        .write_trusted(&who, &set_prefix(None, "shop"), &cloud())
        .unwrap();
    assert_eq!(
        effects(&store, &who),
        [(
            DestructiveKind::RemovesDomain,
            "web.acme.ployz.app".to_owned(),
            "web.managedHostnames".to_owned()
        )]
    );
}

#[test]
fn a_removed_generated_domain_is_named_under_the_cluster_domain_it_runs_under() {
    let (store, who) = deployed_generated_domain();
    let mut other = cloud();
    other.domains.cluster_domain = Some(ClusterDomain {
        name: host("other.ployz.app"),
        status: ClusterDomainStatus::Ready,
    });
    admit(&store, &who, 2, &other).unwrap();
    store
        .write(
            &who,
            &Cancel {
                deployment: deployment(2),
            },
        )
        .unwrap();
    store
        .write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "web".into(),
            },
            &cloud(),
        )
        .unwrap();
    assert_eq!(
        effects(&store, &who),
        [(
            DestructiveKind::RemovesDomain,
            "web.acme.ployz.app".to_owned(),
            "web.managedHostnames".to_owned()
        )]
    );
}

#[test]
fn a_staged_generated_domain_reads_by_the_hostname_it_runs_under() {
    let (store, who) = deployed_generated_domain();
    store
        .write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "web".into(),
            },
            &cloud(),
        )
        .unwrap();
    store
        .write_trusted(&who, &add_prefix("shop"), &cloud())
        .unwrap();
    let mut other = cloud();
    other.domains.cluster_domain = Some(ClusterDomain {
        name: host("other.ployz.app"),
        status: ClusterDomainStatus::Ready,
    });
    let read = |trusted: &Trusted| -> Vec<String> {
        let diff: DiffView = store
            .read_trusted(&who, &DiffQuery::default(), trusted)
            .unwrap();
        diff.changes
            .iter()
            .flat_map(|change| &change.settings)
            .filter_map(DomainChange::of)
            .flatten()
            .map(|domain| domain.to_string())
            .collect()
    };
    // The deployed one keeps its hostname; the new one takes the current Cluster Domain.
    assert_eq!(
        read(&other),
        ["removed web.acme.ployz.app", "added shop.other.ployz.app"]
    );
    // Without Cloud's evidence the last Deployment's Cluster Domain stands in.
    assert_eq!(
        read(&Trusted::default()),
        ["removed web.acme.ployz.app", "added shop.acme.ployz.app"]
    );
}

#[test]
fn a_restored_generated_domain_reads_by_its_full_hostname() {
    let (store, who) = deployed_generated_domain();
    store
        .write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "web".into(),
            },
            &cloud(),
        )
        .unwrap();
    let publish = Publish {
        id: None,
        environment: EnvironmentRef::default(),
        version: None,
        message: None,
        accept_volume_loss: Vec::new(),
    };
    store.write_trusted(&who, &publish, &cloud()).unwrap();
    store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    let diff: DiffView = store
        .read_trusted(&who, &DiffQuery::default(), &cloud())
        .unwrap();
    let domains: Vec<String> = diff
        .review_changes()
        .iter()
        .flat_map(|change| &change.settings)
        .filter_map(DomainChange::of)
        .flatten()
        .map(|domain| domain.to_string())
        .collect();
    assert_eq!(domains, ["added web.acme.ployz.app"]);
}

#[test]
fn a_generated_domain_added_in_flight_is_named_under_its_deployments_cluster_domain() {
    let (store, who) = shop();
    admit(&store, &who, 1, &cloud()).unwrap();
    apply(&store, 1);
    store
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    let mut other = cloud();
    other.domains.cluster_domain = Some(ClusterDomain {
        name: host("other.ployz.app"),
        status: ClusterDomainStatus::Ready,
    });
    admit(&store, &who, 2, &other).unwrap();
    store
        .write_trusted(
            &who,
            &RemoveDomain {
                environment: EnvironmentRef::default(),
                domain: "web".into(),
            },
            &cloud(),
        )
        .unwrap();
    assert_eq!(
        effects(&store, &who),
        [(
            DestructiveKind::RemovesDomain,
            "web.other.ployz.app".to_owned(),
            "web.managedHostnames".to_owned()
        )]
    );
}

#[test]
fn a_check_sees_fixed_dns_before_the_certificate_retries() {
    let (store, who) = shop();
    let staged = rows(&store, &who, &cloud());
    assert!(staged.is_empty());
    store
        .write_trusted(&who, &add(Some("app.example.com"), None), &cloud())
        .unwrap();
    let before = &rows(&store, &who, &cloud())[0];
    assert_eq!(
        (before.status, before.action.clone()),
        (DomainStatus::SettingUp, Some(DomainAction::Deploy))
    );
    admit(&store, &who, 1, &cloud()).unwrap();
    apply(&store, 1);

    let mut observed = cloud();
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
        .write_trusted(&who, &add(None, None), &cloud())
        .unwrap();
    admit(&store, &who, 1, &cloud()).unwrap();
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
                rolls_back: None,
                version: None,
                accept_volume_loss: Vec::new(),
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
fn adding_a_custom_domain_refuses_new_invalid_names_without_tightening_readers() {
    let (store, who) = shop();
    for (hostname, reason) in [
        ("1.2.3.4", "IP address"),
        ("127.1", "IP address"),
        ("1.2.3.4.5", "IP address"),
        ("ployz.app", "reserved"),
        ("a.ployz.app", "reserved"),
        ("xn--zz.com", "punycode"),
    ] {
        let legacy: Hostname = serde_json::from_value(json!(hostname)).unwrap();
        assert_eq!(legacy.as_str(), hostname);
        let refused = store.write(&who, &add(Some(hostname), None)).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
        assert!(refused.message.contains(reason), "{hostname}: {refused}");
    }
    assert!(rows(&store, &who, &Trusted::default()).is_empty());
}

#[test]
fn readding_a_legacy_custom_domain_changes_its_port_only_where_it_exists() {
    for hostname in ["1.2.3.4", "a.ployz.app", "xn--zz.com"] {
        let dir = tempfile::tempdir().unwrap();
        let url = backend::fresh_url(&dir);
        let (store, who) = shop_in(ConfigStore::open(&url, backend::key()).unwrap());
        store
            .write(&who, &add(Some("legacy.example.com"), Some(80)))
            .unwrap();
        backend::execute(
            &url,
            &format!(
                "UPDATE config_environment SET working = replace(working, 'legacy.example.com', '{hostname}')"
            ),
        );
        let changed = store.write(&who, &add(Some(hostname), Some(8080))).unwrap();
        assert!(!changed.added);
        assert_eq!(changed.written.domain.port, Some(8080));
        let current = rows(&store, &who, &Trusted::default());
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].domain.shown(), hostname);
        assert_eq!(current[0].domain.port, Some(8080));
        let refused = store
            .write(
                &who,
                &AddDomain {
                    environment: at(Some("staging")),
                    ..add(Some(hostname), Some(8080))
                },
            )
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
    }
}

#[test]
fn custom_domain_validation_follows_scope_and_service_resolution() {
    let (store, who) = shop();
    let unprivileged = Trusted {
        permissions: Some(Default::default()),
        ..Trusted::default()
    };
    assert_eq!(
        store
            .write_trusted(&who, &add(Some("1.2.3.4"), None), &unprivileged)
            .unwrap_err()
            .code
            .as_str(),
        "forbidden"
    );
    let missing_environment = AddDomain {
        environment: at(Some("missing")),
        ..add(Some("1.2.3.4"), None)
    };
    assert_eq!(
        store.write(&who, &missing_environment).unwrap_err().code,
        RpcErrorCode::NotFound
    );
    let outsider = Actor::system(OrganizationId::parse("other").unwrap());
    assert_eq!(
        store
            .write(&outsider, &add(Some("1.2.3.4"), None))
            .unwrap_err()
            .code,
        RpcErrorCode::NotFound
    );
    let missing_service = AddDomain {
        service: ServiceName::parse("missing").unwrap(),
        ..add(Some("1.2.3.4"), None)
    };
    assert_eq!(
        store.write(&who, &missing_service).unwrap_err().code,
        RpcErrorCode::NotFound
    );
}

#[test]
fn a_custom_domain_under_the_cluster_domain_is_refused() {
    let (store, who) = shop();
    for cluster in ["acme.ployz.app", "cluster.example.com"] {
        let mut trusted = cloud();
        trusted.domains.cluster_domain.as_mut().unwrap().name = host(cluster);
        for hostname in [
            cluster.to_owned(),
            format!("web-shop.{cluster}"),
            format!("a.b.{cluster}"),
        ] {
            let refused = store
                .write_trusted(&who, &add(Some(&hostname), None), &trusted)
                .unwrap_err();
            assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{hostname}");
            assert!(
                refused.message.contains(&format!(
                    "is under {cluster}, which Ployz generates domains in"
                )),
                "{refused}"
            );
            assert_eq!(refused.details["cluster_domain"], json!(cluster));
        }
    }
    store
        .write_trusted(&who, &add(Some("notcluster.example.com"), None), &cloud())
        .unwrap();
}

/// `cloud()` with `hostname` published by `service` in Namespace `namespace`.
fn publishing(hostname: &str, namespace: &str, service: &str) -> Trusted {
    let mut trusted = cloud();
    trusted.domains.published.push(PublishedHostname {
        hostname: host(hostname),
        namespace: ployz_core::Namespace::parse(namespace).unwrap(),
        service: ServiceName::parse(service).unwrap(),
    });
    trusted
}

fn set_prefix(environment: Option<&str>, prefix: &str) -> SetGeneratedDomain {
    SetGeneratedDomain {
        environment: at(environment),
        service: ServiceName::parse("web").unwrap(),
        prefix: ployz_core::DomainPrefix::parse(prefix).unwrap(),
        domain: None,
        port: None,
    }
}

#[test]
fn a_generated_prefix_changes_to_a_free_dns_label() {
    let (store, who) = shop();
    let trusted = cloud();
    store
        .write_trusted(&who, &add(None, None), &trusted)
        .unwrap();
    let staging = AddDomain {
        environment: at(Some("staging")),
        ..add(None, None)
    };
    store.write_trusted(&who, &staging, &trusted).unwrap();

    let set = store
        .write_trusted(&who, &set_prefix(None, "shop"), &trusted)
        .unwrap();
    assert_eq!(set.domain.shown(), "shop.acme.ployz.app");
    assert_eq!(set.staged.len(), 1);
    let refused = |prefix: &str, trusted: &Trusted| {
        store
            .write_trusted(&who, &set_prefix(Some("staging"), prefix), trusted)
            .unwrap_err()
    };
    // A prefix is one lowercase DNS label, checked as it is read.
    for invalid in ["-shop", "Shop", "a.b"] {
        assert!(
            ployz_core::DomainPrefix::parse(invalid).is_err(),
            "{invalid}"
        );
    }
    // Unique in the Organization, and against what another Namespace publishes.
    assert_eq!(refused("shop", &trusted).code, RpcErrorCode::Conflict);
    let orphan = refused(
        "old",
        &publishing("old.acme.ployz.app", "gone-production", "web"),
    );
    assert_eq!(orphan.code, RpcErrorCode::Conflict);
    assert_eq!(
        orphan.details["next"],
        "ployz server clean --namespace gone-production --confirm gone-production"
    );
    assert!(
        store
            .write_trusted(&who, &set_prefix(None, "shop"), &trusted)
            .unwrap()
            .staged
            .is_empty()
    );
    // One edit sets the port too; leaving it out keeps it, and clearing it keeps
    // the prefix.
    let port = |port| SetGeneratedDomain {
        port: Some(port),
        ..set_prefix(None, "shop")
    };
    let set = store
        .write_trusted(&who, &port(Some(8080)), &trusted)
        .unwrap();
    assert_eq!(set.domain.port, Some(8080));
    let kept = store
        .write_trusted(&who, &set_prefix(None, "shop"), &trusted)
        .unwrap();
    assert_eq!((kept.domain.port, kept.staged.len()), (Some(8080), 0));
    let cleared = store.write_trusted(&who, &port(None), &trusted).unwrap();
    assert_eq!(cleared.staged.len(), 1);
    assert_eq!(cleared.domain.port, None);
    assert_eq!(cleared.domain.shown(), "shop.acme.ployz.app");
}

#[test]
fn a_generated_hostname_longer_than_a_hostname_is_refused_when_its_prefix_is_chosen() {
    let (store, who) = shop();
    let mut trusted = cloud();
    // 245 characters: `web` fits under it, a 10-character prefix makes 256.
    let cluster = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(53)
    );
    trusted.domains.cluster_domain.as_mut().unwrap().name = host(&cluster);
    let added = store
        .write_trusted(&who, &add(None, None), &trusted)
        .unwrap();
    assert_eq!(added.written.domain.shown(), format!("web.{cluster}"));

    let long = "abcdefghij";
    for refused in [
        store
            .write_trusted(&who, &set_prefix(None, long), &trusted)
            .unwrap_err(),
        store
            .write_trusted(&who, &add_prefix(long), &trusted)
            .unwrap_err(),
    ] {
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");
        assert!(refused.message.contains("253"), "{refused:?}");
        assert_eq!(refused.details["hostname"], format!("{long}.{cluster}"));
    }
    assert_eq!(
        rows(&store, &who, &trusted)
            .iter()
            .map(|row| row.domain.shown().to_owned())
            .collect::<Vec<_>>(),
        [format!("web.{cluster}")]
    );
}

/// A Branch adds its name to each generated prefix it copies, so a hostname that fits
/// the Parent can be too long on the Branch: refused when the Branch is made, not at Deploy.
#[test]
fn a_branch_whose_name_makes_a_generated_hostname_too_long_is_refused() {
    let (store, who) = shop();
    let mut trusted = cloud();
    // 245 characters: `web.` fits under it, `web-fixit.` makes 255.
    let cluster = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(53)
    );
    trusted.domains.cluster_domain.as_mut().unwrap().name = host(&cluster);
    store
        .write_trusted(&who, &add(None, None), &trusted)
        .unwrap();
    let branch = |name: &str| ployz_store::CreateBranch {
        id: EnvironmentId::parse(
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string(),
        )
        .unwrap(),
        from: at(None),
        name: EnvironmentName::parse(name).unwrap(),
        copy: vec![ployz_store::NodeName::Service(
            ServiceName::parse("web").unwrap(),
        )],
        live: Vec::new(),
        setup: Vec::new(),
        keep: false,
        fix: None,
    };

    admit(&store, &who, 1, &trusted).unwrap();
    apply(&store, 1);

    // Without Cloud's evidence, the Cluster Domain the Parent last deployed under.
    for refused in [
        store
            .write_trusted(&who, &branch("fixit"), &trusted)
            .unwrap_err(),
        store.write(&who, &branch("fixit")).unwrap_err(),
    ] {
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");
        assert!(refused.message.contains("253"), "{refused:?}");
        assert!(
            refused.message.contains("shorter branch name"),
            "{refused:?}"
        );
        assert_eq!(refused.details["hostname"], format!("web-fixit.{cluster}"));
    }

    store.write(&who, &branch("fix")).unwrap();
}

#[test]
fn a_new_generated_prefix_avoids_published_hostnames() {
    let (store, who) = shop();
    let added = store
        .write_trusted(
            &who,
            &add(None, None),
            &publishing("web.acme.ployz.app", "gone-production", "web"),
        )
        .unwrap();
    assert_eq!(added.written.domain.shown(), "web-2.acme.ployz.app");
}

#[test]
fn a_deploy_refuses_a_hostname_another_namespace_publishes_naming_its_owner() {
    let (store, who) = shop();
    let trusted = cloud();
    store
        .write_trusted(&who, &add(None, None), &trusted)
        .unwrap();
    // Staging deployed once, so its Namespace is reserved.
    let staging = Command::Admit(Admit::Deploy(Deploy {
        id: deployment(1),
        environment: at(Some("staging")),
        services: Vec::new(),
        version: None,
        upload: None,
        accept_volume_loss: Vec::new(),
        message: None,
        prune: false,
        no_prune: false,
    }));
    store.write_trusted(&who, &staging, &trusted).unwrap();
    let refused = admit(
        &store,
        &who,
        2,
        &publishing("web.acme.ployz.app", "shop-staging", "web"),
    )
    .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::Conflict);
    assert_eq!(
        refused.message,
        "web.acme.ployz.app is already published by web in shop/staging"
    );
    // Its own Namespace publishing it is no clash.
    admit(
        &store,
        &who,
        2,
        &publishing("web.acme.ployz.app", "shop-production", "web"),
    )
    .unwrap();
}

#[test]
fn a_renamed_project_reads_by_its_new_name() {
    let (store, who) = shop();
    let renamed = store
        .write(
            &who,
            &RenameProject {
                project: ProjectName::parse("shop").unwrap(),
                name: ProjectName::parse("store").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(renamed.name.as_str(), "store");
    let namespace = store
        .read(&who, &ployz_store::NamespaceQuery::default())
        .unwrap();
    assert_eq!(namespace.environment.project.as_str(), "store");
    // Nothing deployed yet: it takes a Namespace by the new name.
    assert_eq!(namespace.namespace.as_str(), "store-production");
    let back = RenameProject {
        project: ProjectName::parse("store").unwrap(),
        name: ProjectName::parse("store").unwrap(),
    };
    assert_eq!(store.write(&who, &back).unwrap().name.as_str(), "store");
    let missing = RenameProject {
        project: ProjectName::parse("shop").unwrap(),
        name: ProjectName::parse("store").unwrap(),
    };
    assert_eq!(
        store.write(&who, &missing).unwrap_err().code,
        RpcErrorCode::NotFound
    );
}
