use ployz_sdk::StdioTransport;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;

#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Default)]
pub struct SshOptions {
    pub identity_file: Option<PathBuf>,
}

pub struct EphemeralSshIdentityFile {
    path: PathBuf,
}

impl EphemeralSshIdentityFile {
    pub fn write(private_key: &str) -> Result<Self, String> {
        let pid = std::process::id();
        let base_dir = std::env::temp_dir();
        for attempt in 0..16 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|err| format!("system clock error: {err}"))?
                .as_nanos();
            let path = base_dir.join(format!("ployz-ssh-{pid}-{nanos}-{attempt}.key"));
            match write_identity_file(&path, private_key) {
                Ok(()) => return Ok(Self { path }),
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    return Err(format!(
                        "failed to write operation-scoped ssh identity '{}': {err}",
                        path.display()
                    ));
                }
            }
        }
        Err("failed to allocate a unique path for operation-scoped ssh identity".into())
    }

    #[must_use]
    pub fn ssh_options(&self) -> SshOptions {
        SshOptions {
            identity_file: Some(self.path.clone()),
        }
    }
}

impl Drop for EphemeralSshIdentityFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub async fn run_ssh(
    target: &str,
    remote_script: &str,
    options: &SshOptions,
) -> Result<String, String> {
    run_ssh_inner(target, remote_script, None, options).await
}

pub async fn run_ssh_with_stdin(
    target: &str,
    remote_script: &str,
    stdin_bytes: &[u8],
    options: &SshOptions,
) -> Result<String, String> {
    run_ssh_inner(target, remote_script, Some(stdin_bytes), options).await
}

#[must_use]
pub fn ssh_stdio_transport(
    target: &str,
    remote_script: &str,
    options: &SshOptions,
) -> StdioTransport {
    let mut transport = StdioTransport::new(ssh_program());
    #[cfg(test)]
    {
        let overrides = test_ssh_overrides_snapshot();
        for (key, value) in overrides.env {
            transport = transport.env(key, value);
        }
    }
    transport = append_common_ssh_transport_options(transport);
    if let Some(identity_file) = &options.identity_file {
        transport = transport
            .arg("-i")
            .arg(identity_file)
            .arg("-o")
            .arg("IdentitiesOnly=yes");
    }
    transport.arg(target).arg(remote_script)
}

async fn run_ssh_inner(
    target: &str,
    remote_script: &str,
    stdin_bytes: Option<&[u8]>,
    options: &SshOptions,
) -> Result<String, String> {
    let mut command = TokioCommand::new(ssh_program());
    #[cfg(test)]
    {
        let overrides = test_ssh_overrides_snapshot();
        for (key, value) in overrides.env {
            command.env(key, value);
        }
    }
    append_common_ssh_options(&mut command);
    if let Some(identity_file) = &options.identity_file {
        command
            .arg("-i")
            .arg(identity_file)
            .arg("-o")
            .arg("IdentitiesOnly=yes");
    }
    command.arg(target).arg(remote_script);
    if stdin_bytes.is_some() {
        command.stdin(Stdio::piped());
    }

    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("ssh execution failed: {e}"))?;

    if let Some(bytes) = stdin_bytes {
        let Some(mut stdin) = child.stdin.take() else {
            return Err("ssh stdin unavailable".into());
        };
        stdin
            .write_all(bytes)
            .await
            .map_err(|e| format!("ssh stdin write failed: {e}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("ssh stdin shutdown failed: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("ssh execution failed: {e}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if output.status.success() {
        return Ok(stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!(
        "ssh to '{target}' failed (status: {}){}",
        output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into()),
        if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        }
    ))
}

fn append_common_ssh_options(command: &mut TokioCommand) {
    command
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("ConnectTimeout=10");
}

fn append_common_ssh_transport_options(transport: StdioTransport) -> StdioTransport {
    transport
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=accept-new")
        .arg("-o")
        .arg("ConnectTimeout=10")
}

fn ssh_program() -> OsString {
    #[cfg(test)]
    if let Some(path) = test_ssh_overrides_snapshot().program {
        return path;
    }

    OsString::from("ssh")
}

fn write_identity_file(path: &Path, private_key: &str) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(private_key.as_bytes())?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
#[derive(Debug, Clone, Default)]
struct TestSshOverrides {
    program: Option<OsString>,
    env: BTreeMap<&'static str, OsString>,
}

#[cfg(test)]
fn test_ssh_overrides() -> &'static Mutex<TestSshOverrides> {
    static OVERRIDES: OnceLock<Mutex<TestSshOverrides>> = OnceLock::new();
    OVERRIDES.get_or_init(|| Mutex::new(TestSshOverrides::default()))
}

#[cfg(test)]
fn test_ssh_overrides_snapshot() -> TestSshOverrides {
    test_ssh_overrides()
        .lock()
        .expect("ssh overrides lock")
        .clone()
}

#[cfg(test)]
pub(crate) struct TestSshProgramGuard {
    previous: Option<OsString>,
}

#[cfg(test)]
impl TestSshProgramGuard {
    pub(crate) fn set(path: PathBuf) -> Self {
        let mut overrides = test_ssh_overrides().lock().expect("ssh overrides lock");
        let previous = overrides.program.replace(path.into_os_string());
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for TestSshProgramGuard {
    fn drop(&mut self) {
        let mut overrides = test_ssh_overrides().lock().expect("ssh overrides lock");
        overrides.program = self.previous.take();
    }
}

#[cfg(test)]
pub(crate) struct TestSshEnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

#[cfg(test)]
impl TestSshEnvGuard {
    pub(crate) fn set(key: &'static str, value: Option<OsString>) -> Self {
        let mut overrides = test_ssh_overrides().lock().expect("ssh overrides lock");
        let previous = match value {
            Some(value) => overrides.env.insert(key, value),
            None => overrides.env.remove(key),
        };
        Self { key, previous }
    }
}

#[cfg(test)]
impl Drop for TestSshEnvGuard {
    fn drop(&mut self) {
        let mut overrides = test_ssh_overrides().lock().expect("ssh overrides lock");
        match self.previous.take() {
            Some(value) => {
                overrides.env.insert(self.key, value);
            }
            None => {
                overrides.env.remove(self.key);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_ssh_env_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn run_ssh_with_stdin_preserves_quotes_newlines_and_shell_metacharacters() {
        let _guard = test_ssh_env_lock().lock().await;
        let temp_dir = unique_temp_dir("ployz-ssh-test");
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let fake_ssh = write_fake_ssh(&temp_dir);

        let payload = "quotes'\nnewlines\n$HOME;`rm -rf /`\"".as_bytes().to_vec();
        let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
        let stdin_path = temp_dir.join("captured.stdin");
        let _stdin_guard = TestSshEnvGuard::set(
            "PLOYZ_TEST_SSH_STDIN_PATH",
            Some(stdin_path.clone().into_os_string()),
        );

        let output = run_ssh_with_stdin(
            "fake-target",
            "set -eu; ployzd mesh join --token-stdin",
            &payload,
            &SshOptions::default(),
        )
        .await
        .expect("ssh with stdin");
        assert_eq!(output, "ok");

        let captured = std::fs::read(&stdin_path).expect("read captured stdin");
        assert_eq!(captured, payload);
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }

    fn write_fake_ssh(dir: &std::path::Path) -> PathBuf {
        let script = dir.join("ssh");
        std::fs::write(
            &script,
            "#!/bin/sh\ncat \"$PLOYZ_TEST_SSH_STDIN_PATH\" >/dev/null 2>&1 || true\ncat > \"$PLOYZ_TEST_SSH_STDIN_PATH\"\nprintf 'ok'\n",
        )
        .expect("write fake ssh");

        #[cfg(unix)]
        {
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("set script permissions");
        }

        script
    }

    #[tokio::test]
    async fn run_ssh_passes_explicit_identity_file() {
        let _guard = test_ssh_env_lock().lock().await;
        let temp_dir = unique_temp_dir("ployz-ssh-identity-test");
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let fake_ssh = temp_dir.join("ssh");
        let args_path = temp_dir.join("captured.args");

        std::fs::write(
            &fake_ssh,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf 'ok'\n",
                args_path.display()
            ),
        )
        .expect("write fake ssh");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(&fake_ssh)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&fake_ssh, permissions).expect("set script permissions");
        }

        let _ssh_guard = TestSshProgramGuard::set(fake_ssh);
        let identity = temp_dir.join("id_ed25519");
        std::fs::write(&identity, "fake-private-key").expect("write identity");

        let output = run_ssh(
            "fake-target",
            "echo ok",
            &SshOptions {
                identity_file: Some(identity.clone()),
            },
        )
        .await
        .expect("ssh with identity");
        assert_eq!(output, "ok");

        let args = std::fs::read_to_string(&args_path).expect("read args");
        assert!(args.contains("BatchMode=yes"));
        assert!(args.contains("StrictHostKeyChecking=accept-new"));
        assert!(args.contains("ConnectTimeout=10"));
        assert!(args.contains("-i"));
        assert!(args.contains(&identity.display().to_string()));
        assert!(args.contains("IdentitiesOnly=yes"));
        assert!(args.contains("fake-target"));
    }
}
