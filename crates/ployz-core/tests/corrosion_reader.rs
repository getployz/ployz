use ployz_core::corrosion::{
    MalformedDocument, NamespaceDocument, RowSkipReason, StoredRow, read_named_rows, read_rows,
};
use ployz_core::ids::ClusterId;

const CLUSTER_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const LOWER_ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const HIGHER_ROW_ID: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";

fn cluster_id() -> ClusterId {
    ClusterId::try_new(CLUSTER_ID).expect("fixture cluster id is canonical")
}

fn namespace_document(name: &str) -> String {
    format!(r#"{{"v":1,"cluster_id":"{CLUSTER_ID}","name":"{name}"}}"#)
}

#[test]
fn empty_and_malformed_rows_stay_visible_as_typed_skips() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new("whitespace", "  \n"),
            StoredRow::new("zero-key-object", "{}"),
            StoredRow::new("invalid-json", "{"),
            StoredRow::new(
                "missing-version",
                format!(r#"{{"cluster_id":"{CLUSTER_ID}","name":"alpha"}}"#),
            ),
            StoredRow::new(
                "unsupported-old-version",
                format!(r#"{{"v":0,"cluster_id":"{CLUSTER_ID}","name":"alpha"}}"#),
            ),
        ],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [whitespace, zero_key, invalid_json, missing_version, unsupported_version]
            if matches!(whitespace.reason, RowSkipReason::Empty)
                && whitespace.source.key == "whitespace"
                && whitespace.source.document == "  \n"
                && matches!(zero_key.reason, RowSkipReason::Empty)
                && matches!(
                    invalid_json.reason,
                    RowSkipReason::Malformed(MalformedDocument::InvalidJson { .. })
                )
                && matches!(
                    missing_version.reason,
                    RowSkipReason::Malformed(MalformedDocument::MissingVersion)
                )
                && matches!(
                    unsupported_version.reason,
                    RowSkipReason::Malformed(MalformedDocument::UnsupportedVersion { found: 0 })
                )
    ));
}

#[test]
fn raw_cluster_fence_wins_before_other_header_validation() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [StoredRow::new(
            LOWER_ROW_ID,
            r#"{"v":"not-an-integer","cluster_id":"not-a-ulid","name":42}"#,
        )],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if matches!(
                &skipped.reason,
                RowSkipReason::ForeignCluster { expected, found }
                    if expected == CLUSTER_ID && found == "not-a-ulid"
            )
    ));
}

#[test]
fn newer_version_is_skipped_without_interpreting_its_payload() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [StoredRow::new(
            LOWER_ROW_ID,
            format!(r#"{{"v":2,"cluster_id":"{CLUSTER_ID}","name":42}}"#),
        )],
    );

    assert!(report.accepted.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped]
            if matches!(
                skipped.reason,
                RowSkipReason::NewerVersion { found: 2, supported: 1 }
            )
    ));
}

#[test]
fn additive_unknown_fields_do_not_hide_a_current_document() {
    let report = read_rows::<NamespaceDocument>(
        &cluster_id(),
        [StoredRow::new(
            LOWER_ROW_ID,
            format!(
                r#"{{"v":1,"cluster_id":"{CLUSTER_ID}","name":"alpha","future":{{"enabled":true}}}}"#
            ),
        )],
    );

    assert!(report.skipped.is_empty());
    assert!(matches!(report.accepted.as_slice(), [accepted] if accepted.value.name == "alpha"));
}

#[test]
fn lowest_canonical_ulid_wins_and_the_duplicate_remains_visible() {
    let report = read_named_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new(HIGHER_ROW_ID, namespace_document("alpha")),
            StoredRow::new(LOWER_ROW_ID, namespace_document("alpha")),
        ],
    );

    assert!(report.skipped.is_empty());
    assert!(matches!(
        report.accepted.as_slice(),
        [winner] if winner.id.as_str() == LOWER_ROW_ID && winner.value.name == "alpha"
    ));
    assert!(matches!(
        report.shadows.as_slice(),
        [conflict]
            if conflict.winner.id.as_str() == LOWER_ROW_ID
                && conflict.loser.id.as_str() == HIGHER_ROW_ID
                && conflict.winner.source.document == namespace_document("alpha")
                && conflict.loser.source.document == namespace_document("alpha")
    ));
}

#[test]
fn invalid_row_id_cannot_win_or_become_a_shadow() {
    let report = read_named_rows::<NamespaceDocument>(
        &cluster_id(),
        [
            StoredRow::new("0000000000000000000000000!", namespace_document("alpha")),
            StoredRow::new(HIGHER_ROW_ID, namespace_document("alpha")),
        ],
    );

    assert!(matches!(
        report.accepted.as_slice(),
        [winner] if winner.id.as_str() == HIGHER_ROW_ID
    ));
    assert!(report.shadows.is_empty());
    assert!(matches!(
        report.skipped.as_slice(),
        [skipped] if matches!(skipped.reason, RowSkipReason::InvalidRowId { .. })
    ));
}
