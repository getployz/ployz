//! Smoke Pool lifecycle contracts with isolated ZFS command adapters.

use super::*;
use std::{env, os::unix::fs::PermissionsExt};

#[test]
fn smoke_pool_cleanup_preserves_backing_until_absence_is_proven() {
    const CASE: &str = "PLOYZ_SMOKE_CLEANUP_CASE";
    const ROOT: &str = "PLOYZ_SMOKE_CLEANUP_ROOT";
    if let Ok(case) = env::var(CASE) {
        let root = std::path::PathBuf::from(env::var_os(ROOT).unwrap());
        let stage = tempfile::tempdir_in(&root).unwrap();
        let directory = stage.path().to_owned();
        let backing = directory.join("backing");
        fs::write(&backing, b"pool data").unwrap();
        let result = validate_zfs_pool(stage);
        if case == "success" || case == "query-failed" {
            assert!(!directory.exists());
            assert_eq!(result.is_ok(), case == "success");
        } else {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains(&directory.display().to_string())
            );
            assert_eq!(fs::read(&backing).unwrap(), b"pool data", "{case}");
        }
        fs::write(root.join("completed"), case).unwrap();
        return;
    }

    for case in [
        "success",
        "query-failed",
        "destroy-failed",
        "pool-inspection-failed",
        "pool-remains",
        "dataset-inspection-failed",
        "dataset-remains",
        "create-unknown",
    ] {
        let root = tempfile::tempdir().unwrap();
        let commands = root.path().join("commands");
        fs::create_dir(&commands).unwrap();
        for (name, body) in [
            (
                "zpool",
                r#"
case "$1" in
    create)
        for arg do previous=${last-}; last=$arg; done
        echo "$previous" > "$PLOYZ_SMOKE_CLEANUP_ROOT/pool"
        [ "$PLOYZ_SMOKE_CLEANUP_CASE" != create-unknown ] ;;
    destroy) [ "$PLOYZ_SMOKE_CLEANUP_CASE" != destroy-failed ] ;;
    list)
        [ "$PLOYZ_SMOKE_CLEANUP_CASE" != create-unknown ] || exit 1
        if [ "$2" = -Hp ]; then
            [ "$PLOYZ_SMOKE_CLEANUP_CASE" != query-failed ]
        else
            [ "$PLOYZ_SMOKE_CLEANUP_CASE" != pool-inspection-failed ] || exit 1
            if [ "$PLOYZ_SMOKE_CLEANUP_CASE" = pool-remains ]; then
                /bin/cat "$PLOYZ_SMOKE_CLEANUP_ROOT/pool"
            fi
        fi ;;
esac
"#,
            ),
            (
                "zfs",
                r#"
if [ "$2" = -H ]; then
    [ "$PLOYZ_SMOKE_CLEANUP_CASE" != dataset-inspection-failed ] || exit 1
    if [ "$PLOYZ_SMOKE_CLEANUP_CASE" = dataset-remains ]; then
        /bin/cat "$PLOYZ_SMOKE_CLEANUP_ROOT/pool"
    fi
fi
"#,
            ),
        ] {
            let path = commands.join(name);
            fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", "installer::storage::tests::smoke_pool_cleanup_preserves_backing_until_absence_is_proven", "--nocapture"])
            .env(CASE, case)
            .env(ROOT, root.path())
            .env("PATH", commands)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(root.path().join("completed")).unwrap(),
            case
        );
    }
}
