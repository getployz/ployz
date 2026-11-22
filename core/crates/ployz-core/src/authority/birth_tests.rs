#![expect(clippy::indexing_slicing, reason = "JSON fixtures")]

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::*;
use crate::authority::{
    ApprovedChange, AutoDeployRule, RuleBasis, RuleTerms,
    rule::template,
    rule_draft::tests::{terms_json, view_json},
};

const IMAGE: &str = "nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001";
/// 3 Nov 2026 14:02 UTC.
const MADE: u64 = 1_793_714_520_000;
const DAY: u64 = 86_400_000;

static APP: std::sync::LazyLock<Namespace> =
    std::sync::LazyLock::new(|| Namespace::parse("app").unwrap());
static NONE_PLANNED: PlannedVolumes = BTreeMap::new();
static NONE_MINTED: Births = BTreeMap::new();

impl Mounts<'static> {
    /// A spec's mounts in Namespace `app`, with no names from the Store and no birth
    /// minted.
    pub(crate) fn unnamed(now: Timestamp) -> Self {
        Mounts {
            namespace: &APP,
            volumes: &NONE_PLANNED,
            minted: &NONE_MINTED,
            now,
            against: None,
        }
    }
}

fn namespace() -> Namespace {
    APP.clone()
}

fn declared() -> DockerVolumeName {
    DockerVolumeName::parse("vol-1").unwrap()
}

fn files(birth: Option<&VolumeBirth>) -> RequestedServiceSpec {
    let mut source = json!({"kind": "provisioned", "name": "vol-1", "maximum_bytes": 1024});
    if let Some(birth) = birth {
        source["birth"] = serde_json::to_value(birth).unwrap();
    }
    mounting(source, json!({"volume": "data", "target": "/srv/previews"}))
}

fn mounting(source: Value, mount: Value) -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "files",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": IMAGE, "pull_policy": "missing"},
        "volumes": [{"reference": "data", "source": source}],
        "mounts": [mount]
    }))
    .unwrap()
}

fn planned(name: &str, unborn: bool) -> PlannedVolumes {
    BTreeMap::from([(
        declared(),
        PlannedVolume {
            name: name.to_owned(),
            unborn,
        },
    )])
}

fn at(millis: u64) -> Timestamp {
    Timestamp::from_millis(millis).unwrap()
}

fn previews() -> VolumeBirth {
    VolumeBirth {
        namespace: namespace(),
        name: "previews".to_owned(),
        made_at: at(MADE),
        nonce: BirthNonce::from_bytes([9; 16]),
    }
}

fn lines(spec: &RequestedServiceSpec, volumes: &PlannedVolumes, minted: &Births) -> Vec<String> {
    let mounts = Mounts {
        namespace: &APP,
        volumes,
        minted,
        now: at(MADE + 16 * DAY),
        against: None,
    };
    ApprovedChange::of(
        std::slice::from_ref(spec),
        &namespace(),
        &BTreeMap::new(),
        &mounts,
    )
    .unwrap()
    .iter()
    .flat_map(ApprovedChange::lines)
    .collect()
}

#[test]
fn the_approval_that_first_mounts_a_volume_mints_it_and_says_it_is_new() {
    let volumes = planned("previews", true);
    let mut target = vec![files(None)];
    let minted = mint(&mut target, (&namespace(), &volumes), at(MADE), |_| {
        BirthNonce::from_bytes([7; 16])
    })
    .unwrap();
    let birth = minted
        .get(&declared())
        .expect("a birth for the unborn Volume");
    assert_eq!(
        (&birth.namespace, birth.name.as_str()),
        (&namespace(), "previews")
    );
    let [spec] = target.as_slice() else {
        panic!("one spec");
    };
    assert_eq!(spec.volume_graph().volumes()[0].source.birth(), Some(birth));
    assert_eq!(carried(spec, &minted), minted);
    let lines = lines(spec, &volumes, &minted);
    assert!(
        lines.contains(
            &"  mounts managed volume previews at /srv/previews, read-write: a new, empty volume"
                .to_owned()
        ),
        "{lines:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("\"volumes\"") || line.starts_with("  volumes ")),
        "the mount shows as its line, not as JSON: {lines:#?}"
    );
}

/// Cloud lists a Volume as new but relays a spec that already carries an old birth: the
/// signer keeps that birth and says whose data it is, so declining starts nothing new.
#[test]
fn a_birth_cloud_relays_as_new_reads_as_the_data_it_names() {
    let volumes = planned("thumbs", true);
    let mut target = vec![files(Some(&previews()))];
    let minted = mint(
        &mut target,
        (&namespace(), &volumes),
        at(MADE + 16 * DAY),
        |_| BirthNonce::from_bytes([7; 16]),
    )
    .unwrap();
    assert!(minted.is_empty(), "nothing minted over a relayed birth");
    let lines = lines(&target[0], &volumes, &minted);
    assert!(
        lines.contains(
            &"  mounts managed volume previews at /srv/previews, read-write: data made as \
              previews, 3 Nov 14:02 UTC, 16 days ago (Ployz Cloud calls it thumbs)"
                .to_owned()
        ),
        "{lines:#?}"
    );
}

#[test]
fn a_volume_from_before_births_reads_as_legacy() {
    let volumes = planned("previews", false);
    let mut target = vec![files(None)];
    let minted = mint(&mut target, (&namespace(), &volumes), at(MADE), |_| {
        BirthNonce::from_bytes([7; 16])
    })
    .unwrap();
    assert!(minted.is_empty());
    let lines = lines(&target[0], &volumes, &minted);
    assert!(
        lines.contains(
            &"  mounts managed volume app_vol-1 at /srv/previews, read-write: data from before \
              this version of Ployz recorded where data began (Ployz Cloud calls it previews)"
                .to_owned()
        ),
        "{lines:#?}"
    );
}

#[test]
fn a_birth_from_another_environment_is_refused_before_signing() {
    let other = VolumeBirth {
        namespace: Namespace::parse("app-preview").unwrap(),
        ..previews()
    };
    let mut target = vec![files(Some(&other))];
    let refused = mint(
        &mut target,
        (&namespace(), &planned("previews", false)),
        at(MADE),
        |_| BirthNonce::from_bytes([7; 16]),
    );
    assert!(matches!(refused, Err(BirthError::OtherEnvironment(_))));
}

/// Two Volumes from before births that Ployz Cloud names alike read apart: each line
/// names the Volume as its Servers check it, and Cloud's name only beside it.
#[test]
fn two_legacy_volumes_cloud_names_alike_read_apart() {
    let named = |declared: &str| {
        let spec = mounting(
            json!({"kind": "provisioned", "name": declared, "maximum_bytes": 1024}),
            json!({"volume": "data", "target": "/srv/previews"}),
        );
        let volumes = BTreeMap::from([(
            DockerVolumeName::parse(declared).unwrap(),
            PlannedVolume {
                name: "previews".to_owned(),
                unborn: false,
            },
        )]);
        lines(&spec, &volumes, &Births::new())
    };
    let (one, two) = (named("vol-1"), named("vol-2"));
    assert_ne!(one, two);
    assert!(
        two.contains(
            &"  mounts managed volume app_vol-2 at /srv/previews, read-write: data from before \
              this version of Ployz recorded where data began (Ployz Cloud calls it previews)"
                .to_owned()
        ),
        "{two:#?}"
    );
}

/// A Docker volume and a managed one with the same data read apart, since the kind of
/// storage is signed.
#[test]
fn a_docker_volume_and_a_managed_one_read_apart() {
    let birth = serde_json::to_value(previews()).unwrap();
    let mount = json!({"volume": "data", "target": "/srv/previews"});
    let managed = mounting(
        json!({"kind": "provisioned", "name": "vol-1", "maximum_bytes": 1024, "birth": birth}),
        mount.clone(),
    );
    let docker = mounting(
        json!({"kind": "ordinary", "name": "vol-1", "driver": {"name": "local"}, "birth": birth}),
        mount,
    );
    let volumes = planned("previews", false);
    let docker = lines(&docker, &volumes, &Births::new());
    assert_ne!(lines(&managed, &volumes, &Births::new()), docker);
    assert!(
        docker.contains(
            &"  mounts Docker volume previews at /srv/previews, read-write: data made as \
              previews, 3 Nov 14:02 UTC, 16 days ago"
                .to_owned()
        ),
        "{docker:#?}"
    );
}

/// A mount with a subpath, no initial copy and labels keeps its line, which shows each
/// of them, rather than falling back to the spec's JSON.
#[test]
fn a_mount_with_more_settings_keeps_its_line_and_shows_them() {
    let volumes = planned("previews", true);
    let mut target = vec![mounting(
        json!({"kind": "provisioned", "name": "vol-1", "maximum_bytes": 1024, "labels": {"team": "a"}}),
        json!({"volume": "data", "target": "/srv/previews", "subpath": "logs", "no_copy": true}),
    )];
    let minted = mint(&mut target, (&namespace(), &volumes), at(MADE), |_| {
        BirthNonce::from_bytes([7; 16])
    })
    .unwrap();
    let lines = lines(&target[0], &volumes, &minted);
    assert!(
        lines.contains(
            &"  mounts managed volume previews at /srv/previews, subpath logs, no initial copy, \
              labels {\"team\":\"a\"}, read-write: a new, empty volume"
                .to_owned()
        ),
        "{lines:#?}"
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("  volumes ") || line.starts_with("  mounts [")),
        "the mount shows as its line, not as JSON: {lines:#?}"
    );
}

/// A rule approved for some Services mints births only for the Volumes those Services
/// mount, so the Store records none for a Volume another Service alone mounts.
#[test]
fn a_rule_mints_only_for_the_services_it_covers() {
    let mut view: Value = view_json();
    let provisioned = |name: &str| json!([{"reference": "data", "source": {"kind": "provisioned", "name": name, "maximum_bytes": 1024}}]);
    let mount = json!([{"volume": "data", "target": "/srv/data"}]);
    view["target"][0]["volumes"] = provisioned("vol-1");
    view["target"][0]["mounts"] = mount.clone();
    view["target"][1]["volumes"] = provisioned("vol-2");
    view["target"][1]["mounts"] = mount;
    view["volumes"] = json!({
        "vol-1": {"name": "web-data", "unborn": true},
        "vol-2": {"name": "cache-data", "unborn": true},
    });
    let mut basis: RuleBasis = serde_json::from_value(view).unwrap();
    let terms: RuleTerms = serde_json::from_value(terms_json()).unwrap();
    let names = basis.branch_services();
    assert_eq!(names, [ServiceName::parse("web").unwrap()]);
    AutoDeployRule::draft(&mut basis, &names, terms).unwrap();
    assert_eq!(
        basis.minted.keys().collect::<Vec<_>>(),
        [&declared()],
        "only web's Volume is minted"
    );
    assert_eq!(
        basis.target[1].volume_graph().volumes()[0].source.birth(),
        None
    );
}

/// A rule that adds a mount mints its birth, prints the mount's line in full, and
/// approves the spec a push sends once the Store records that birth.
#[test]
fn a_rule_that_adds_a_mount_mints_it_and_prints_the_line() {
    let mut view: Value = view_json();
    view["target"][0]["volumes"] = json!([{"reference": "data", "source": {"kind": "provisioned", "name": "vol-1", "maximum_bytes": 1024}}]);
    view["target"][0]["mounts"] = json!([{"volume": "data", "target": "/srv/previews"}]);
    view["volumes"] = json!({"vol-1": {"name": "previews", "unborn": true}});
    let mut basis: RuleBasis = serde_json::from_value(view).unwrap();
    let terms: RuleTerms = serde_json::from_value(terms_json()).unwrap();
    let names = basis.branch_services();
    let rule = AutoDeployRule::draft(&mut basis, &names, terms).unwrap();
    let birth = basis
        .minted
        .get(&declared())
        .expect("the rule minted a birth");
    let draft = rule.drafted(&basis).unwrap();
    assert_eq!(draft.births, basis.minted);
    let web = &draft.summary.services[0];
    assert!(
        web.settings.contains(
            &"mounts managed volume previews at /srv/previews, read-write: a new, empty volume"
                .to_owned()
        ),
        "{:#?}",
        web.settings
    );
    assert!(
        !web.settings.iter().any(|line| line.starts_with("volumes ")),
        "{:#?}",
        web.settings
    );
    let pushed = basis.target[0].clone();
    assert_eq!(
        pushed.volume_graph().volumes()[0].source.birth(),
        Some(birth)
    );
    assert_eq!(
        template(&pushed, &basis.namespace).unwrap(),
        rule.services[0].template
    );
}

fn thumbs(nonce: u8) -> VolumeBirth {
    VolumeBirth {
        nonce: BirthNonce::from_bytes([nonce; 16]),
        ..previews()
    }
}

/// What a Machine is handed for `spec`: scoped and resolved.
fn resolved(spec: &RequestedServiceSpec) -> crate::ResolvedServiceSpec {
    let mut spec = spec.clone();
    spec.mount_graph = spec.mount_graph.scope_to_namespace(&namespace()).unwrap();
    let id = crate::ServiceId::parse("c".repeat(32)).unwrap();
    spec.to_resolved(id, crate::ResolvedUpdateConfig::default())
        .unwrap()
}

/// `spec` as a Server lists its Container, labelled with the origins its mounts expect
/// unless `legacy`, as a Container from before origins.
fn listed(spec: &RequestedServiceSpec, running: bool, legacy: bool) -> ContainerObservation {
    let resolved_spec = resolved(spec);
    let mut labels = BTreeMap::new();
    if let (false, Some(origins)) = (
        legacy,
        MountOrigins::of(resolved_spec.volume_graph()).unwrap(),
    ) {
        labels.insert(
            crate::labels::VOLUME_ORIGINS.to_owned(),
            origins.label().unwrap(),
        );
    }
    ContainerObservation::try_from(crate::ContainerObservationParts {
        container_id: crate::ContainerId::parse("a".repeat(64)).unwrap(),
        display_name: spec.name.to_string(),
        created_at_unix_nanos: 0,
        machine_id: crate::MachineId::parse("6".repeat(32)).unwrap(),
        namespace: namespace(),
        kind: ContainerKind::ServiceContainer,
        runtime: match running {
            true => ContainerRuntimeObservation::Running {
                health: crate::HealthObservation::Unhealthy,
            },
            false => ContainerRuntimeObservation::Exited { code: 0 },
        },
        effective_healthcheck: None,
        resolved_spec,
        address: None,
        labels,
        rotation: None,
        r#gen: None,
        stopped: false,
        run: None,
        log_driver: None,
    })
    .unwrap()
}

fn named(mut spec: RequestedServiceSpec, name: &str) -> RequestedServiceSpec {
    spec.name = ServiceName::parse(name).unwrap();
    spec
}

fn approved(spec: &RequestedServiceSpec) -> Before {
    Before::Approved(Approved {
        spec: spec.clone(),
        signed_at: at(MADE + DAY),
    })
}

/// `spec`'s mounts against `before`, with `containers` listed and `unanswered` silent.
fn reviewed(
    spec: &RequestedServiceSpec,
    before: Before,
    containers: &[ContainerObservation],
    unanswered: &[&str],
    minted: &Births,
) -> Result<(Vec<String>, Vec<String>), BirthError> {
    let held = Held::of(
        &APP,
        containers,
        unanswered.iter().map(|name| (*name).to_owned()).collect(),
    );
    let before = BTreeMap::from([(spec.name.clone(), before)]);
    let mounts = Mounts {
        namespace: &APP,
        volumes: &planned("previews", false),
        minted,
        now: at(MADE + 16 * DAY),
        against: Some(Against {
            before: &before,
            held: Some(&held),
        }),
    };
    let lines = mounts.lines(spec)?;
    let text = |lines: Vec<Shown>| lines.into_iter().map(String::from).collect();
    Ok((text(lines.lines), text(lines.confirm)))
}

#[test]
fn an_image_bump_onto_the_data_the_servers_run_collapses_and_asks_nothing() {
    let base = files(Some(&previews()));
    let mut bumped = base.clone();
    bumped.container.image = IMAGE.replace("0001", "0002");
    let (lines, confirm) = reviewed(
        &bumped,
        approved(&base),
        &[listed(&base, true, false)],
        &[],
        &Births::new(),
    )
    .unwrap();
    assert_eq!(
        lines,
        ["1 volume keeps the data you approved (signed 4 Nov 14:02 UTC)"]
    );
    assert!(confirm.is_empty(), "{confirm:#?}");
}

/// Cloud offers a genuine older version as the base. It matches the proposed data, but
/// the Server runs other data, so the line asks rather than collapsing.
#[test]
fn a_stale_base_asks_when_the_servers_run_other_data() {
    let base = files(Some(&previews()));
    let (_, confirm) = reviewed(
        &base,
        approved(&base),
        &[listed(&files(Some(&thumbs(3))), true, false)],
        &[],
        &Births::new(),
    )
    .unwrap();
    assert_eq!(confirm.len(), 1, "{confirm:#?}");
    assert!(
        confirm[0].contains(", not what files uses now"),
        "{confirm:#?}"
    );
}

/// A birth swapped in at the same target asks, and two births with the same name and time
/// read apart by their origins.
#[test]
fn a_swapped_birth_asks_and_reads_apart_from_its_look_alike() {
    let base = files(Some(&previews()));
    let swapped = files(Some(&thumbs(8)));
    let (_, confirm) = reviewed(
        &swapped,
        approved(&base),
        &[listed(&base, true, false)],
        &[],
        &Births::new(),
    )
    .unwrap();
    let [line] = confirm.as_slice() else {
        panic!("{confirm:#?}");
    };
    let ids: Vec<&str> = line.split(" #").skip(1).map(|id| &id[..6]).collect();
    assert!(
        line.contains("different data than the version you approved: was previews"),
        "{line}"
    );
    assert_eq!(ids.len(), 2, "{line}");
    assert_ne!(ids[0], ids[1], "{line}");
}

/// A Server that doesn't answer may hold other data, so the mount prints in full, names
/// it and asks.
#[test]
fn a_server_that_doesnt_answer_keeps_a_mount_from_collapsing() {
    let base = files(Some(&previews()));
    let (_, confirm) = reviewed(
        &base,
        approved(&base),
        &[listed(&base, true, false)],
        &["web-2"],
        &Births::new(),
    )
    .unwrap();
    assert_eq!(confirm.len(), 1, "{confirm:#?}");
    assert!(confirm[0].contains("no answer from web-2"), "{confirm:#?}");
}

#[test]
fn a_new_volume_at_a_target_that_held_data_asks() {
    let base = files(Some(&previews()));
    let fresh = thumbs(5);
    let minted = Births::from([(declared(), fresh.clone())]);
    let (_, confirm) = reviewed(
        &files(Some(&fresh)),
        approved(&base),
        &[listed(&base, true, false)],
        &[],
        &minted,
    )
    .unwrap();
    assert_eq!(confirm.len(), 1, "{confirm:#?}");
    assert!(
        confirm[0].contains("a new, empty volume in place of the data at /srv/previews"),
        "{confirm:#?}"
    );
}

/// Data from before births needs a version that mounts it with no birth, or a listed
/// Container that uses it. A name no one uses, such as a new Volume Cloud left out of
/// `unborn`, is refused.
#[test]
fn legacy_data_passes_only_on_an_approved_version_or_a_listed_container() {
    let legacy = files(None);
    let none = Births::new();
    for before in [Before::New, Before::Unknown(Unknown::NoRecord)] {
        let refused = reviewed(&legacy, before, &[], &[], &none);
        assert!(
            matches!(refused, Err(BirthError::Legacy { .. })),
            "{refused:?}"
        );
    }
    let db = listed(&named(legacy.clone(), "db"), true, true);
    let (_, confirm) = reviewed(&legacy, Before::New, &[db], &[], &none).unwrap();
    assert_eq!(confirm.len(), 1, "{confirm:#?}");
    assert!(confirm[0].contains(", that db uses now"), "{confirm:#?}");
    let mine = listed(&legacy, true, true);
    let (lines, confirm) = reviewed(&legacy, approved(&legacy), &[mine], &[], &none).unwrap();
    assert!(confirm.is_empty(), "{confirm:#?}");
    assert!(lines[0].starts_with("1 volume keeps"), "{lines:#?}");
    let unbirthed = reviewed(
        &legacy,
        approved(&files(Some(&previews()))),
        &[],
        &[],
        &none,
    );
    assert!(
        matches!(unbirthed, Err(BirthError::Unbirthed { .. })),
        "{unbirthed:?}"
    );
}

#[test]
fn a_first_attach_names_the_service_that_uses_the_data_or_says_none_does() {
    let born = files(Some(&previews()));
    let none = Births::new();
    let db = listed(&named(born.clone(), "db"), true, false);
    let (_, confirm) = reviewed(&born, Before::New, &[db], &[], &none).unwrap();
    assert!(confirm[0].contains(", that db uses now"), "{confirm:#?}");
    let (_, confirm) = reviewed(&born, Before::New, &[], &[], &none).unwrap();
    assert!(
        confirm[0].contains(", that no server lists a container using"),
        "{confirm:#?}"
    );
}
