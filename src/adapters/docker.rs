#[derive(Debug, Clone)]
pub struct DockerRuntime {
    pub socket_path: String,
}

impl Default for DockerRuntime {
    fn default() -> Self {
        Self {
            socket_path: "/var/run/docker.sock".into(),
        }
    }
}
