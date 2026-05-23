use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{DaemonJsonPayload, ImagePresence, parse_daemon_json_response};

const SOURCE_IMAGE: &str = "ployz-e2e-preload/http-smoke:latest";

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

    run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --plain image push {SOURCE_IMAGE} --to peer"),
    )?;

    let status = run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --json image status --digest {digest} --machine peer"),
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
        .find(|record| record.machine_id == "peer" && record.digest == digest)
    else {
        return Err(Error::Message(format!(
            "image status did not include peer availability for {digest}"
        )));
    };
    match &record.presence {
        ImagePresence::Present { .. } => Ok(()),
        ImagePresence::Failed { reason } => Err(Error::Message(format!(
            "peer image availability for {digest} failed: {reason}"
        ))),
        ImagePresence::Absent {} | ImagePresence::Transferring {} => Err(Error::Message(format!(
            "peer image availability for {digest} was not present"
        ))),
    }
}
