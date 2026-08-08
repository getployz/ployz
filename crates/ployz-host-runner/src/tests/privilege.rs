use std::path::{Path, PathBuf};

use crate::execution::{
    API_GROUP, API_USER, ApiRuntimeAccessMatrix, CONTROL_GROUP, HostPackageFamily,
    HostRunnerCommandOutput, HostRunnerCommandRunner, InstalledIdentity, SupervisorDirectories,
    UnixGroup, api_privilege_install_commands, migrate_existing_systemd_api_privileges,
};
use ployz_core::operation::FailureMessage;

#[test]
fn api_access_matrix_is_narrow_and_excludes_an_unrelated_identity() {
    let matrix = ApiRuntimeAccessMatrix::for_state(Path::new("/var/lib/ployz"));
    let api = InstalledIdentity::api();
    let unrelated = InstalledIdentity::unrelated();

    assert!(matrix.entries().iter().all(|entry| entry.allows(&api)));
    assert!(
        matrix
            .entries()
            .iter()
            .all(|entry| !entry.allows(&unrelated))
    );
    assert_eq!(api.user(), API_USER);
    assert_eq!(api.primary_group(), API_GROUP);
    assert_eq!(
        api.supplementary_groups(),
        [UnixGroup::Docker, CONTROL_GROUP]
    );

    let rendered = matrix
        .entries()
        .iter()
        .map(|entry| {
            format!(
                "{} {}:{} {:04o}",
                entry.path().display(),
                entry.owner(),
                entry.group(),
                entry.mode(),
            )
        })
        .collect::<Vec<_>>();
    for expected in [
        "/var/lib/ployz root:ployz-api 0710",
        "/var/lib/ployz/door.key root:ployz-api 0640",
        "/var/lib/ployz/door.crt root:ployz-api 0640",
        "/var/lib/ployz/door.fingerprint root:ployz-api 0640",
        "/var/lib/ployz/join-substrate.json root:ployz-api 0640",
        "/var/lib/ployz/api/evidence ployz-api:ployz-api 0750",
        "/var/lib/ployz/api/lease ployz-api:ployz-api 0700",
        "/var/lib/ployz/api/upgrade-staging ployz-api:ployz-control 0770",
        "/var/run/docker.sock root:docker 0660",
        "/run/ployz root:ployz-control 0750",
        "/run/ployz/keeper-upgrade.sock root:ployz-control 0660",
        "/run/ployz-control root:ployz-control 0750",
        "/run/ployz-control/keeper-control.sock root:ployz-control 0660",
    ] {
        assert!(
            rendered.iter().any(|entry| entry == expected),
            "{rendered:?}"
        );
    }
}

#[derive(Default)]
struct MigrationRunner {
    calls: Vec<String>,
}

impl HostRunnerCommandRunner for MigrationRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.calls.push(format!("{program} {}", args.join(" ")));
        Ok(HostRunnerCommandOutput {
            success: true,
            exit_code: Some(0),
            stdout: if program == "cat" {
                "ID=ubuntu\nVERSION_ID=24.04\n".to_owned()
            } else {
                String::new()
            },
            stdout_truncated: false,
            failure: String::new(),
        })
    }

    fn is_linux(&mut self) -> bool {
        true
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        Ok(0)
    }

    fn download(&mut self, _url: &str, _destination: &Path) -> Result<(), FailureMessage> {
        unreachable!("privilege migration does not download")
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        unreachable!("privilege migration does not inspect Docker")
    }

    fn docker_is_installed(&mut self) -> bool {
        true
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        unreachable!("privilege migration does not inspect Docker")
    }

    fn docker_has_insecure_registry(&mut self, _cidr: &str) -> Result<bool, FailureMessage> {
        unreachable!("privilege migration does not inspect Docker")
    }
}

#[cfg(unix)]
#[test]
fn current_root_layout_migrates_once_and_stable_units_remain_rollback_compatible() {
    let root = tempfile::tempdir().expect("migration root");
    let state = root.path().join("state");
    let systemd = root.path().join("systemd");
    std::fs::create_dir_all(&state).expect("state directory");
    std::fs::create_dir_all(&systemd).expect("systemd directory");
    std::fs::write(
        state.join("ployzd.env"),
        "PLOYZ_MACHINE_ID=01ARZ3NDEKTSV4RRFFQ69G5FAW\nPLOYZ_CORROSION_API_ADDR=127.0.0.1:8080\nPLOYZ_CORROSION_BEARER_TOKEN=secret\nPLOYZ_CLUSTER_ID=01ARZ3NDEKTSV4RRFFQ69G5FAV\nPLOYZ_API_LISTEN_ADDR=[::1]:2020\n",
    )
    .expect("legacy shared environment");
    for role in ["keeper", "api", "gateway", "dns"] {
        std::fs::write(
            systemd.join(format!("ployzd-{role}.service")),
            format!("[Service]\nExecStart={}/current {role}\n", state.display()),
        )
        .expect("root-running current unit");
    }
    let directories = SupervisorDirectories::new(systemd.clone(), root.path().join("openrc"));
    let mut runner = MigrationRunner::default();

    let first = migrate_existing_systemd_api_privileges(&state, &directories, &mut runner)
        .expect("current layout migrates");
    assert_eq!(first.rewritten_units(), 4);
    let api =
        std::fs::read_to_string(systemd.join("ployzd-api.service")).expect("migrated API unit");
    assert!(api.contains("User=ployz-api\nGroup=ployz-api\n"), "{api}");
    assert!(
        api.contains("SupplementaryGroups=docker ployz-control\n"),
        "{api}"
    );
    assert!(api.contains(&format!("ExecStart={}/current api", state.display())));
    let gateway = std::fs::read_to_string(systemd.join("ployzd-gateway.service"))
        .expect("migrated Gateway unit");
    assert!(gateway.contains("User=ployz-gateway\nGroup=ployz-gateway\n"));
    assert!(gateway.contains(&format!(
        "BindReadOnlyPaths={}/current:/run/ployz-gateway/ployzd",
        state.display()
    )));
    assert!(gateway.contains(&format!(
        "EnvironmentFile={}/ployz-gateway.env",
        state.display()
    )));
    let gateway_environment =
        std::fs::read_to_string(state.join("ployz-gateway.env")).expect("gateway environment");
    assert_eq!(
        gateway_environment,
        "PLOYZ_CORROSION_API_ADDR=127.0.0.1:8080\nPLOYZ_CORROSION_BEARER_TOKEN=secret\nPLOYZ_CLUSTER_ID=01ARZ3NDEKTSV4RRFFQ69G5FAV\nPLOYZ_GATEWAY_LISTEN_ADDR=0.0.0.0:80\n"
    );
    let reload = runner
        .calls
        .iter()
        .position(|call| call == "systemctl daemon-reload")
        .expect("migration reloads systemd");
    let first_verification = runner
        .calls
        .iter()
        .position(|call| call.contains("stat -c"))
        .expect("migration verifies access paths");
    assert!(reload < first_verification);
    assert!(runner.calls.iter().any(|call| {
        call.contains("chown root:ployz-control") && call.contains("keeper-upgrade.sock")
    }));

    let repeat_start = runner.calls.len();
    let second = migrate_existing_systemd_api_privileges(&state, &directories, &mut runner)
        .expect("repeated migration remains converged");
    assert_eq!(second.rewritten_units(), 0);
    let Some(repeat_calls) = runner.calls.get(repeat_start..) else {
        panic!("repeat start is within the recorded command list")
    };
    assert!(repeat_calls.iter().all(|call| {
        !call.starts_with("install ")
            && !call.starts_with("chown ")
            && !call.starts_with("chmod ")
            && !call.starts_with("usermod ")
            && call != "systemctl daemon-reload"
    }));

    std::os::unix::fs::symlink(PathBuf::from("artifacts/old"), state.join("previous"))
        .expect("rollback target");
    std::os::unix::fs::symlink(state.join("previous"), state.join("current"))
        .expect("rollback current link");
    assert_eq!(
        std::fs::read_to_string(systemd.join("ployzd-api.service"))
            .expect("rollback keeps migrated unit"),
        api
    );
    assert_eq!(
        std::fs::read_link(state.join("current")).expect("rollback link"),
        state.join("previous")
    );
}

#[test]
fn account_and_path_install_commands_are_idempotent_and_platform_specific() {
    let systemd =
        api_privilege_install_commands(Path::new("/var/lib/ployz"), HostPackageFamily::Debian)
            .expect("systemd-host plan");
    let openrc =
        api_privilege_install_commands(Path::new("/var/lib/ployz"), HostPackageFamily::Alpine)
            .expect("OpenRC-host plan");
    let systemd = systemd.iter().map(ToString::to_string).collect::<Vec<_>>();
    let openrc = openrc.iter().map(ToString::to_string).collect::<Vec<_>>();

    assert!(
        systemd
            .iter()
            .any(|command| command.contains("groupadd --system ployz-api"))
    );
    assert!(
        systemd
            .iter()
            .any(|command| command.contains("groupadd --system ployz-gateway"))
    );
    assert!(
        systemd
            .iter()
            .any(|command| command.contains("useradd --system --gid ployz-gateway"))
    );
    assert!(
        systemd
            .iter()
            .any(|command| command.contains("useradd --system"))
    );
    assert!(
        openrc
            .iter()
            .any(|command| command.contains("addgroup -S ployz-gateway"))
    );
    assert!(
        openrc
            .iter()
            .any(|command| command.contains("-G ployz-gateway ployz-gateway"))
    );
    assert!(
        systemd
            .iter()
            .any(|command| { command == "usermod --groups docker,ployz-control ployz-api" })
    );
    assert!(
        openrc
            .iter()
            .any(|command| command.contains("addgroup -S ployz-api"))
    );
    assert!(
        openrc
            .iter()
            .any(|command| command.contains("adduser -S -D -H"))
    );
    assert!(
        openrc
            .iter()
            .any(|command| command.contains("addgroup ployz-api docker"))
    );

    for commands in [&systemd, &openrc] {
        assert!(commands.iter().any(|command| {
            command == "install -d -o root -g ployz-api -m 0710 /var/lib/ployz"
        }));
        assert!(commands.iter().any(|command| {
            command == "install -d -o root -g ployz-control -m 0750 /run/ployz-control"
        }));
        assert!(commands.iter().any(|command| {
            command == "install -d -o ployz-api -g ployz-api -m 0700 /var/lib/ployz/api/lease"
        }));
        assert!(
            commands
                .iter()
                .any(|command| { command == "chown root:ployz-api /var/lib/ployz/door.key" })
        );
        assert!(
            commands
                .iter()
                .any(|command| { command == "chmod 0640 /var/lib/ployz/join-substrate.json" })
        );
    }
}
