use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

use super::{
    convert::{invalid, is_external},
    model::{ComposeError, ComposeProject, RawSecret},
};

const SECRET_PREFIX: &str = "secret://";
const SECRET_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const SECRET_COMMAND_OUTPUT_LIMIT: usize = 1024 * 1024;

impl ComposeProject {
    pub fn resolve_secrets(&mut self) -> Result<(), ComposeError> {
        let references = self
            .services
            .iter()
            .flat_map(|(service, spec)| {
                spec.container
                    .environment
                    .iter()
                    .filter_map(|(key, value)| {
                        value
                            .strip_prefix(SECRET_PREFIX)
                            .filter(|name| !name.is_empty())
                            .map(|name| (service.clone(), key.clone(), name.to_owned()))
                    })
            })
            .collect::<Vec<_>>();
        for (_, _, name) in &references {
            if self.resolved_secrets.contains_key(name) {
                continue;
            }
            let secret = self.secrets.get(name).ok_or_else(|| {
                invalid(format!(
                    "secret '{name}' referenced via '{SECRET_PREFIX}{name}' is not defined"
                ))
            })?;
            let value = resolve_secret(name, secret, &self.working_dir, &self.environment)?;
            self.resolved_secrets.insert(name.clone(), value);
        }
        for (service, key, name) in references {
            let resolved = self
                .resolved_secrets
                .get(&name)
                .ok_or_else(|| invalid(format!("secret '{name}' was not resolved")))?;
            self.services
                .get_mut(&service)
                .and_then(|spec| spec.container.environment.get_mut(&key))
                .ok_or_else(|| invalid(format!("service '{service}' environment changed")))?
                .clone_from(resolved);
        }
        Ok(())
    }
}

pub(super) fn validate_secret(name: &str, secret: &RawSecret) -> Result<(), ComposeError> {
    if is_external(&secret.external) {
        return Err(invalid(format!(
            "secret '{name}': external secrets are not supported"
        )));
    }
    if let Some(command) = &secret.command {
        if command.is_empty() {
            return Err(invalid(format!(
                "secret '{name}': x-command must be a non-empty string"
            )));
        }
        if secret.driver.is_some() || !secret.driver_opts.is_empty() {
            return Err(invalid(format!(
                "secret '{name}': x-command cannot be combined with driver or driver_opts"
            )));
        }
        if secret.file.is_some() || secret.environment.is_some() {
            return Err(invalid(format!(
                "secret '{name}': x-command cannot be combined with file or environment"
            )));
        }
        return Ok(());
    }
    if let Some(driver) = &secret.driver {
        if driver != "exec" {
            return Err(invalid(format!(
                "secret '{name}': unsupported driver '{driver}'"
            )));
        }
        if secret
            .driver_opts
            .get("command")
            .is_none_or(String::is_empty)
        {
            return Err(invalid(format!(
                "secret '{name}': exec driver requires driver_opts.command"
            )));
        }
        if secret.file.is_some() || secret.environment.is_some() {
            return Err(invalid(format!(
                "secret '{name}': a secret using a driver cannot also define file or environment"
            )));
        }
        return Ok(());
    }
    if usize::from(secret.file.is_some()) + usize::from(secret.environment.is_some()) != 1 {
        return Err(invalid(format!(
            "secret '{name}' must define exactly one of file or environment"
        )));
    }
    Ok(())
}

fn resolve_secret(
    name: &str,
    secret: &RawSecret,
    directory: &Path,
    environment: &BTreeMap<String, String>,
) -> Result<String, ComposeError> {
    if let Some(file) = &secret.file {
        return fs::read_to_string(directory.join(file)).map_err(|error| {
            ComposeError::Io(format!("read secret '{name}' file '{file}': {error}"))
        });
    }
    if let Some(variable) = &secret.environment {
        return environment
            .get(variable)
            .cloned()
            .ok_or_else(|| invalid(format!("environment variable '{variable}' is not set")));
    }
    let line = secret
        .command
        .as_ref()
        .or_else(|| secret.driver_opts.get("command"))
        .ok_or_else(|| invalid(format!("secret '{name}' has no source")))?;
    let args = shell_words::split(line).map_err(|error| invalid(error.to_string()))?;
    let (program, args) = args
        .split_first()
        .ok_or_else(|| invalid(format!("secret '{name}' command is empty")))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(directory)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::inherit());
    let output = bounded_output(command, SECRET_COMMAND_TIMEOUT)
        .map_err(|error| invalid(format!("run secret '{name}' command: {error}")))?;
    if !output.status.success() {
        return Err(invalid(format!(
            "run secret '{name}' command: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let mut value = String::from_utf8_lossy(&output.stdout).into_owned();
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    Ok(value)
}

fn bounded_output(mut command: Command, timeout: Duration) -> Result<std::process::Output, String> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child.stdout.take().ok_or("capture stdout")?;
    let stderr = child.stderr.take().ok_or("capture stderr")?;
    let stdout_reader = thread::spawn(move || read_capped(stdout));
    let stderr_reader = thread::spawn(move || read_capped(stderr));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        if start.elapsed() >= timeout {
            #[cfg(unix)]
            {
                let group = format!("-{}", child.id());
                let _ = Command::new("kill")
                    .args(["-KILL", "--", &group])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(format!("timed out after {}s", timeout.as_secs()));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| "stdout reader panicked".to_owned())?
        .map_err(|error| error.to_string())?;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| "stderr reader panicked".to_owned())?
        .map_err(|error| error.to_string())?;
    if stdout_truncated || stderr_truncated {
        return Err(format!(
            "output exceeded {SECRET_COMMAND_OUTPUT_LIMIT} bytes"
        ));
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn read_capped(reader: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    reader
        .take((SECRET_COMMAND_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > SECRET_COMMAND_OUTPUT_LIMIT;
    bytes.truncate(SECRET_COMMAND_OUTPUT_LIMIT);
    Ok((bytes, truncated))
}
