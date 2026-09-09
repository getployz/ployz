//! Which platforms a Deploy's Railpack Builds must produce, read from the
//! Machines each Service may be placed on.

use std::collections::BTreeMap;

use ployz_core::{
    Machine, MachineObservation, MembershipObservation, RequestedServiceSpec,
    ServicePlacementEligibility,
};

use super::{CapturedBuild, ComposeError, invalid_build};
use crate::compose::CapturedCompose;

/// The only platforms the pinned Railpack frontend produces.
pub(super) const RAILPACK_PLATFORMS: [&str; 2] = ["linux/amd64", "linux/arm64"];

impl CapturedBuild {
    /// Fix each Railpack target's platforms to what the Machines its Service
    /// may be placed on run natively, from read-only observations. Authored
    /// `build.platforms` must already cover them; the execution host's default
    /// platform is replaced, since it describes the client and not the Cluster.
    /// Dockerfile targets keep one platform; Deploy's coverage check refuses a
    /// mismatch before any change.
    ///
    /// # Errors
    /// Names the Service whose explicit platforms miss a Machine's platform.
    pub fn cover_machines(
        &mut self,
        candidate: &CapturedCompose,
        machines: &[MachineObservation],
    ) -> Result<(), ComposeError> {
        let applied = candidate.intent().applied_names();
        for target in &mut self.targets {
            if !self
                .railpack
                .iter()
                .any(|recipe| recipe.name == target.name)
            {
                continue;
            }
            // A build-only dependency (profile-gated Service context) is never
            // placed, so nothing constrains its platform.
            let Some(spec) = candidate
                .intent()
                .target
                .iter()
                .find(|spec| spec.name.as_str() == target.name && applied.contains(&spec.name))
            else {
                continue;
            };
            let required = machine_platforms(
                &target.name,
                &candidate.intent().project_name,
                spec,
                machines,
            )?;
            if required.is_empty() {
                // No placement is visible; the coverage check after the Build
                // decides, and nothing here can name a better platform.
                continue;
            }
            if !self.authored_platforms.contains(&target.name) {
                target.platforms = required.into_keys().collect();
            } else if let Some((platform, machines)) = required
                .iter()
                .find(|(platform, _)| !target.platforms.contains(platform))
            {
                return Err(invalid_build(&format!(
                    "service '{}' builds for {} but {} runs {platform}; add it to build.platforms",
                    target.name,
                    target.platforms.join(", "),
                    machines
                        .iter()
                        .map(|machine| named(machine))
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        Ok(())
    }
}

/// Railpack platforms the Machines a Service may be placed on run natively,
/// each with the Machines that need it.
///
/// # Errors
/// Names a Machine the Service may land on that no Railpack platform runs:
/// no rerun can cover it, so the Build is refused before compilation.
fn machine_platforms<'observed>(
    service: &str,
    project: &ployz_core::ProjectName,
    spec: &RequestedServiceSpec,
    machines: &'observed [MachineObservation],
) -> Result<BTreeMap<String, Vec<&'observed Machine>>, ComposeError> {
    let mut required = BTreeMap::<String, Vec<&Machine>>::new();
    for machine in machines
        .iter()
        .filter(|machine| machine.membership != MembershipObservation::Down)
        .filter(|machine| {
            !matches!(
                spec.placement_eligibility_in_project(
                    project,
                    &machine.machine,
                    machine.storage.as_ref()
                ),
                ServicePlacementEligibility::Ineligible(_)
            )
        })
    {
        let architecture = &machine.machine.runtime.architecture;
        let Some(platform) = RAILPACK_PLATFORMS
            .iter()
            .find(|platform| crate::image::platform_compatible(platform, architecture))
        else {
            return Err(invalid_build(&format!(
                "service '{service}' may run on {}, which reports architecture {architecture:?}; Railpack builds only {}. Use deploy.placement.constraints to select Machines it builds for",
                named(&machine.machine),
                RAILPACK_PLATFORMS.join(" and ")
            )));
        };
        required
            .entry((*platform).to_owned())
            .or_default()
            .push(&machine.machine);
    }
    Ok(required)
}

fn named(machine: &Machine) -> String {
    format!("{} ({})", machine.name, machine.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{BuildOptions, capture_build, plan_build};

    #[test]
    fn placement_machines_fix_railpack_platforms_and_explicit_ones_must_cover_them() {
        use ployz_core::{Machine, MachineId, MachineName, WireGuardPublicKey};

        let root =
            std::env::temp_dir().join(format!("ployz-cover-machines-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
        let observed = |seed: u8, architecture: &str, membership| {
            MachineObservation::new(
                Machine {
                    // Letters keep the ID a YAML string inside placement constraints.
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
                },
                membership,
            )
        };
        let machines = [
            observed(1, "x86_64", MembershipObservation::Up),
            observed(2, "aarch64", MembershipObservation::Suspect),
            // A Down Machine is never a placement, so it derives nothing.
            observed(3, "aarch64", MembershipObservation::Down),
        ];
        let capture = |compose: &str| {
            let mut project = crate::compose::parse_normalized(compose, &root).unwrap();
            // The client's default platform describes this host, not the Cluster.
            project
                .environment
                .insert("DOCKER_DEFAULT_PLATFORM".into(), "linux/amd64".into());
            let options = BuildOptions::default();
            let plan = plan_build(&project, &options).unwrap();
            let captured = capture_build(&plan, &options, &mut project).unwrap();
            let candidate = project.capture(
                ployz_core::ProjectName::parse("example").unwrap(),
                ployz_core::PlanOptions::default(),
                Vec::new(),
                None,
                Vec::new(),
            );
            (captured, candidate)
        };

        let (mut captured, candidate) = capture(&format!(
            "services:\n  pinned:\n    image: registry.invalid/pinned:1\n    build: {{context: ., x-recipe: railpack}}\n    deploy: {{placement: {{constraints: [node.id=={}]}}}}\n  anywhere:\n    image: registry.invalid/anywhere:1\n    build: {{context: ., x-recipe: railpack}}\n  file:\n    image: registry.invalid/file:1\n    build: .\n",
            machines[0].machine.id
        ));
        // Before derivation every target carries the host default.
        assert!(
            captured
                .targets
                .iter()
                .all(|target| target.platforms == ["linux/amd64"])
        );
        captured.cover_machines(&candidate, &machines).unwrap();
        let platforms = |captured: &CapturedBuild, name: &str| {
            captured
                .targets
                .iter()
                .find(|target| target.name == name)
                .unwrap()
                .platforms
                .clone()
        };
        assert_eq!(platforms(&captured, "pinned"), ["linux/amd64"]);
        assert_eq!(
            platforms(&captured, "anywhere"),
            ["linux/amd64", "linux/arm64"]
        );
        let definition = ployz_build::remote::Definition {
            targets: captured.targets.clone(),
            retained_tags: Vec::new(),
            image_contexts: Default::default(),
            output: ployz_build::Output::Load,
            no_cache: false,
            pull: false,
        };
        ployz_build::remote::validate_capture(captured.inputs.root(), &definition).unwrap();
        // A Dockerfile keeps the host default; Deploy's coverage check decides.
        assert_eq!(platforms(&captured, "file"), ["linux/amd64"]);
        assert!(captured.cover_machines(&candidate, &[]).is_ok());
        assert_eq!(
            platforms(&captured, "anywhere"),
            ["linux/amd64", "linux/arm64"]
        );

        let (mut captured, candidate) = capture(
            "services:\n  explicit:\n    image: registry.invalid/explicit:1\n    build: {context: ., x-recipe: railpack, platforms: [linux/amd64]}\n",
        );
        let error = captured
            .cover_machines(&candidate, &machines)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("'explicit'")
                && error.contains("linux/arm64")
                && error.contains("machine-2")
                && !error.contains("machine-3"),
            "{error}"
        );
        assert!(captured.cover_machines(&candidate, &machines[..1]).is_ok());

        let (mut dependency, dependency_candidate) = capture(
            "services:\n  app:\n    build: {context: ., additional_contexts: {base: 'service:base'}}\n  base:\n    profiles: [build-only]\n    build: {context: ., x-recipe: railpack, platforms: [linux/amd64]}\n",
        );
        dependency
            .cover_machines(&dependency_candidate, &machines)
            .unwrap();
        assert_eq!(platforms(&dependency, "base"), ["linux/amd64"]);

        let (mut volume_build, volume_candidate) = capture(
            "services:\n  app:\n    build: {context: ., x-recipe: railpack, platforms: [linux/amd64]}\n    volumes: [data:/data]\nx-volumes: {data: 1G}\n",
        );
        let mut storage_machines = machines.clone();
        storage_machines[0].storage = Some(ployz_core::MachineStorageObservation::Ready);
        storage_machines[1].storage = Some(ployz_core::MachineStorageObservation::Stateless);
        volume_build
            .cover_machines(&volume_candidate, &storage_machines)
            .unwrap();
        storage_machines[1].storage = None;
        assert!(
            volume_build
                .cover_machines(&volume_candidate, &storage_machines)
                .is_err()
        );

        // A possible placement Railpack cannot build for is refused before
        // compilation: no rerun could cover it.
        let mut unbuildable = [
            machines[0].clone(),
            observed(4, "riscv64", MembershipObservation::Up),
        ];
        let error = captured
            .cover_machines(&candidate, &unbuildable)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("machine-4")
                && error.contains("riscv64")
                && error.contains("deploy.placement.constraints"),
            "{error}"
        );
        // A Build-only Machine cannot place the Service, regardless of CPU.
        unbuildable[1].machine.accepts_services = false;
        captured.cover_machines(&candidate, &unbuildable).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
