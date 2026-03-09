use std::ffi::OsString;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;

#[cfg(test)]
use std::sync::{Mutex, OnceLock};

pub async fn run_ssh(target: &str, remote_script: &str) -> Result<String, String> {
    run_ssh_inner(target, remote_script, None).await
}

pub async fn run_ssh_with_stdin(
    target: &str,
    remote_script: &str,
    stdin_bytes: &[u8],
) -> Result<String, String> {
    run_ssh_inner(target, remote_script, Some(stdin_bytes)).await
}

async fn run_ssh_inner(
    target: &str,
    remote_script: &str,
    stdin_bytes: Option<&[u8]>,
) -> Result<String, String> {
    let mut command = TokioCommand::new(ssh_program());
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

fn ssh_program() -> OsString {
    #[cfg(test)]
    if let Some(path) = std::env::var_os(TEST_SSH_BIN_ENV) {
        return path;
    }

    OsString::from("ssh")
}

#[must_use] 
pub fn shell_escape(input: &str) -> String {
    input.replace('"', "\\\"")
}

#[must_use] 
pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) const TEST_SSH_BIN_ENV: &str = "PLOYZ_TEST_SSH_BIN";

#[cfg(test)]
pub(crate) fn test_ssh_env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn run_ssh_with_stdin_preserves_quotes_newlines_and_shell_metacharacters() {
        let _guard = test_ssh_env_lock().lock().expect("env lock");
        let temp_dir = unique_temp_dir("ployz-ssh-test");
        std::fs::create_dir_all(&temp_dir).expect("create temp dir");
        let fake_ssh = write_fake_ssh(&temp_dir);

        let payload = "quotes'\nnewlines\n$HOME;`rm -rf /`\"".as_bytes().to_vec();
        let _ssh_guard = EnvVarGuard::set(TEST_SSH_BIN_ENV, Some(fake_ssh.into_os_string()));
        let stdin_path = temp_dir.join("captured.stdin");
        let _stdin_guard = EnvVarGuard::set(
            "PLOYZ_TEST_SSH_STDIN_PATH",
            Some(stdin_path.clone().into_os_string()),
        );

        let output = run_ssh_with_stdin(
            "fake-target",
            "set -eu; ployz mesh join --token-stdin",
            &payload,
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

    fn write_fake_ssh(dir: &PathBuf) -> PathBuf {
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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: Option<OsString>) -> Self {
            let previous = std::env::var_os(key);
            match value {
                Some(value) => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::set_var(key, value) }
                }
                None => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::remove_var(key) }
                }
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::set_var(self.key, value) }
                }
                None => {
                    // Tests serialize PATH/env mutations behind a process-wide mutex.
                    unsafe { std::env::remove_var(self.key) }
                }
            }
        }
    }
}
