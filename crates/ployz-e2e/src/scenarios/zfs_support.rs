use crate::error::{Error, Result};
use crate::runner::ScenarioRun;
use crate::support::wait_until;
use std::time::Duration;

pub(crate) const VOLUME_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const VOLUME_TARGET: &str = "/var/lib/postgresql/data";
const VOLUME_DATASET: &str = "default/data";

#[derive(Debug)]
pub(crate) struct ZfsContext {
    pub(crate) mode: String,
    pub(crate) root: String,
    pub(crate) root_mountpoint: String,
}

impl ZfsContext {
    pub(crate) fn volume_source(&self) -> String {
        format!("{}/{}", self.root_mountpoint, VOLUME_DATASET)
    }

    pub(crate) fn volume_dataset(&self) -> String {
        format!("{}/{}", self.root, VOLUME_DATASET)
    }
}

pub(crate) fn zfs_context(run: &ScenarioRun, node_name: &str) -> Result<ZfsContext> {
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

pub(crate) fn deploy_volume_manifest(
    run: &ScenarioRun,
    node_name: &str,
    value: &str,
) -> Result<()> {
    write_volume_manifest(run, node_name, value)?;
    run.ssh_expect_ok_name(node_name, "ployzd deploy -f /tmp/ployz-volume-smoke.json")?;
    Ok(())
}

pub(crate) fn write_volume_manifest(run: &ScenarioRun, node_name: &str, value: &str) -> Result<()> {
    let manifest = volume_manifest(value);
    let command = format!("cat >/tmp/ployz-volume-smoke.json <<'EOF'\n{manifest}\nEOF");
    run.ssh_expect_ok_name(node_name, &command)?;
    Ok(())
}

fn volume_manifest(value: &str) -> String {
    format!(
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
        "command": ["sh", "-c", "test -f {VOLUME_TARGET}/value || printf '{value}\\n' >{VOLUME_TARGET}/value; sleep 3600"],
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
    )
}

pub(crate) fn wait_for_volume_value(
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

pub(crate) fn wait_for_container_bind(
    run: &ScenarioRun,
    node_name: &str,
    volume_source: &str,
) -> Result<()> {
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

pub(crate) fn wait_for_no_service_container(run: &ScenarioRun, node_name: &str) -> Result<()> {
    let command = "test -z \"$(docker ps -q --filter label=dev.ployz.namespace=default --filter label=dev.ployz.service=db)\"";
    wait_until(VOLUME_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, command)?;
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "db container on {node_name} was still running after migrate: {error}"
        ))
    })
}

pub(crate) fn assert_real_zfs_dataset(
    run: &ScenarioRun,
    node_name: &str,
    zfs: &ZfsContext,
) -> Result<()> {
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
