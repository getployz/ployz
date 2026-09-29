use clap::ArgMatches;
use ployz_core::{ProjectName, derive_projects};

use crate::{
    deploy::{DeploySnapshot, VolumeFate, remove_project},
    project::refuse_reserved,
};

use super::{Error, data_loss, leaf_matches, required, with_client};
use crate::output::{self, Gaps, say};

pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    with_client(root, |client| {
        Box::pin(async move {
            let machines = client.machines().await?;
            let snapshot = client.deploy_snapshot(machines).await?;
            for line in observer_listing_warnings(&snapshot) {
                eprintln!("{line}");
            }
            let projects = derive_projects(
                &snapshot.containers,
                snapshot
                    .volume_snapshot
                    .observations()
                    .iter()
                    .map(|volume| (&volume.id, &volume.labels)),
            );
            let mut gaps = Gaps::default();
            gaps.extend(&snapshot.container_failures, &snapshot.container_omissions);
            let volumes = &snapshot.volume_snapshot;
            gaps.extend(volumes.machine_failures(), volumes.omissions());
            gaps.unavailable_volumes = volumes.named_failures().to_vec();
            output::finish_fanout("projects", &projects, gaps, || {
                say!("PROJECT\tSERVICES\tVOLUMES");
                for project in &projects {
                    say!(
                        "{}\t{}\t{}",
                        project.name,
                        project.services.len(),
                        project.volumes.len()
                    );
                }
            })
        })
    })
}

pub(super) fn remove(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let name = ProjectName::parse(required(matches, "project")?)?;
    refuse_reserved(&name)?;
    let volumes = if matches.get_flag("volumes") {
        VolumeFate::Destroy
    } else {
        VolumeFate::Preserve
    };
    let command = root.clone();
    with_client(root, move |client| {
        Box::pin(async move {
            let observed = client
                .data_loss_if_project_destroyed(&name, volumes)
                .await?;
            let Some(confirmation) = data_loss::confirm_removal(
                &command,
                client,
                &observed,
                "Remove Project",
                &[name.to_string()],
                if volumes == VolumeFate::Destroy {
                    data_loss::VolumeEffect::Delete
                } else {
                    data_loss::VolumeEffect::Preserve
                },
            )?
            else {
                return Ok(());
            };
            let context = match client.connection_source() {
                crate::context::ConnectionSource::Context(name) => name.clone(),
                crate::context::ConnectionSource::Direct => "direct connection".into(),
                crate::context::ConnectionSource::LocalSocket => "local socket".into(),
            };
            let outcome = remove_project(client, &name, volumes, &context, &confirmation).await?;
            crate::deploy::emit_outcome(&outcome)
        })
    })
}

fn observer_listing_warnings(snapshot: &DeploySnapshot) -> Vec<String> {
    let mut lines =
        vec!["WARNING: Live Observation is observer-relative and not globally complete".into()];
    lines.extend(snapshot.container_failures.iter().map(|failure| {
        format!(
            "WARNING: Machine {} failed: {}",
            failure.machine_id, failure.error.message
        )
    }));
    lines.extend(
        snapshot
            .container_omissions
            .iter()
            .map(|machine_id| format!("WARNING: Machine {machine_id} was omitted")),
    );
    lines.extend(snapshot.volume_snapshot.listing_warnings());
    lines
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        DockerVolumeId, DockerVolumeName, MachineFailure, MachineId, RpcError, RpcErrorCode,
        VolumeObservationFailure,
    };

    use super::*;

    #[test]
    fn listing_warnings_are_observer_relative_and_include_volume_gaps() {
        let machine = MachineId::parse("1".repeat(32)).unwrap();
        let omitted = MachineId::parse("2".repeat(32)).unwrap();
        let snapshot = DeploySnapshot {
            container_failures: vec![MachineFailure {
                machine_id: machine,
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "down".into(),
                    details: serde_json::Value::Null,
                },
            }],
            volume_snapshot: crate::deploy::VolumeSnapshot::try_from_parts(
                Vec::new(),
                vec![VolumeObservationFailure {
                    id: DockerVolumeId {
                        machine_id: machine,
                        name: DockerVolumeName::parse("data").unwrap(),
                    },
                    error: RpcError {
                        code: RpcErrorCode::Unavailable,
                        message: "inspect failed".into(),
                        details: serde_json::Value::Null,
                    },
                }],
                Vec::new(),
                vec![omitted],
            )
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        };
        let warnings = observer_listing_warnings(&snapshot);
        assert_eq!(
            warnings.get(..3).unwrap(),
            &[
                "WARNING: Live Observation is observer-relative and not globally complete".into(),
                format!("WARNING: Machine {machine} failed: down"),
                format!("WARNING: Machine {omitted} was omitted listing volumes"),
            ]
        );
        let listing = warnings.last().unwrap();
        let (_, planning) = snapshot.volume_snapshot.named_gap(|_| true).unwrap();
        let deploy = snapshot
            .volume_snapshot
            .deploy_warnings()
            .find_map(|warning| {
                if let crate::deploy::DeployWarning::ObservationFailed { message, .. } = warning {
                    Some(message)
                } else {
                    None
                }
            })
            .unwrap();
        for message in [listing, &planning, &deploy] {
            for hint in [
                "data",
                &machine.to_string(),
                "inspect failed",
                "inspect the Volume again",
            ] {
                assert!(message.contains(hint), "{message}");
            }
        }
    }
}
