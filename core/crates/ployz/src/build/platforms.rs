//! Which platforms a Deploy's Railpack Builds must produce, read from the
//! Machines each Service may be placed on.

use std::collections::BTreeSet;

use ployz_core::{
    DeployIntent, Machine, MachineObservation, MembershipObservation, RequestedServiceSpec,
    ServicePlacementEligibility, config::BuildMethod,
};

use super::{CapturedBuild, Error, invalid};

/// The only platforms the pinned Railpack frontend produces.
const RAILPACK_PLATFORMS: [&str; 2] = ["linux/amd64", "linux/arm64"];

impl CapturedBuild {
    /// Fix each Railpack target's platforms to what the Machines its Service
    /// may be placed on run natively, from read-only observations. A Dockerfile
    /// Build keeps its native platform; Deploy's coverage check refuses a
    /// mismatch before any change.
    ///
    /// # Errors
    /// Names a Machine the Service may land on that Railpack cannot build for.
    pub fn cover_machines(
        &mut self,
        intent: &DeployIntent,
        machines: &[MachineObservation],
    ) -> Result<(), Error> {
        let applied = intent.applied_names();
        for captured in self
            .targets
            .iter_mut()
            .filter(|captured| captured.build_method == BuildMethod::Railpack)
        {
            let Some(spec) = intent
                .target
                .iter()
                .find(|spec| spec.name == captured.name && applied.contains(&spec.name))
            else {
                continue;
            };
            let required = machine_platforms(spec, &intent.project_name, machines)?;
            // With no visible placement, the coverage check after the Build decides.
            if !required.is_empty() {
                captured.target.platforms = required.into_iter().collect();
            }
        }
        Ok(())
    }
}

/// The platforms the Machines the Deploy's Services may be placed on run
/// natively, whatever their Build Method: what a Builder outside the Cluster
/// must produce for them.
///
/// # Errors
/// Names a Machine whose architecture neither build platform runs.
pub fn placement_platforms(
    intent: &DeployIntent,
    machines: &[MachineObservation],
) -> Result<BTreeSet<String>, Error> {
    let mut required = BTreeSet::new();
    for spec in &intent.target {
        required.extend(machine_platforms(spec, &intent.project_name, machines)?);
    }
    Ok(required)
}

/// Railpack platforms the Machines a Service may be placed on run natively.
///
/// # Errors
/// Names a Machine the Service may land on that no Railpack platform runs:
/// no rerun can cover it, so the Build is refused before compilation.
fn machine_platforms(
    spec: &RequestedServiceSpec,
    project: &ployz_core::ProjectName,
    machines: &[MachineObservation],
) -> Result<BTreeSet<String>, Error> {
    let mut required = BTreeSet::new();
    for machine in placeable(spec, project, machines) {
        let Machine {
            name, id, runtime, ..
        } = &machine.machine;
        let architecture = &runtime.architecture;
        let Some(platform) = RAILPACK_PLATFORMS
            .iter()
            .find(|platform| crate::image::platform_compatible(platform, architecture))
        else {
            return Err(invalid(format!(
                "service '{}' may run on {name} ({id}), which reports architecture {architecture:?}; Railpack builds only {}",
                spec.name,
                RAILPACK_PLATFORMS.join(" and ")
            )));
        };
        required.insert((*platform).to_owned());
    }
    Ok(required)
}

/// Machines the Service may be placed on: not Down and not ineligible.
pub(super) fn placeable<'observed>(
    spec: &'observed RequestedServiceSpec,
    project: &'observed ployz_core::ProjectName,
    machines: &'observed [MachineObservation],
) -> impl Iterator<Item = &'observed MachineObservation> {
    machines.iter().filter(|machine| {
        machine.membership != MembershipObservation::Down
            && !matches!(
                spec.placement_eligibility_in_project(
                    project,
                    &machine.machine,
                    machine.storage.as_ref()
                ),
                ServicePlacementEligibility::Ineligible(_)
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::{MachineId, MachineName, MachineStorageObservation, WireGuardPublicKey};

    fn observed(
        seed: u8,
        architecture: &str,
        membership: MembershipObservation,
    ) -> MachineObservation {
        MachineObservation::new(
            Machine {
                id: MachineId::parse(char::from(b'a' + seed).to_string().repeat(32)).unwrap(),
                name: MachineName::parse(format!("machine-{seed}")).unwrap(),
                subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                labels: Default::default(),
                accepts_services: true,
                accepts_builds: true,
                accepts_ingress: true,
                runtime: ployz_core::MachineRuntime {
                    architecture: architecture.into(),
                    ..Default::default()
                },
                build_concurrency: None,
            },
            membership,
        )
    }

    #[test]
    fn placement_platforms_are_what_the_services_machines_run() {
        let machines = [
            observed(1, "x86_64", MembershipObservation::Up),
            observed(2, "aarch64", MembershipObservation::Up),
            observed(3, "aarch64", MembershipObservation::Down),
        ];
        let mut pinned = super::super::tests::intent(&["pinned"]);
        pinned.target.get_mut(0).unwrap().placement.constraints =
            [ployz_core::PlacementConstraint::parse(format!(
                "node.id=={}",
                machines[0].machine.id
            ))
            .unwrap()]
            .into();
        assert_eq!(
            placement_platforms(&pinned, &machines).unwrap(),
            BTreeSet::from(["linux/amd64".to_owned()])
        );
        let anywhere = super::super::tests::intent(&["anywhere"]);
        assert_eq!(
            placement_platforms(&anywhere, &machines).unwrap(),
            BTreeSet::from(["linux/amd64".to_owned(), "linux/arm64".to_owned()])
        );
        // A placement no build platform runs refuses, naming the Machine.
        let unbuildable = [observed(4, "riscv64", MembershipObservation::Up)];
        let error = placement_platforms(&anywhere, &unbuildable)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("machine-4") && error.contains("riscv64"),
            "{error}"
        );
    }

    #[test]
    fn placement_machines_fix_railpack_platforms() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Dockerfile"), "FROM scratch\n").unwrap();
        let machines = [
            observed(1, "x86_64", MembershipObservation::Up),
            observed(2, "aarch64", MembershipObservation::Suspect),
            // A Down Machine is never a placement, so it derives nothing.
            observed(3, "aarch64", MembershipObservation::Down),
        ];
        let mut intent = super::super::tests::intent(&["pinned", "anywhere", "file"]);
        let pinned = intent.target.get_mut(0).unwrap();
        pinned.placement.constraints = [ployz_core::PlacementConstraint::parse(format!(
            "node.id=={}",
            machines[0].machine.id
        ))
        .unwrap()]
        .into();
        let railpack = || super::super::BuildSpec {
            context: root.path().to_owned(),
            recipe: super::super::Recipe::Railpack { command: None },
        };
        let specs = [
            ("pinned", railpack()),
            ("anywhere", railpack()),
            (
                "file",
                super::super::BuildSpec {
                    context: root.path().to_owned(),
                    recipe: super::super::Recipe::Dockerfile(root.path().join("Dockerfile")),
                },
            ),
        ]
        .map(|(name, spec)| (ployz_core::ServiceName::parse(name).unwrap(), spec));
        let mut captured = super::super::capture(&intent, specs.into()).unwrap();
        let platforms = |captured: &CapturedBuild, name: &str| {
            captured
                .targets()
                .find(|target| target.name == name)
                .unwrap()
                .platforms
                .clone()
        };
        assert!(captured.targets().all(|target| target.platforms.is_empty()));
        captured.cover_machines(&intent, &machines).unwrap();
        assert_eq!(platforms(&captured, "pinned"), ["linux/amd64"]);
        assert_eq!(
            platforms(&captured, "anywhere"),
            ["linux/amd64", "linux/arm64"]
        );
        // A Dockerfile keeps its native platform.
        assert!(platforms(&captured, "file").is_empty());
        for captured in &captured.targets {
            ployz_build::remote::validate_capture(
                captured.inputs.root(),
                &ployz_build::remote::Definition {
                    targets: vec![captured.target.clone()],
                    retained_tags: vec![captured.retained_tag.clone()],
                    image_contexts: Default::default(),
                    output: ployz_build::Output::Load,
                    no_cache: false,
                    pull: false,
                },
            )
            .unwrap();
        }
        // Without visible placements, earlier platforms stay for the coverage check.
        captured.cover_machines(&intent, &[]).unwrap();
        assert_eq!(
            platforms(&captured, "anywhere"),
            ["linux/amd64", "linux/arm64"]
        );

        // Provisioned storage limits placement to Machines that can hold it.
        let mut storage = machines.clone();
        storage[0].storage = Some(MachineStorageObservation::Ready);
        storage[1].storage = Some(MachineStorageObservation::Stateless);
        let mut volume_intent = intent.clone();
        let anywhere = volume_intent.target.get_mut(1).unwrap();
        anywhere.set_volume_graph(
            ployz_core::ServiceVolumeGraph::parse(
                vec![serde_json::from_value(serde_json::json!({
                    "reference": "data",
                    "source": {"kind": "provisioned", "name": "data", "maximum_bytes": 1_073_741_824}
                }))
                .unwrap()],
                vec![serde_json::from_value(serde_json::json!({"volume": "data", "target": "/data"})).unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
        captured.cover_machines(&volume_intent, &storage).unwrap();
        assert_eq!(platforms(&captured, "anywhere"), ["linux/amd64"]);
        // Unknown storage keeps the Machine a possible placement.
        storage[1].storage = None;
        captured.cover_machines(&volume_intent, &storage).unwrap();
        assert_eq!(
            platforms(&captured, "anywhere"),
            ["linux/amd64", "linux/arm64"]
        );

        // A possible placement Railpack cannot build for is refused before
        // compilation: no rerun could cover it.
        let mut unbuildable = [
            machines[0].clone(),
            observed(4, "riscv64", MembershipObservation::Up),
        ];
        let error = captured
            .cover_machines(&intent, &unbuildable)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("machine-4") && error.contains("riscv64"),
            "{error}"
        );
        // A Build-only Machine cannot place the Service, regardless of CPU.
        unbuildable[1].machine.accepts_services = false;
        captured.cover_machines(&intent, &unbuildable).unwrap();
    }
}
