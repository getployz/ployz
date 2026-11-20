use std::path::{Path, PathBuf};

use bollard::models::{ContainerCreateBody, HostConfig, Mount, MountType};
use ployz_core::labels;
use url::Url;

pub(super) const NAME: &str = "ployz-collector";
pub(super) const HEALTH_ADDRESS: &str = "127.0.0.1:13133";
pub(super) const BASE: &str = include_str!("base.yaml");

const CONFIG_MOUNT: &str = "/etc/ployz/config";
const TOKEN_MOUNT: &str = "/etc/ployz/token";
const STATE_MOUNT: &str = "/var/lib/otelcol";
const REQUESTS_MOUNT: &str = super::requests::REQUESTS_MOUNT;
const LOGS_MOUNT: &str = "/var/lib/docker/containers";
const MEMORY_BYTES: i64 = 256 << 20;
pub(super) const STOP_GRACE_SECONDS: i32 = 15;

pub(super) struct Paths {
    pub(super) config: PathBuf,
    pub(super) token: PathBuf,
    pub(super) state: PathBuf,
    pub(super) logs: PathBuf,
    pub(super) requests: PathBuf,
}

impl Paths {
    pub(super) fn under(machine_root: &Path, data_dir: &Path, docker_root: &Path) -> Self {
        let run_dir = machine_root.join("run/ployz");
        let run = run_dir.join("collector");
        let access_log = crate::ingress::access_log_path(&run_dir);
        Self {
            requests: access_log
                .parent()
                .map_or_else(|| run_dir.clone(), Path::to_path_buf),
            config: run.join("config"),
            token: run.join("token"),
            state: data_dir.join("collector"),
            logs: docker_root.join("containers"),
        }
    }

    pub(super) fn token_file(&self) -> PathBuf {
        self.token.join("token")
    }

    pub(super) fn base_file(&self) -> PathBuf {
        self.config.join("base.yaml")
    }

    pub(super) fn mask_file(&self) -> PathBuf {
        self.config.join("mask.yaml")
    }

    pub(super) fn requests_file(&self) -> PathBuf {
        self.config.join("requests.yaml")
    }
}

pub(super) fn endpoint(push_target: &Url) -> String {
    format!(
        "{}/api/machines/otlp",
        push_target.origin().ascii_serialization()
    )
}

pub(super) fn body(paths: &Paths, endpoint: &str) -> ContainerCreateBody {
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
            format!("--config={CONFIG_MOUNT}/mask.yaml"),
            format!("--config={CONFIG_MOUNT}/requests.yaml"),
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
                mount(&paths.requests, REQUESTS_MOUNT, true),
            ]),
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
