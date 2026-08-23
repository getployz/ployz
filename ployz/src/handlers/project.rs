use clap::ArgMatches;
use ployz_core::{ProjectName, derive_projects};

use crate::{
    deploy::{DeploySnapshot, VolumeFate, remove_project},
    project::refuse_reserved,
};

use super::{Error, data_loss, leaf_matches, required, string_values, with_client};

pub(super) fn list(root: &ArgMatches) -> Result<(), Error> {
    let json = leaf_matches(root)
        .get_one::<String>("output")
        .map(String::as_str)
        == Some("json");
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
                    .volumes
                    .iter()
                    .map(|volume| (&volume.id, &volume.labels)),
            );
            if json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else {
                println!("PROJECT\tSERVICES\tVOLUMES");
                for project in projects {
                    println!(
                        "{}\t{}\t{}",
                        project.name,
                        project.services.len(),
                        project.volumes.len()
                    );
                }
            }
            Ok(())
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
    let yes = matches.get_flag("yes");
    let named = string_values(matches, "data-loss");
    let context = matches
        .get_one::<String>("context")
        .cloned()
        .unwrap_or_else(|| "default".into());
    with_client(root, move |client| {
        Box::pin(async move {
            let observed = client
                .data_loss_if_project_destroyed(&name, volumes)
                .await?;
            let confirmation = data_loss::collect_data_loss_confirmation(&observed, &named)?;
            remove_project(client, &name, volumes, yes, &context, &confirmation).await
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
    lines.extend(snapshot.volume_failures.iter().map(|failure| {
        format!(
            "WARNING: Machine {} failed listing volumes: {}",
            failure.machine_id, failure.error.message
        )
    }));
    lines.extend(
        snapshot
            .volume_omissions
            .iter()
            .map(|machine_id| format!("WARNING: Machine {machine_id} was omitted listing volumes")),
    );
    lines
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineFailure, MachineId, RpcError, RpcErrorCode};

    use super::*;

    #[test]
    fn listing_warnings_are_observer_relative_and_include_volume_gaps() {
        let machine = MachineId::parse("1".repeat(32)).unwrap();
        let snapshot = DeploySnapshot {
            container_failures: vec![MachineFailure {
                machine_id: machine,
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "down".into(),
                    details: serde_json::Value::Null,
                },
            }],
            volume_omissions: vec![machine],
            ..Default::default()
        };
        assert_eq!(
            observer_listing_warnings(&snapshot),
            [
                "WARNING: Live Observation is observer-relative and not globally complete".into(),
                format!("WARNING: Machine {machine} failed: down"),
                format!("WARNING: Machine {machine} was omitted listing volumes"),
            ]
        );
    }
}
