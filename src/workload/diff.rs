use super::spec::{PullPolicy, ServiceSpec, VolumeMount, VolumeSource};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpecDiff {
    pub recreate: Vec<String>,
    pub update: Vec<String>,
    pub driver_update: Vec<String>,
}

impl SpecDiff {
    pub fn is_up_to_date(&self) -> bool {
        self.recreate.is_empty() && self.update.is_empty() && self.driver_update.is_empty()
    }

    pub fn needs_recreate(&self) -> bool {
        !self.recreate.is_empty()
    }
}

pub fn eval_spec_change(desired: &ServiceSpec, running: &ServiceSpec) -> SpecDiff {
    let mut diff = SpecDiff::default();
    let d = &desired.container;
    let r = &running.container;

    // Immutable: baked at container creation -> recreate
    if d.image != r.image {
        diff.recreate
            .push(format!("image: {} -> {}", r.image, d.image));
    }
    if d.command != r.command {
        diff.recreate.push("command changed".into());
    }
    if d.entrypoint != r.entrypoint {
        diff.recreate.push("entrypoint changed".into());
    }
    if d.env != r.env {
        diff.recreate.push("env changed".into());
    }
    if d.cap_add != r.cap_add {
        diff.recreate.push("cap_add changed".into());
    }
    if d.cap_drop != r.cap_drop {
        diff.recreate.push("cap_drop changed".into());
    }
    if d.privileged != r.privileged {
        diff.recreate.push("privileged changed".into());
    }
    if d.user != r.user {
        diff.recreate.push("user changed".into());
    }
    if d.sysctls != r.sysctls {
        diff.recreate.push("sysctls changed".into());
    }
    if d.pull_policy == PullPolicy::Always {
        diff.recreate.push("pull policy is always".into());
    }
    if desired.network != running.network {
        diff.recreate.push("network mode changed".into());
    }
    if desired.ports != running.ports {
        diff.recreate.push("ports changed".into());
    }

    // Volumes: type-aware diffing
    diff_volumes(&d.volumes, &r.volumes, &mut diff);

    // Mutable: Docker update API
    if d.resources != r.resources {
        diff.update.push("resources changed".into());
    }
    if desired.restart != running.restart {
        diff.update.push("restart policy changed".into());
    }

    diff
}

fn diff_volumes(desired: &[VolumeMount], running: &[VolumeMount], diff: &mut SpecDiff) {
    if desired.len() != running.len() {
        diff.recreate.push("volume count changed".into());
        return;
    }
    for (d, r) in desired.iter().zip(running.iter()) {
        if d.target != r.target {
            diff.recreate
                .push(format!("volume target changed: {}", d.target));
            continue;
        }
        if d.readonly != r.readonly {
            diff.recreate
                .push(format!("volume readonly changed: {}", d.target));
            continue;
        }
        match (&d.source, &r.source) {
            (VolumeSource::Bind(_), VolumeSource::Managed(_))
            | (VolumeSource::Managed(_), VolumeSource::Bind(_))
            | (VolumeSource::Tmpfs, _)
            | (_, VolumeSource::Tmpfs) => {
                diff.recreate
                    .push(format!("volume type changed: {}", d.target));
            }
            (VolumeSource::Bind(dp), VolumeSource::Bind(rp)) if dp != rp => {
                diff.recreate
                    .push(format!("bind source changed: {}", d.target));
            }
            (VolumeSource::Managed(dm), VolumeSource::Managed(rm)) if dm != rm => {
                if dm.name != rm.name || dm.driver != rm.driver {
                    diff.recreate
                        .push(format!("managed volume identity changed: {}", d.target));
                } else {
                    diff.driver_update
                        .push(format!("volume options changed: {}", d.target));
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::spec::*;
    use std::collections::BTreeMap;

    fn base_spec() -> ServiceSpec {
        ServiceSpec {
            name: "api".into(),
            namespace: Namespace::default_ns(),
            schedule: Schedule::Imperative,
            container: ContainerSpec {
                image: "myapp:v1".into(),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                volumes: vec![],
                cap_add: vec![],
                cap_drop: vec![],
                privileged: false,
                user: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::default(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            ports: vec![],
            labels: BTreeMap::new(),
            stop_grace_period: None,
            restart: RestartPolicy::UnlessStopped,
        }
    }

    #[test]
    fn identical_specs_are_up_to_date() {
        let spec = base_spec();
        let diff = eval_spec_change(&spec, &spec);
        assert!(diff.is_up_to_date());
        assert!(!diff.needs_recreate());
    }

    #[test]
    fn image_change_requires_recreate() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.container.image = "myapp:v2".into();
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
        assert_eq!(diff.recreate.len(), 1);
        assert!(diff.recreate[0].contains("image"));
    }

    #[test]
    fn env_change_requires_recreate() {
        let mut desired = base_spec();
        let running = base_spec();
        desired
            .container
            .env
            .insert("NEW_VAR".into(), "value".into());
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
    }

    #[test]
    fn resources_change_is_update() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.container.resources.cpu_millicores = Some(2000);
        let diff = eval_spec_change(&desired, &running);
        assert!(!diff.needs_recreate());
        assert!(!diff.is_up_to_date());
        assert_eq!(diff.update.len(), 1);
        assert!(diff.update[0].contains("resources"));
    }

    #[test]
    fn restart_policy_change_is_update() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.restart = RestartPolicy::Always;
        let diff = eval_spec_change(&desired, &running);
        assert!(!diff.needs_recreate());
        assert_eq!(diff.update.len(), 1);
    }

    #[test]
    fn volume_count_change_requires_recreate() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.container.volumes.push(VolumeMount {
            source: VolumeSource::Bind("/host/path".into()),
            target: "/data".into(),
            readonly: false,
        });
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
    }

    #[test]
    fn volume_type_change_requires_recreate() {
        let mut desired = base_spec();
        let mut running = base_spec();
        let bind = VolumeMount {
            source: VolumeSource::Bind("/host".into()),
            target: "/data".into(),
            readonly: false,
        };
        let managed = VolumeMount {
            source: VolumeSource::Managed(ManagedVolumeSpec {
                name: "data".into(),
                driver: None,
                options: BTreeMap::new(),
            }),
            target: "/data".into(),
            readonly: false,
        };
        desired.container.volumes.push(managed);
        running.container.volumes.push(bind);
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
        assert!(diff.recreate[0].contains("volume type changed"));
    }

    #[test]
    fn managed_volume_options_change_is_driver_update() {
        let mut desired = base_spec();
        let mut running = base_spec();
        let vol = |quota: &str| VolumeMount {
            source: VolumeSource::Managed(ManagedVolumeSpec {
                name: "data".into(),
                driver: Some("zfs".into()),
                options: BTreeMap::from([("quota".into(), quota.into())]),
            }),
            target: "/data".into(),
            readonly: false,
        };
        desired.container.volumes.push(vol("20G"));
        running.container.volumes.push(vol("10G"));
        let diff = eval_spec_change(&desired, &running);
        assert!(!diff.needs_recreate());
        assert!(diff.is_up_to_date() == false);
        assert_eq!(diff.driver_update.len(), 1);
        assert!(diff.driver_update[0].contains("volume options changed"));
    }

    #[test]
    fn managed_volume_name_change_requires_recreate() {
        let mut desired = base_spec();
        let mut running = base_spec();
        let vol = |name: &str| VolumeMount {
            source: VolumeSource::Managed(ManagedVolumeSpec {
                name: name.into(),
                driver: None,
                options: BTreeMap::new(),
            }),
            target: "/data".into(),
            readonly: false,
        };
        desired.container.volumes.push(vol("new-data"));
        running.container.volumes.push(vol("old-data"));
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
        assert!(diff.recreate[0].contains("managed volume identity"));
    }

    #[test]
    fn bind_path_change_requires_recreate() {
        let mut desired = base_spec();
        let mut running = base_spec();
        let vol = |path: &str| VolumeMount {
            source: VolumeSource::Bind(path.into()),
            target: "/data".into(),
            readonly: false,
        };
        desired.container.volumes.push(vol("/new/path"));
        running.container.volumes.push(vol("/old/path"));
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
    }

    #[test]
    fn pull_always_forces_recreate() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.container.pull_policy = PullPolicy::Always;
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
    }

    #[test]
    fn network_mode_change_requires_recreate() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.network = NetworkMode::Host;
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
    }

    #[test]
    fn multiple_buckets_simultaneously() {
        let mut desired = base_spec();
        let running = base_spec();
        desired.container.image = "myapp:v2".into();
        desired.container.resources.memory_bytes = Some(1024);
        let diff = eval_spec_change(&desired, &running);
        assert!(diff.needs_recreate());
        assert!(!diff.update.is_empty());
    }
}
