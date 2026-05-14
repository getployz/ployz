use ployz_model::MachineId;

use super::request::{first_duplicate, update_targets_with_self_last};
use super::version::{
    installer_version_argument, normalize_requested_version, requested_version_matches_current,
};

#[test]
fn version_normalization_trims_and_drops_v_prefix() {
    assert_eq!(
        normalize_requested_version(" v0.5.3-alpha.1 "),
        "0.5.3-alpha.1"
    );
    assert_eq!(normalize_requested_version("latest"), "latest");
}

#[test]
fn requested_version_matches_current_build() {
    assert!(requested_version_matches_current(env!("CARGO_PKG_VERSION")));
    assert!(requested_version_matches_current(&format!(
        "v{}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(!requested_version_matches_current("latest"));
}

#[test]
fn duplicate_detection_returns_first_repeated_value() {
    let values = vec!["a".to_string(), "b".to_string(), "a".to_string()];
    assert_eq!(first_duplicate(values.as_slice()).as_deref(), Some("a"));
}

#[test]
fn duplicate_detection_catches_repeated_self() {
    let values = vec!["self".to_string(), "self".to_string()];
    assert_eq!(first_duplicate(values.as_slice()).as_deref(), Some("self"));
}

#[test]
fn explicit_self_target_is_moved_last() {
    let targets = update_targets_with_self_last(
        &[
            "self".to_string(),
            "remote-a".to_string(),
            "remote-b".to_string(),
        ],
        &MachineId::new("self"),
    );

    assert_eq!(targets, vec!["remote-a", "remote-b", "self"]);
}

#[test]
fn installer_version_argument_re_adds_v_prefix_for_pinned_versions() {
    assert_eq!(installer_version_argument("0.5.3"), "v0.5.3");
    assert_eq!(
        installer_version_argument("0.5.3-alpha.1"),
        "v0.5.3-alpha.1"
    );
    assert_eq!(installer_version_argument("latest"), "latest");
}
