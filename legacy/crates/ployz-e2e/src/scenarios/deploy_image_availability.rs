use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{DaemonJsonPayload, ImagePresence, parse_daemon_json_response};

const SOURCE_IMAGE: &str = "ployz-e2e-preload/http-smoke:latest";
const NAMESPACE: &str = "image-preflight";
const SERVICE: &str = "web";

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "peer")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.wait_mesh_ready_name("peer")?;

    let digest = run
        .ssh_expect_ok_name(
            "founder",
            &format!("docker image inspect --format '{{{{.Id}}}}' {SOURCE_IMAGE}"),
        )?
        .stdout
        .trim()
        .to_string();
    if digest.is_empty() {
        return Err(Error::Message(format!(
            "source image '{SOURCE_IMAGE}' did not expose a docker image id"
        )));
    }

    run.ssh_expect_ok_name("founder", "ployzd machine drain founder")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "draining",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;

    write_manifest(run, &digest)?;
    assert_missing_image_preflight_fails(run, &digest)?;

    run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --plain image distribute --digest {digest} --from founder --to founder --to peer"),
    )?;
    assert_image_present_on(run, &digest, "founder")?;
    assert_image_present_on(run, &digest, "peer")?;
    run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy -f /tmp/ployz-image-never.json",
    )?;
    run.wait_service_container_name("peer", NAMESPACE, SERVICE)
}

fn write_manifest(run: &ScenarioRun, digest: &str) -> Result<()> {
    let manifest = format!(
        r#"{{
  "namespace": "{NAMESPACE}",
  "services": [
    {{
      "name": "{SERVICE}",
      "placement": {{"replicated": {{"count": 1}}}},
      "template": {{
        "image": "{digest}",
        "command": ["sh", "-c", "mkdir -p /www && printf 'ployz image preflight\\n' >/www/index.html && httpd -f -p 80 -h /www"],
        "pull_policy": "never"
      }},
      "network": "overlay"
    }}
  ]
}}"#
    );
    run.ssh_expect_ok_name(
        "founder",
        &format!("cat >/tmp/ployz-image-never.json <<'EOF'\n{manifest}\nEOF"),
    )?;
    Ok(())
}

fn assert_missing_image_preflight_fails(run: &ScenarioRun, digest: &str) -> Result<()> {
    let output = run.ssh_run_name(
        "founder",
        "ployzd --json deploy -f /tmp/ployz-image-never.json",
    )?;
    if output.status.success() {
        return Err(Error::Message(
            "deploy unexpectedly succeeded before image availability was recorded on peer".into(),
        ));
    }
    let response = parse_daemon_json_response(&output.stdout)?;
    if response.ok {
        return Err(Error::Message(
            "deploy returned ok=true before image availability was recorded on peer".into(),
        ));
    }
    if response.code != "DEPLOY_IMAGE_AVAILABILITY_MISSING" {
        return Err(Error::Message(format!(
            "deploy failure used code {}, expected DEPLOY_IMAGE_AVAILABILITY_MISSING",
            response.code
        )));
    }
    if !response.message.contains(digest) || !response.message.contains("peer") {
        return Err(Error::Message(format!(
            "deploy failure did not identify missing peer image {digest}: {}",
            response.message
        )));
    }
    Ok(())
}

fn assert_image_present_on(run: &ScenarioRun, digest: &str, machine_id: &str) -> Result<()> {
    let status = run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --json image status --digest {digest} --machine {machine_id}"),
    )?;
    let response = parse_daemon_json_response(&status.stdout)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "image status failed [{}]: {}",
            response.code, response.message
        )));
    }
    let Some(DaemonJsonPayload::ImageStatus(payload)) = response.payload else {
        return Err(Error::Message(
            "image status response did not include image-status payload".into(),
        ));
    };
    let Some(record) = payload
        .records
        .iter()
        .find(|record| record.machine_id == machine_id && record.digest == digest)
    else {
        return Err(Error::Message(format!(
            "image status did not include {machine_id} availability for {digest}"
        )));
    };
    match &record.presence {
        ImagePresence::Present { .. } => Ok(()),
        ImagePresence::Failed { reason } => Err(Error::Message(format!(
            "{machine_id} image availability for {digest} failed: {reason}"
        ))),
        ImagePresence::Absent {} | ImagePresence::Transferring {} => Err(Error::Message(format!(
            "{machine_id} image availability for {digest} was not present"
        ))),
    }
}
