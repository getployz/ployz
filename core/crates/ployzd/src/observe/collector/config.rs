use std::path::{Path, PathBuf};

use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountType};
use ployz_core::labels;
use url::Url;

pub(super) const NAME: &str = "ployz-collector";
pub(super) const HEALTH_ADDRESS: &str = "127.0.0.1:13133";
pub(super) const BASE: &str = include_str!("base.yaml");
pub(super) const METRICS: &str = include_str!("metrics.yaml");

const CONFIG_MOUNT: &str = "/etc/ployz/config";
const TOKEN_MOUNT: &str = "/etc/ployz/token";
const STATE_MOUNT: &str = "/var/lib/otelcol";
const LOGS_MOUNT: &str = "/var/lib/docker/containers";
const DOCKER_SOCKET_MOUNT: &str = "/var/run/docker.sock";
const HOST_PROC_MOUNT: &str = "/hostfs/proc";
const HOST_SYS_MOUNT: &str = "/hostfs/sys";
const MEMORY_BYTES: i64 = 256 << 20;
pub(super) const STOP_GRACE_SECONDS: i32 = 15;

pub(super) struct Paths {
    pub(super) config: PathBuf,
    pub(super) token: PathBuf,
    pub(super) state: PathBuf,
    pub(super) logs: PathBuf,
    pub(super) docker_socket: PathBuf,
    pub(super) proc: PathBuf,
    pub(super) sys: PathBuf,
}

impl Paths {
    pub(super) fn under(machine_root: &Path, data_dir: &Path, docker_root: &Path) -> Self {
        let run = machine_root.join("run/ployz/collector");
        Self {
            config: run.join("config"),
            token: run.join("token"),
            state: data_dir.join("collector"),
            logs: docker_root.join("containers"),
            docker_socket: machine_root.join("var/run/docker.sock"),
            proc: machine_root.join("proc"),
            sys: machine_root.join("sys"),
        }
    }

    pub(super) fn token_file(&self) -> PathBuf {
        self.token.join("token")
    }

    pub(super) fn base_file(&self) -> PathBuf {
        self.config.join("base.yaml")
    }

    pub(super) fn metrics_file(&self) -> PathBuf {
        self.config.join("metrics.yaml")
    }

    pub(super) fn mask_file(&self) -> PathBuf {
        self.config.join("mask.yaml")
    }
}

pub(super) fn endpoint(push_target: &Url) -> String {
    format!(
        "{}/api/machines/otlp",
        push_target.origin().ascii_serialization()
    )
}

/// `docker_gid` owns the Docker socket. The socket is root-equivalent: the Collector reads
/// container stats through it, and anyone who controls the Collector controls Docker.
pub(super) fn body(paths: &Paths, endpoint: &str, docker_gid: u32) -> ContainerCreateBody {
    let mount = |source: &Path, target: &str, read_only: bool| Mount {
        typ: Some(MountType::BIND),
        source: Some(source.to_string_lossy().into_owned()),
        target: Some(target.to_owned()),
        read_only: Some(read_only),
        ..Default::default()
    };
    ContainerCreateBody {
        cmd: Some(vec![
            format!("--config={CONFIG_MOUNT}/base.yaml"),
            format!("--config={CONFIG_MOUNT}/metrics.yaml"),
            format!("--config={CONFIG_MOUNT}/mask.yaml"),
        ]),
        env: Some(vec![
            "GOMAXPROCS=1".into(),
            "GOMEMLIMIT=160MiB".into(),
            format!("PLOYZ_OTLP_ENDPOINT={endpoint}"),
        ]),
        labels: Some(labels::system_labels()),
        host_config: Some(HostConfig {
            network_mode: Some("host".into()),
            log_config: Some(crate::docker::container_log_config()),
            mounts: Some(vec![
                mount(&paths.config, CONFIG_MOUNT, true),
                mount(&paths.token, TOKEN_MOUNT, true),
                mount(&paths.state, STATE_MOUNT, false),
                mount(&paths.logs, LOGS_MOUNT, true),
                mount(&paths.docker_socket, DOCKER_SOCKET_MOUNT, true),
                mount(&paths.proc, HOST_PROC_MOUNT, true),
                mount(&paths.sys, HOST_SYS_MOUNT, true),
            ]),
            group_add: Some(vec![docker_gid.to_string()]),
            memory: Some(MEMORY_BYTES),
            memory_swap: Some(MEMORY_BYTES),
            readonly_rootfs: Some(true),
            cap_drop: Some(vec!["ALL".into()]),
            security_opt: Some(vec!["no-new-privileges".into()]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::scrub::SealedValues;
    use serde_norway::{Mapping, Value};

    use super::{BASE, METRICS};

    /// The Collector merges its `--config` files key by key, as here.
    fn merge(into: &mut Value, from: Value) {
        match (into, from) {
            (Value::Mapping(into), Value::Mapping(from)) => {
                for (key, value) in from {
                    match into.get_mut(&key) {
                        Some(existing) => merge(existing, value),
                        None => {
                            into.insert(key, value);
                        }
                    }
                }
            }
            (into, from) => *into = from,
        }
    }

    #[test]
    fn every_pipeline_names_components_the_config_files_define() {
        let mut config = Value::Mapping(Mapping::new());
        let mask = super::super::mask::fragment(&SealedValues::none());
        for file in [BASE, METRICS, mask.as_str()] {
            merge(&mut config, serde_norway::from_str(file).unwrap());
        }
        let pipelines = config["service"]["pipelines"].as_mapping().unwrap();
        assert!(pipelines.contains_key("metrics") && pipelines.contains_key("logs"));
        for (name, pipeline) in pipelines {
            for kind in ["receivers", "processors", "exporters"] {
                for component in pipeline[kind].as_sequence().unwrap() {
                    assert!(
                        config[kind].get(component).is_some(),
                        "pipeline {name:?} names {component:?}, which no file defines under {kind}"
                    );
                }
            }
        }
    }
}
