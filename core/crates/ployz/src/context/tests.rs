//! Saved context mutation and credential persistence contracts.

#[test]
fn rotation_updates_latest_contexts_and_preserves_other_connections() {
    use super::*;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let old =
        ManagementCapability::new(ployz_core::ManagementIdentity::from_bytes([1; 32]), [2; 32]);
    let new = ManagementCapability::new(*old.machine(), [3; 32]);
    let connection = Connection::management(old.to_secret_string()).unwrap();
    let config = Config::new(
        dir.path().join("config.yaml"),
        Some("first".into()),
        BTreeMap::from([(
            "first".into(),
            Context {
                connections: vec![connection.clone()],
            },
        )]),
    );
    config.save().unwrap();
    // Another writer adds a context while enrollment is in flight.
    let mut latest = Config::load(config.path()).unwrap();
    let unix = Connection::unix("/tmp/ployz.sock").unwrap();
    latest.contexts.insert(
        "second".into(),
        Context {
            connections: vec![unix.clone(), connection],
        },
    );
    latest.save().unwrap();
    config.save_management_capability(&new).unwrap();
    let saved = Config::load(config.path()).unwrap();
    let replacement = Connection::management(new.to_secret_string()).unwrap();
    assert_eq!(
        saved.contexts.get("first").unwrap().connections,
        vec![replacement.clone()]
    );
    assert_eq!(
        saved.contexts.get("second").unwrap().connections,
        vec![unix, replacement]
    );
    assert_eq!(saved.current_context(), Some("first"));
    let unrelated =
        ManagementCapability::new(ployz_core::ManagementIdentity::from_bytes([4; 32]), [5; 32]);
    assert!(matches!(
        config.save_management_capability(&unrelated),
        Err(ConfigError::ManagementConnectionMissing)
    ));
    assert_eq!(Config::load(config.path()).unwrap(), saved);
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        config.save_management_capability(&old),
        Err(ConfigError::PrivatePermissions(_))
    ));
}

#[test]
fn connection_sources_are_plain_text() {
    use super::ConnectionSource;
    assert_eq!(
        ConnectionSource::Direct.to_string(),
        "the explicit connection"
    );
    assert_eq!(
        ConnectionSource::LocalSocket.to_string(),
        "the local socket"
    );
    assert_eq!(
        ConnectionSource::Context("prod".into()).to_string(),
        "context prod"
    );
}

use std::{collections::BTreeMap, fs, path::PathBuf};

use super::{Config, Context, ContextError, RemovedContext};

#[test]
fn removing_a_non_current_context_leaves_current_and_the_other_entry() {
    let mut config = Config::new(
        "/tmp/config.yaml",
        Some("prod".into()),
        BTreeMap::from([
            ("default".into(), Context::default()),
            ("prod".into(), Context::default()),
        ]),
    );

    assert_eq!(
        config.remove_context("default").unwrap(),
        RemovedContext::Other
    );
    assert_eq!(config.current_context(), Some("prod"));
    assert!(config.contexts.contains_key("prod"));
    assert!(!config.contexts.contains_key("default"));
}

#[test]
fn removing_the_current_context_unsets_current_and_drops_that_entry() {
    let mut config = Config::new(
        "/tmp/config.yaml",
        Some("prod".into()),
        BTreeMap::from([
            ("default".into(), Context::default()),
            ("prod".into(), Context::default()),
        ]),
    );

    assert_eq!(
        config.remove_context("prod").unwrap(),
        RemovedContext::Current
    );
    assert_eq!(config.current_context(), None);
    assert!(config.contexts.contains_key("default"));
    assert!(!config.contexts.contains_key("prod"));
}

#[test]
fn removing_the_last_context_leaves_an_empty_map_and_no_current() {
    let mut config = Config::new(
        "/tmp/config.yaml",
        Some("default".into()),
        BTreeMap::from([("default".into(), Context::default())]),
    );

    assert_eq!(
        config.remove_context("default").unwrap(),
        RemovedContext::Current
    );
    assert!(config.contexts.is_empty());
    assert_eq!(config.current_context(), None);
}

#[test]
fn removing_a_missing_context_is_context_not_found_and_does_not_mutate() {
    let path = PathBuf::from("/tmp/config.yaml");
    let mut config = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([("prod".into(), Context::default())]),
    );
    let before = config.clone();

    assert_eq!(
        config.remove_context("gone"),
        Err(ContextError::ContextNotFound {
            name: "gone".into(),
            path,
        })
    );
    assert_eq!(config, before);
}

#[test]
fn new_config_with_a_dangling_current_name_stores_none() {
    let config = Config::new(
        "/tmp/config.yaml",
        Some("gone".into()),
        BTreeMap::from([("prod".into(), Context::default())]),
    );

    assert_eq!(config.current_context(), None);
    assert!(config.contexts.contains_key("prod"));
}

#[test]
fn dangling_current_context_yaml_loads_as_none() {
    let root = std::env::temp_dir().join(format!(
        "ployz-dangling-current-yaml-{}",
        std::process::id()
    ));
    let path = root.join("config.yaml");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        &path,
        "current_context: gone\ncontexts:\n  prod:\n    connections: []\n",
    )
    .unwrap();

    let config = Config::load(&path).unwrap();
    assert_eq!(config.current_context(), None);
    assert!(config.contexts.contains_key("prod"));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn set_current_context_rejects_an_unknown_name() {
    let path = PathBuf::from("/tmp/config.yaml");
    let mut config = Config::new(
        &path,
        Some("prod".into()),
        BTreeMap::from([("prod".into(), Context::default())]),
    );

    assert_eq!(
        config.set_current_context(Some("gone".into())),
        Err(ContextError::ContextNotFound {
            name: "gone".into(),
            path,
        })
    );
    assert_eq!(config.current_context(), Some("prod"));
}
