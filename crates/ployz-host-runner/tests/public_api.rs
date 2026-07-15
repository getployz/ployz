#[test]
fn host_runner_keeps_implementation_capabilities_private() {
    let library = include_str!("../src/lib.rs");

    for capability in ["execution", "lifecycle", "plan", "recovery"] {
        assert!(
            library.contains(&format!("mod {capability};")),
            "missing private {capability} capability"
        );
        assert!(
            !library.contains(&format!("pub mod {capability};")),
            "implementation capability {capability} remains public"
        );
    }

    for transitional in [
        "artifacts",
        "command",
        "cloud_client",
        "core_demote",
        "executor",
        "firewall",
        "fsx",
        "host_platform",
        "local",
        "nats_identity",
        "recovery_secret",
        "report",
        "release_manifest",
        "steps",
        "supervisor",
        "systemd",
    ] {
        assert!(
            !library.contains(&format!("pub mod {transitional};")),
            "transitional module {transitional} remains public"
        );
    }
}

#[test]
fn recovery_capability_groups_the_local_recovery_seams() {
    let recovery = include_str!("../src/recovery/mod.rs");

    for seam in [
        "mod demote;",
        "mod environment;",
        "mod identity;",
        "mod promote;",
        "mod secret;",
    ] {
        assert!(recovery.contains(seam), "missing recovery seam {seam}");
    }
}
