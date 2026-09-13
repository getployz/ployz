#![expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]

use ployz_core::config::config_request;
use serde_json::{Value, json};

#[test]
fn publication_review_binds_exact_basis_removals_and_visible_revision() {
    let node = |kind, id| json!({"nodeType":kind,"nodeId":id});
    let removals = config_request(json!({"operation":"destructive_publication","value":{
        "workingNodes":[node("service","keep")],
        "savedNodes":[node("service","keep"),node("service","remove"),node("volume","volume"),node("volume","never-deployed")],
        "appliedNodes":[node("service","keep"),node("service","remove"),node("volume","volume")]
    }})).unwrap();
    assert_eq!(
        removals,
        json!({"serviceIds":["remove"],"volumeIds":["volume"]})
    );
    let mismatch = |reviewed: Value| {
        config_request(json!({"operation":"destructive_publication_mismatch","expected":removals,"reviewed":reviewed})).unwrap()
    };
    assert!(mismatch(removals.clone()).is_null());
    assert!(
        mismatch(json!({"serviceIds":[],"volumeIds":["volume"]}))
            .as_str()
            .unwrap()
            .contains("changed after review")
    );
    assert!(
        mismatch(json!({"serviceIds":["remove","remove"],"volumeIds":["volume"]}))
            .as_str()
            .unwrap()
            .contains("duplicate")
    );
    assert_eq!(config_request(json!({"operation":"publication_basis_matches","basis":{"kind":"saved_revision","savedStateSnapshotId":"reviewed"},"latest":"newer"})).unwrap(), false);
    assert_eq!(config_request(json!({"operation":"publication_basis_matches","basis":{"kind":"no_saved_state"},"latest":null})).unwrap(), true);
    let mut state = json!({"nodeSnapshots":[{"nodeType":"service","nodeId":"service","nodeLineageId":"lineage","configVersion":1,"config":{"env":{"TOKEN":{"fingerprint":"existing","encryptedValue":{"ciphertext":"a"},"parts":[]}}},"encryptedRegistrySecret":"private-credential"}],"revisionMarkers":["revision-1"]});
    let canonical = |value: Value| {
        config_request(json!({"operation":"canonical_working_review","value":value})).unwrap()
    };
    let before = canonical(state.clone());
    assert!(!before.as_str().unwrap().contains("private-credential"));
    assert!(!before.as_str().unwrap().contains("ciphertext"));
    state["nodeSnapshots"][0]["config"]["env"]["TOKEN"]["encryptedValue"]["ciphertext"] =
        json!("b");
    assert_eq!(before, canonical(state.clone()));
    state["revisionMarkers"] = json!(["revision-2"]);
    assert_ne!(before, canonical(state));
    let candidate = json!({"intent":{"version":1,"environmentSlug":"production","services":[],"variableGroups":[],"volumes":[]},"volumeDeletionAuthorizations":[]});
    assert_eq!(config_request(json!({"operation":"reuse_publication","policy":"reuse_latest_if_equivalent","current":candidate,"latest":candidate})).unwrap(), true);
    assert_eq!(config_request(json!({"operation":"reuse_publication","policy":"always_create","current":candidate,"latest":candidate})).unwrap(), false);
}
