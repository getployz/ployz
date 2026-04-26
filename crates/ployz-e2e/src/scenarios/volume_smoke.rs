use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::wait_until;
use std::time::Duration;

const VOLUME_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const VOLUME_TARGET: &str = "/var/lib/postgresql/data";
const VOLUME_DATASET: &str = "default/data";

#[derive(Debug)]
struct ZfsContext {
    mode: String,
    root: String,
    root_mountpoint: String,
}

impl ZfsContext {
    fn volume_source(&self) -> String {
        format!("{}/{}", self.root_mountpoint, VOLUME_DATASET)
    }

    fn volume_dataset(&self) -> String {
        format!("{}/{}", self.root, VOLUME_DATASET)
    }
}

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.wait_machine_rows(
        "founder",
        &[MachineExpectation {
            id: "founder",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        }],
    )?;

    run.log_progress("verify zfs configured");
    let zfs = zfs_context(run, "founder")?;

    run.log_progress("deploy managed volume manifest v1");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &zfs.volume_source())?;
    assert_zfs_mode_state(run, "founder", &zfs)?;

    run.log_progress("deploy managed volume manifest v2");
    deploy_volume_manifest(run, "founder", "v2")?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v2")?;
    wait_for_container_bind(run, "founder", &zfs.volume_source())?;
    assert_zfs_mode_state(run, "founder", &zfs)?;

    Ok(())
}

fn zfs_context(run: &ScenarioRun, node_name: &str) -> Result<ZfsContext> {
    let output = run.ssh_expect_ok_name(
        node_name,
        "mode=$(cat /var/lib/ployz-e2e-zfs/mode); \
         root=$(awk -F '\"' '/zfs_root/ { print $2; exit }' /root/.config/ployz/config.toml); \
         test -n \"$mode\"; test -n \"$root\"; \
         mountpoint=$(zfs list -H -o mountpoint \"$root\"); \
         printf '%s\n%s\n%s\n' \"$mode\" \"$root\" \"$mountpoint\"",
    )?;
    let mut lines = output.stdout.lines();
    let Some(mode) = lines.next() else {
        return Err(Error::Message("zfs mode was not reported".to_string()));
    };
    let Some(root) = lines.next() else {
        return Err(Error::Message("zfs root was not reported".to_string()));
    };
    let Some(root_mountpoint) = lines.next() else {
        return Err(Error::Message(
            "zfs root mountpoint was not reported".to_string(),
        ));
    };
    Ok(ZfsContext {
        mode: mode.to_string(),
        root: root.to_string(),
        root_mountpoint: root_mountpoint.trim_end_matches('/').to_string(),
    })
}

fn deploy_volume_manifest(run: &ScenarioRun, node_name: &str, value: &str) -> Result<()> {
    let manifest = format!(
        r#"{{
  "namespace": "default",
  "volumes": [
    {{
      "name": "data",
      "scope": "single",
      "quota": "1G",
      "mode": "0750",
      "owner": "999:999"
    }}
  ],
  "services": [
    {{
      "name": "db",
      "placement": {{"replicated": {{"count": 1}}}},
      "template": {{
        "image": "ployz-e2e-preload/http-smoke:latest",
        "command": ["sh", "-c", "printf '{value}\\n' >{VOLUME_TARGET}/value && sleep 3600"],
        "mounts": [
          {{
            "source": {{"volume": "data"}},
            "target": "{VOLUME_TARGET}"
          }}
        ]
      }},
      "network": "overlay"
    }}
  ]
}}"#
    );
    let command = format!(
        "cat >/tmp/ployz-volume-smoke.json <<'EOF'\n{manifest}\nEOF\nployzd deploy -f /tmp/ployz-volume-smoke.json"
    );
    run.ssh_expect_ok_name(node_name, &command)?;
    Ok(())
}

fn wait_for_volume_value(
    run: &ScenarioRun,
    node_name: &str,
    volume_source: &str,
    value: &str,
) -> Result<()> {
    let command = format!("test \"$(cat {volume_source}/value 2>/dev/null)\" = '{value}'");
    wait_until(VOLUME_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "managed volume on {node_name} did not contain value '{value}': {error}"
        ))
    })
}

fn wait_for_container_bind(run: &ScenarioRun, node_name: &str, volume_source: &str) -> Result<()> {
    let command = format!(
        "container_id=$(docker ps -q --filter label=dev.ployz.namespace=default --filter label=dev.ployz.service=db | head -n1); \
         test -n \"$container_id\"; \
         docker inspect --format '{{{{range .Mounts}}}}{{{{println .Source \"->\" .Destination}}}}{{{{end}}}}' \"$container_id\" | \
         grep -Fx '{volume_source} -> {VOLUME_TARGET}'"
    );
    wait_until(VOLUME_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "db container on {node_name} did not have managed volume bind {volume_source}:{VOLUME_TARGET}: {error}"
        ))
    })
}

fn assert_zfs_mode_state(run: &ScenarioRun, node_name: &str, zfs: &ZfsContext) -> Result<()> {
    match zfs.mode.as_str() {
        "fake" => assert_zfs_create_count(run, node_name, 1),
        "real" => assert_real_zfs_dataset(run, node_name, zfs),
        other => Err(Error::Message(format!(
            "unsupported zfs e2e mode '{other}'"
        ))),
    }
}

fn assert_zfs_create_count(run: &ScenarioRun, node_name: &str, expected: usize) -> Result<()> {
    let output = run.ssh_expect_ok_name(
        node_name,
        "grep -c '^zfs create ' /var/lib/ployz-e2e-zfs/calls.log",
    )?;
    let actual = output
        .stdout
        .trim()
        .parse::<usize>()
        .map_err(|error| Error::Message(format!("parse fake zfs create count: {error}")))?;
    if actual == expected {
        return Ok(());
    }
    Err(Error::Message(format!(
        "expected fake zfs create count {expected}, got {actual}"
    )))
}

fn assert_real_zfs_dataset(run: &ScenarioRun, node_name: &str, zfs: &ZfsContext) -> Result<()> {
    let dataset = zfs.volume_dataset();
    let source = zfs.volume_source();
    let command = format!(
        "test \"$(zfs list -H -o mountpoint {dataset})\" = '{source}'; \
         quota=$(zfs get -H -o value quota {dataset}); \
         case \"$quota\" in 1G|1.00G|1073741824) exit 0 ;; *) echo \"unexpected quota: $quota\" >&2; exit 1 ;; esac"
    );
    run.ssh_expect_ok_name(node_name, &command)?;
    Ok(())
}
