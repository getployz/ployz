use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{DaemonJsonPayload, ImagePresence, parse_daemon_json_response};

const BASE_IMAGE: &str = "ployz-e2e-preload/http-smoke:latest";
const BUILT_IMAGE: &str = "ployz-e2e-local-build:http";
const CONTEXT_DIR: &str = "/tmp/ployz-local-build-context";
const MANIFEST_PATH: &str = "/tmp/ployz-local-build-never.json";
const NAMESPACE: &str = "local-build";
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

    write_build_context(run)?;
    let build = run.ssh_expect_ok_name(
        "founder",
        &format!(
            "ployzd --json build local --method dockerfile --image {BUILT_IMAGE} {CONTEXT_DIR}"
        ),
    )?;
    let response = parse_daemon_json_response(&build.stdout)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "build local failed [{}]: {}",
            response.code, response.message
        )));
    }
    let Some(DaemonJsonPayload::BuildResult(payload)) = response.payload else {
        return Err(Error::Message(
            "build local response did not include build-result payload".into(),
        ));
    };
    let operation_id = payload.operation_id;
    let digest = payload.artifact.image.digest;
    if digest.is_empty() {
        return Err(Error::Message(
            "build local response did not include an image digest".into(),
        ));
    }
    assert_founder_build_availability(run, &digest, &operation_id)?;

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
        &format!("ployzd --plain image push {BUILT_IMAGE} --to peer --expected-digest {digest}"),
    )?;
    run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --json deploy -f {MANIFEST_PATH}"),
    )?;
    run.wait_service_container_name("peer", NAMESPACE, SERVICE)
}

fn assert_founder_build_availability(
    run: &ScenarioRun,
    digest: &str,
    operation_id: &str,
) -> Result<()> {
    let status = run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --json image status --digest {digest} --machine founder"),
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
        .find(|record| record.machine_id == "founder" && record.digest == digest)
    else {
        return Err(Error::Message(format!(
            "image status did not include founder availability for {digest}"
        )));
    };
    match &record.presence {
        ImagePresence::Present {
            source_operation_id,
        } if source_operation_id.as_deref() == Some(operation_id) => Ok(()),
        ImagePresence::Present {
            source_operation_id,
        } => Err(Error::Message(format!(
            "founder image availability for {digest} came from {:?}, expected {operation_id}",
            source_operation_id
        ))),
        ImagePresence::Failed { reason } => Err(Error::Message(format!(
            "founder image availability for {digest} failed: {reason}"
        ))),
        ImagePresence::Absent {} | ImagePresence::Transferring {} => Err(Error::Message(format!(
            "founder image availability for {digest} was not present"
        ))),
    }
}

fn write_build_context(run: &ScenarioRun) -> Result<()> {
    let dockerfile = format!(
        r#"FROM {BASE_IMAGE}
RUN mkdir -p /www && printf 'ployz local build\n' >/www/index.html
CMD ["sh", "-c", "httpd -f -p 80 -h /www"]
"#
    );
    run.ssh_expect_ok_name(
        "founder",
        &format!(
            "rm -rf {CONTEXT_DIR} && mkdir -p {CONTEXT_DIR} && cat >{CONTEXT_DIR}/Dockerfile <<'EOF'\n{dockerfile}EOF"
        ),
    )?;
    Ok(())
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
        "pull_policy": "never"
      }},
      "network": "overlay"
    }}
  ]
}}"#
    );
    run.ssh_expect_ok_name(
        "founder",
        &format!("cat >{MANIFEST_PATH} <<'EOF'\n{manifest}\nEOF"),
    )?;
    Ok(())
}

fn assert_missing_image_preflight_fails(run: &ScenarioRun, digest: &str) -> Result<()> {
    let output = run.ssh_run_name(
        "founder",
        &format!("ployzd --json deploy -f {MANIFEST_PATH}"),
    )?;
    if output.status.success() {
        return Err(Error::Message(
            "deploy unexpectedly succeeded before built image availability was recorded on peer"
                .into(),
        ));
    }
    let response = parse_daemon_json_response(&output.stdout)?;
    if response.ok {
        return Err(Error::Message(
            "deploy returned ok=true before built image availability was recorded on peer".into(),
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
