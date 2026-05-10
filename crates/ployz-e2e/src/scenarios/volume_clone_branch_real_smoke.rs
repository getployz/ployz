use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::parse_daemon_json_response;
use serde_json::Value;

use super::zfs_support::{
    VOLUME_TARGET, assert_real_zfs_dataset, assert_real_zfs_dataset_in_namespace,
    deploy_volume_manifest, wait_for_container_bind, wait_for_container_bind_in_namespace,
    wait_for_volume_value, wait_for_volume_value_in_namespace, zfs_context,
};

const BRANCH_NAMESPACE: &str = "pr-39";

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

    run.log_progress("verify real zfs configured");
    let zfs = zfs_context(run, "founder")?;
    if zfs.mode != "real" {
        return Err(Error::Message(format!(
            "volume clone branch smoke requires real zfs, got founder={}",
            zfs.mode
        )));
    }

    run.log_progress("deploy source namespace volume");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &zfs.volume_source())?;
    assert_real_zfs_dataset(run, "founder", &zfs)?;

    run.log_progress("mutate source before branch clone");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v2\\n' >{}/value", zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v2")?;

    run.log_progress("write branch clone manifest");
    write_branch_clone_manifest(run)?;

    run.log_progress("preview branch clone");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy preview -f /tmp/ployz-volume-branch.json",
    )?;
    assert_volume_clone(&output.stdout, "deploy preview")?;

    run.log_progress("apply branch clone");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy -f /tmp/ployz-volume-branch.json",
    )?;
    assert_volume_clone(&output.stdout, "deploy apply")?;

    run.log_progress("verify cloned namespace has source snapshot data");
    wait_for_volume_value_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data", "v2")?;
    wait_for_container_bind_in_namespace(
        run,
        "founder",
        BRANCH_NAMESPACE,
        "db",
        &zfs.volume_source_for(BRANCH_NAMESPACE, "data"),
    )?;
    assert_real_zfs_dataset_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data")?;

    run.log_progress("verify source and branch diverge after clone");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v3\\n' >{}/value", zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v3")?;
    wait_for_volume_value_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data", "v2")?;

    run.log_progress("reapply branch clone manifest");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy -f /tmp/ployz-volume-branch.json",
    )?;
    assert_no_volume_clone(&output.stdout, "branch clone reapply")?;
    wait_for_volume_value_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data", "v2")?;

    Ok(())
}

fn write_branch_clone_manifest(run: &ScenarioRun) -> Result<()> {
    let manifest = format!(
        r#"{{
  "namespace": "{BRANCH_NAMESPACE}",
  "intent": {{
    "volumes": [
      {{
        "volume": "data",
        "intent": {{
          "clone": {{
            "source_namespace": "default",
            "source_volume": "data",
            "data_policy": "raw",
            "consistency": "crash_consistent"
          }}
        }}
      }}
    ]
  }},
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
        "command": ["sh", "-c", "test -f {VOLUME_TARGET}/value || printf 'branch\\n' >{VOLUME_TARGET}/value; sleep 3600"],
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
    let command = format!("cat >/tmp/ployz-volume-branch.json <<'EOF'\n{manifest}\nEOF");
    run.ssh_expect_ok_name("founder", &command)?;
    Ok(())
}

fn assert_volume_clone(output: &str, context: &str) -> Result<()> {
    let preview = deploy_preview(output, context)?;
    let Some(volume_clones) = preview.get("volume_clones").and_then(Value::as_array) else {
        return Err(Error::Message(format!(
            "{context} did not include preview.volume_clones: {preview}"
        )));
    };
    if volume_clones.iter().any(|clone_plan| {
        clone_plan.get("volume").and_then(Value::as_str) == Some("data")
            && clone_plan.get("source_namespace").and_then(Value::as_str) == Some("default")
            && clone_plan.get("source_volume").and_then(Value::as_str) == Some("data")
            && clone_plan.get("source_machine").and_then(Value::as_str) == Some("founder")
            && clone_plan.get("target_machine").and_then(Value::as_str) == Some("founder")
    }) {
        return Ok(());
    }
    Err(Error::Message(format!(
        "{context} did not plan default/data clone on founder: {preview}"
    )))
}

fn assert_no_volume_clone(output: &str, context: &str) -> Result<()> {
    let preview = deploy_preview(output, context)?;
    let Some(volume_clones) = preview.get("volume_clones").and_then(Value::as_array) else {
        return Ok(());
    };
    if volume_clones.is_empty() {
        return Ok(());
    }
    Err(Error::Message(format!(
        "{context} unexpectedly planned volume clones: {preview}"
    )))
}

fn deploy_preview(output: &str, context: &str) -> Result<Value> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "{context} failed [{}]: {}",
            response.code, response.message
        )));
    }
    let body: Value = serde_json::from_str(&response.message)
        .map_err(|error| Error::Message(format!("failed to parse {context} message: {error}")))?;
    Ok(body.get("preview").cloned().unwrap_or(body))
}
