use std::{collections::BTreeMap, num::NonZeroU64};

use serde_json::json;

use super::*;
use crate::{
    ContainerPath, RawVolumeSource, ServiceMount, ServiceVolume, ServiceVolumeGraph,
    ServiceVolumeReference,
    authority::{canonical, restart::OwnPrompt},
};

const OPERATION: &str = "0f8fad5bd9cb469fa16570867728950e";
const PINNED: &str =
    "postgres@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn id(fill: char) -> MachineId {
    MachineId::parse(fill.to_string().repeat(32)).unwrap()
}

fn volume() -> DockerVolumeName {
    DockerVolumeName::parse("app_data").unwrap()
}

fn gb(count: u64) -> ProvisionedVolumeMaximumBytes {
    ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(count * 1_000_000_000).unwrap())
}

/// A spec running `image` that mounts `app_data` at 1 GB.
fn running(image: &str) -> ResolvedServiceSpec {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": crate::ServiceId::random(),
        "name": "db",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": image, "pull_policy": "missing" },
    }))
    .unwrap();
    let volume = ServiceVolume {
        reference: ServiceVolumeReference::parse("v0").unwrap(),
        source: RawVolumeSource::Provisioned {
            name: DockerVolumeName::parse("data").unwrap(),
            maximum_bytes: gb(1),
            labels: BTreeMap::new(),
        }
        .admit()
        .unwrap(),
    };
    let mount = ServiceMount {
        volume: volume.reference.clone(),
        target: ContainerPath::parse("/data").unwrap(),
        read_only: false,
        no_copy: false,
        subpath: None,
    };
    spec.set_volume_graph(
        ServiceVolumeGraph::parse(vec![volume], vec![mount])
            .unwrap()
            .scope_to_namespace(&Namespace::parse("app").unwrap())
            .unwrap()
            .try_into()
            .unwrap(),
    )
    .unwrap();
    spec
}

fn terms(workflow: DraftWorkflow) -> VolumeTerms {
    VolumeTerms {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        operation: OperationId::parse(OPERATION).unwrap(),
        namespace: Namespace::parse("app").unwrap(),
        volume: volume(),
        name: "data".to_owned(),
        servers: BTreeMap::from([
            (id('a'), "web-1".to_owned()),
            (id('b'), "web-2".to_owned()),
            (id('c'), "web-3".to_owned()),
        ]),
        workflow,
        issued_at: Timestamp::from_millis(1_762_912_093_000).unwrap(),
        signer: SignerKey::Passkey(OwnPrompt::Passkey),
    }
}

fn opened(draft: &VolumeDraft) -> VolumeApproval {
    canonical::from_canonical(&draft.body).unwrap()
}

#[test]
fn a_draft_signs_the_terms_for_fifteen_minutes_as_a_passkey() {
    let draft = terms(DraftWorkflow::Mirror {
        writer: id('a'),
        mirror: id('b'),
        replace: false,
    })
    .draft()
    .unwrap();
    let approval = opened(&draft);
    assert_eq!(
        approval.expires_at.as_millis() - approval.issued_at.as_millis(),
        15 * 60_000
    );
    assert_eq!(approval.signer, SignerKey::Passkey(OwnPrompt::Passkey));
    assert_eq!(
        canonical::to_canonical(&approval).unwrap(),
        draft.body,
        "the body is canonical"
    );
    assert_eq!(
        draft.lines,
        [
            "Approved: mirror data from web-1 to web-2. Later syncs run without asking until you remove the Mirror.",
            "  volume app_data",
            "  namespace app",
            "  writer web-1 (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa)",
            "  mirror web-2 (bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb)",
            "  replace no",
            "  operation 0f8fad5bd9cb469fa16570867728950e",
            "  valid 2025-11-12T01:48:13Z to 2025-11-12T02:03:13Z",
            r#"  cluster "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=""#,
        ]
    );
    assert!(draft.service.is_none());
}

#[test]
fn a_move_draft_starts_the_running_spec_from_the_copied_image_and_names_its_digest() {
    let draft = terms(DraftWorkflow::Move {
        from: id('a'),
        to: id('b'),
        mirrors: vec![id('c'), id('c')],
        running: Box::new(running(PINNED)),
        recorded: Some(gb(4)),
    })
    .draft()
    .unwrap();
    let start = draft.service.as_deref().unwrap();
    assert_eq!(start.container.pull_policy, PullPolicy::Never);
    assert_eq!(start.container.image, PINNED);
    let [change] = <[ApprovedChange; 1]>::try_from(
        ApprovedChange::of(
            &[start.to_requested()],
            &Namespace::parse("app").unwrap(),
            &BTreeMap::new(),
        )
        .unwrap(),
    )
    .unwrap();
    let VolumeWorkflow::Move {
        service, mirrors, ..
    } = opened(&draft).workflow
    else {
        panic!("a Move")
    };
    assert_eq!(service, change.approved(), "the digest of what B starts");
    assert_eq!(mirrors, [id('c')], "sorted and distinct");
    assert_eq!(
        draft.lines.first().unwrap(),
        "Approved: move data from web-1 to web-2. db stops on web-1 for the last copy, then starts on web-2. The Mirror on web-3 keeps syncing."
    );
    assert_eq!(
        draft.lines.get(1).unwrap(),
        &format!("Approved db: {PINNED}")
    );
    assert!(
        draft
            .lines
            .contains(&format!("  service db {}", change.digest)),
        "{:?}",
        draft.lines
    );
    assert!(
        draft
            .lines
            .iter()
            .any(|line| line.contains("4000000000") || line.contains("4GB")),
        "the recorded size the target takes shows: {:?}",
        draft.lines
    );
}

#[test]
fn a_move_of_a_service_on_a_tag_is_refused_before_anything_is_drafted() {
    let refused = terms(DraftWorkflow::Move {
        from: id('a'),
        to: id('b'),
        mirrors: Vec::new(),
        running: Box::new(running("postgres:17")),
        recorded: None,
    })
    .draft()
    .unwrap_err();
    assert!(
        refused.to_string().contains("isn't pinned by digest"),
        "{refused}"
    );
}

#[test]
fn every_workflow_drafts_its_change_and_its_terms() {
    let cases = [
        (
            DraftWorkflow::MirrorRemoval {
                mirror: id('b'),
                writer: None,
                role: SlotRole::Mirror,
            },
            "Approved: delete data's mirror on web-2. No Server writes data, so this may be its last copy, and its data is gone for good.",
            "  writer none",
        ),
        (
            DraftWorkflow::Restore {
                machine: id('b'),
                at: SnapshotGuid::new(7),
                loses_after: Some(Timestamp::from_millis(1_762_912_000_000).unwrap()),
            },
            "Approved: make the copy of data on web-2 its writer, at snapshot 7. Writes to data after 2025-11-12T01:46:40Z are lost. Nothing starts until you deploy.",
            "  loses writes after 2025-11-12T01:46:40Z",
        ),
        (
            DraftWorkflow::Resize {
                machines: vec![id('b'), id('a'), id('b')],
                maximum_bytes: gb(20),
                shrink: true,
            },
            "Approved: set data to 20GB on web-1, web-2, lowering any copy above it.",
            "  size 20000000000",
        ),
        (
            DraftWorkflow::Seed {
                machine: id('a'),
                source: DockerVolumeName::parse("app_old").unwrap(),
                at: SnapshotGuid::new(42),
            },
            "Approved: create data on web-1 from its copy of app_old at snapshot 42.",
            "  from app_old",
        ),
    ];
    for (workflow, change, term) in cases {
        let draft = terms(workflow).draft().unwrap();
        assert_eq!(draft.lines.first().unwrap(), change);
        assert!(
            draft.lines.iter().any(|line| line == term),
            "{term}: {:?}",
            draft.lines
        );
    }
}

#[test]
fn a_relayed_server_name_that_could_pass_for_another_shows_quoted() {
    let mut drafted = terms(DraftWorkflow::Mirror {
        writer: id('a'),
        mirror: id('b'),
        replace: false,
    });
    drafted
        .servers
        .insert(id('b'), "web-2 (aaaa)\u{1b}[2K".to_owned());
    let lines = drafted.draft().unwrap().lines;
    assert!(
        lines
            .first()
            .unwrap()
            .contains(r#""web-2 (aaaa)\u{1b}[2K""#),
        "{}",
        lines.first().unwrap()
    );
    assert!(
        lines.contains(
            &r#"  mirror "web-2 (aaaa)\u{1b}[2K" (bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb)"#.to_owned()
        ),
        "{lines:?}"
    );
}

#[test]
fn utc_writes_rfc_3339_to_the_second() {
    for (millis, written) in [
        (0, "1970-01-01T00:00:00Z"),
        (951_782_400_000, "2000-02-29T00:00:00Z"),
        (1_762_912_093_999, "2025-11-12T01:48:13Z"),
        (4_102_444_799_000, "2099-12-31T23:59:59Z"),
    ] {
        assert_eq!(Timestamp::from_millis(millis).unwrap().utc(), written);
    }
}

/// The input the dashboard's vitest hands the WASM drafter, and the body it must get back.
#[test]
fn the_browser_golden_draft_matches() {
    let input = json!({
        "cluster": SealingPublicKey::from_bytes([9; 32]),
        "operation": OPERATION,
        "namespace": "app",
        "volume": "app_data",
        "name": "data",
        "servers": { "a".repeat(32): "web-1", "b".repeat(32): "web-2" },
        "workflow": { "kind": "resize", "machines": ["b".repeat(32), "a".repeat(32)], "maximum_bytes": 20_000_000_000_u64, "shrink": false },
        "issued_at": 100_000,
        "signer": "passkey",
    });
    let draft = serde_json::from_value::<VolumeTerms>(input)
        .unwrap()
        .draft()
        .unwrap();
    assert_eq!(
        draft.body,
        format!(
            r#"{{"cluster":"CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=","expires_at":1000000,"issued_at":100000,"namespace":"app","operation":"{OPERATION}","signer":"passkey","volume":"app_data","workflow":{{"kind":"resize","machines":["{a}","{b}"],"maximum_bytes":20000000000,"shrink":false}}}}"#,
            a = "a".repeat(32),
            b = "b".repeat(32),
        )
    );
}
