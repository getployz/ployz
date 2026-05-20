use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use mvp_deploy::InstanceId;

use crate::{
    ManagedHttpCommand, ProcessInstance, ProcessInstanceSpec, ProcessRuntimeConfig, RuntimeBackend,
    RuntimeError, RuntimeResult, RuntimeState,
};

#[derive(Debug, Clone)]
pub struct ProcessRuntime {
    config: ProcessRuntimeConfig,
}

impl ProcessRuntime {
    #[must_use]
    pub fn new(config: ProcessRuntimeConfig) -> Self {
        Self { config }
    }

    pub fn managed_http(root: impl Into<PathBuf>, program: impl Into<PathBuf>) -> Self {
        Self::new(ProcessRuntimeConfig::new(
            root,
            ManagedHttpCommand::new(program),
        ))
    }

    pub fn prepare(&self, spec: &ProcessInstanceSpec) -> RuntimeResult<ProcessInstance> {
        let dir = self.instance_dir(&spec.instance_id);
        fs::create_dir_all(dir.join("www")).map_err(|source| RuntimeError::CreateDir {
            path: dir.clone(),
            source,
        })?;
        let index = format!(
            "instance={} service={} revision={}\n",
            spec.instance_id,
            spec.service.as_str(),
            spec.revision.as_str()
        );
        fs::write(dir.join("www").join("index.txt"), index).map_err(|source| {
            RuntimeError::WriteMetadata {
                path: dir.join("www").join("index.txt"),
                source,
            }
        })?;
        let instance = match self.read_instance(&spec.instance_id)? {
            Some(existing)
                if matches!(
                    existing.state,
                    RuntimeState::Running | RuntimeState::Draining
                ) =>
            {
                existing
            }
            Some(mut existing) => {
                existing.service = spec.service.clone();
                existing.revision = spec.revision.clone();
                existing.state = RuntimeState::Prepared;
                existing
            }
            None => ProcessInstance {
                instance_id: spec.instance_id.clone(),
                service: spec.service.clone(),
                revision: spec.revision.clone(),
                address: allocate_loopback_address()?,
                backend_id: None,
                backend_name: None,
                pid: None,
                state: RuntimeState::Prepared,
            },
        };
        self.write_instance(&instance)?;
        Ok(instance)
    }

    pub fn start(&self, spec: &ProcessInstanceSpec) -> RuntimeResult<ProcessInstance> {
        let mut instance = self.prepare(spec)?;
        if matches!(
            instance.state,
            RuntimeState::Running | RuntimeState::Draining
        ) && is_ready(&instance.address)
        {
            return Ok(instance);
        }
        let root = self.instance_dir(&spec.instance_id).join("www");
        let child = Command::new(&self.config.managed_http.program)
            .arg("runtime-http")
            .arg("--addr")
            .arg(&instance.address)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|source| RuntimeError::Spawn {
                instance_id: spec.instance_id.to_string(),
                source,
            })?;
        instance.pid = Some(child.id());
        instance.state = RuntimeState::Running;
        self.write_instance(&instance)?;
        wait_ready(&instance)?;
        Ok(instance)
    }

    pub fn drain(&self, instance_id: &InstanceId) -> RuntimeResult<Option<ProcessInstance>> {
        let Some(mut instance) = self.read_instance(instance_id)? else {
            return Ok(None);
        };
        if !matches!(instance.state, RuntimeState::Stopped) {
            instance.state = RuntimeState::Draining;
            self.write_instance(&instance)?;
        }
        Ok(Some(instance))
    }

    pub fn stop(&self, instance_id: &InstanceId) -> RuntimeResult<Option<ProcessInstance>> {
        let Some(mut instance) = self.read_instance(instance_id)? else {
            return Ok(None);
        };
        if let Some(pid) = instance.pid {
            stop_pid(instance_id, pid)?;
        }
        instance.pid = None;
        instance.state = RuntimeState::Stopped;
        self.write_instance(&instance)?;
        Ok(Some(instance))
    }

    pub fn list(&self) -> RuntimeResult<Vec<ProcessInstance>> {
        let instances_dir = self.config.root.join("instances");
        let entries = match fs::read_dir(&instances_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(RuntimeError::ReadMetadata {
                    path: instances_dir,
                    source,
                });
            }
        };
        let mut instances = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| RuntimeError::ReadMetadata {
                path: self.config.root.join("instances"),
                source,
            })?;
            let path = entry.path().join("instance.json");
            if path.exists() {
                instances.push(read_metadata(&path)?);
            }
        }
        instances.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        Ok(instances)
    }

    fn read_instance(&self, instance_id: &InstanceId) -> RuntimeResult<Option<ProcessInstance>> {
        let path = self.metadata_path(instance_id);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| RuntimeError::DecodeMetadata { path, source }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(RuntimeError::ReadMetadata { path, source }),
        }
    }

    fn write_instance(&self, instance: &ProcessInstance) -> RuntimeResult<()> {
        let path = self.metadata_path(&instance.instance_id);
        let parent = path.parent().expect("runtime metadata path has parent");
        fs::create_dir_all(parent).map_err(|source| RuntimeError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
        let bytes = serde_json::to_vec_pretty(instance)
            .map_err(|source| RuntimeError::EncodeMetadata { source })?;
        let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|source| {
            RuntimeError::WriteMetadata {
                path: path.clone(),
                source,
            }
        })?;
        file.write_all(&bytes)
            .and_then(|_| file.as_file_mut().sync_all())
            .map_err(|source| RuntimeError::WriteMetadata {
                path: path.clone(),
                source,
            })?;
        file.persist(&path)
            .map(|_file| ())
            .map_err(|error| RuntimeError::WriteMetadata {
                path,
                source: error.error,
            })
    }

    fn instance_dir(&self, instance_id: &InstanceId) -> PathBuf {
        self.config
            .root
            .join("instances")
            .join(instance_id.as_str())
    }

    fn metadata_path(&self, instance_id: &InstanceId) -> PathBuf {
        self.instance_dir(instance_id).join("instance.json")
    }
}

impl RuntimeBackend for ProcessRuntime {
    fn prepare(&self, spec: &ProcessInstanceSpec) -> RuntimeResult<ProcessInstance> {
        Self::prepare(self, spec)
    }

    fn start(&self, spec: &ProcessInstanceSpec) -> RuntimeResult<ProcessInstance> {
        Self::start(self, spec)
    }

    fn drain(&self, instance_id: &InstanceId) -> RuntimeResult<Option<ProcessInstance>> {
        Self::drain(self, instance_id)
    }

    fn stop(&self, instance_id: &InstanceId) -> RuntimeResult<Option<ProcessInstance>> {
        Self::stop(self, instance_id)
    }

    fn list(&self) -> RuntimeResult<Vec<ProcessInstance>> {
        Self::list(self)
    }
}

pub fn run_static_http_server(addr: &str, root: &Path) -> RuntimeResult<()> {
    let listener = TcpListener::bind(addr).map_err(|source| RuntimeError::HttpServer { source })?;
    for stream in listener.incoming() {
        let mut stream = stream.map_err(|source| RuntimeError::HttpServer { source })?;
        let mut request = [0u8; 1024];
        if stream.read(&mut request).unwrap_or(0) == 0 {
            continue;
        }
        let body = fs::read(root.join("index.txt")).unwrap_or_else(|_| b"ok\n".to_vec());
        let header = format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream
            .write_all(header.as_bytes())
            .and_then(|_| stream.write_all(&body));
    }
    Ok(())
}

fn read_metadata(path: &Path) -> RuntimeResult<ProcessInstance> {
    let bytes = fs::read(path).map_err(|source| RuntimeError::ReadMetadata {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| RuntimeError::DecodeMetadata {
        path: path.to_path_buf(),
        source,
    })
}

fn allocate_loopback_address() -> RuntimeResult<String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|source| RuntimeError::HttpServer { source })?;
    let addr = listener
        .local_addr()
        .map_err(|source| RuntimeError::HttpServer { source })?;
    Ok(addr.to_string())
}

fn wait_ready(instance: &ProcessInstance) -> RuntimeResult<()> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(2) {
        if is_ready(&instance.address) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(RuntimeError::ReadinessTimeout {
        instance_id: instance.instance_id.to_string(),
        address: instance.address.clone(),
    })
}

fn is_ready(address: &str) -> bool {
    let Ok(addr) = address.parse::<SocketAddr>() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok()
}

fn stop_pid(instance_id: &InstanceId, pid: u32) -> RuntimeResult<()> {
    if !pid_exists(pid) {
        return Ok(());
    }
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map_err(|source| RuntimeError::Stop {
            instance_id: instance_id.to_string(),
            pid,
            source,
        })?;
    if status.success() {
        wait_pid_exited(instance_id, pid, Duration::from_secs(2))
    } else if !pid_exists(pid) {
        Ok(())
    } else {
        Err(RuntimeError::Stop {
            instance_id: instance_id.to_string(),
            pid,
            source: std::io::Error::other(format!("kill exited with {status}")),
        })
    }
}

fn wait_pid_exited(instance_id: &InstanceId, pid: u32, timeout: Duration) -> RuntimeResult<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_exists(pid) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    let status = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status()
        .map_err(|source| RuntimeError::Stop {
            instance_id: instance_id.to_string(),
            pid,
            source,
        })?;
    if !status.success() {
        if !pid_exists(pid) {
            return Ok(());
        }
        return Err(RuntimeError::Stop {
            instance_id: instance_id.to_string(),
            pid,
            source: std::io::Error::other(format!("kill -KILL exited with {status}")),
        });
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if !pid_exists(pid) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(RuntimeError::Stop {
        instance_id: instance_id.to_string(),
        pid,
        source: std::io::Error::other("process did not exit after SIGKILL"),
    })
}

fn pid_exists(pid: u32) -> bool {
    if pid_is_zombie(pid) {
        return false;
    }
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
fn pid_is_zombie(pid: u32) -> bool {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    status
        .split_whitespace()
        .nth(2)
        .is_some_and(|state| state == "Z")
}

#[cfg(target_os = "macos")]
fn pid_is_zombie(pid: u32) -> bool {
    let Ok(output) = Command::new("ps")
        .arg("-o")
        .arg("stat=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
    else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim_start()
        .starts_with('Z')
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pid_is_zombie(_pid: u32) -> bool {
    false
}
