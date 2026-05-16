use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const VERSION: u16 = 1;
const MAX_FRAME_SIZE: usize = 1_048_576;
const DOCKER_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Deserialize)]
struct Request {
    version: u16,
    request_id: String,
    op: Operation,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
    #[serde(rename = "docker.start")]
    Start,
    #[serde(rename = "docker.stop")]
    Stop,
    #[serde(rename = "docker.inspect")]
    Inspect,
    #[serde(rename = "docker.list_ployz")]
    ListPloyz,
}

#[derive(Debug, Serialize)]
struct Response {
    version: u16,
    request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    ok: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<HelperError>,
}

#[derive(Debug, Serialize)]
struct HelperError {
    code: String,
    message: String,
    details: Value,
}

#[derive(Debug, Deserialize)]
struct StartParams {
    name: String,
    image: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct NamedParams {
    name: String,
}

fn main() {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let response = match line {
            Ok(line) => handle_line(&line),
            Err(error) => Response::error(
                String::from("unknown"),
                helper_error(
                    "read_failed",
                    "failed to read helper request",
                    json!({ "error": error.to_string() }),
                ),
            ),
        };

        if serde_json::to_writer(&mut stdout, &response).is_ok() {
            let _ = stdout.write_all(b"\n");
            let _ = stdout.flush();
        }
    }
}

fn handle_line(line: &str) -> Response {
    if line.len() > MAX_FRAME_SIZE {
        return Response::error(
            String::from("unknown"),
            helper_error(
                "frame_too_large",
                "helper request frame exceeds limit",
                json!({ "max_bytes": MAX_FRAME_SIZE }),
            ),
        );
    }

    let request = match serde_json::from_str::<Request>(line) {
        Ok(request) => request,
        Err(error) => {
            return Response::error(
                String::from("unknown"),
                helper_error(
                    "invalid_request",
                    "helper request is not valid protocol JSON",
                    json!({ "error": error.to_string() }),
                ),
            );
        }
    };

    if request.version != VERSION {
        return Response::error(
            request.request_id,
            helper_error(
                "unsupported_version",
                "unsupported helper protocol version",
                json!({ "version": request.version }),
            ),
        );
    }

    if !valid_request_id(&request.request_id) {
        return Response::error(
            String::from("unknown"),
            helper_error(
                "invalid_request_id",
                "helper request_id is invalid",
                json!({}),
            ),
        );
    }

    let request_id = request.request_id.clone();

    match execute(request) {
        Ok(ok) => Response::ok(request_id, ok),
        Err(error) => Response::error(request_id, error),
    }
}

fn execute(request: Request) -> Result<Value, HelperError> {
    match request.op {
        Operation::Start => docker_start(request.params),
        Operation::Stop => docker_stop(request.params),
        Operation::Inspect => docker_inspect(request.params),
        Operation::ListPloyz => docker_list_ployz(),
    }
}

fn docker_start(params: Value) -> Result<Value, HelperError> {
    let params: StartParams = parse_params(params)?;
    validate_container_name(&params.name)?;
    validate_image(&params.image)?;
    validate_string_vec("args", &params.args)?;
    validate_string_map("env", &params.env)?;
    validate_string_map("labels", &params.labels)?;

    let mut args = vec![
        String::from("run"),
        String::from("-d"),
        String::from("--name"),
        params.name.clone(),
        String::from("--label"),
        String::from("ployz.managed=true"),
    ];

    for (key, value) in &params.labels {
        args.push(String::from("--label"));
        args.push(format!("{key}={value}"));
    }

    for (key, value) in &params.env {
        args.push(String::from("--env"));
        args.push(format!("{key}={value}"));
    }

    args.push(params.image);
    args.extend(params.args);

    run_docker(&args).map(|output| {
        json!({
            "container_id": output.trim(),
            "name": params.name,
        })
    })
}

fn docker_stop(params: Value) -> Result<Value, HelperError> {
    let params: NamedParams = parse_params(params)?;
    validate_container_name(&params.name)?;
    run_docker(&[String::from("stop"), params.name.clone()]).map(|output| {
        json!({
            "name": params.name,
            "stopped": output.trim(),
        })
    })
}

fn docker_inspect(params: Value) -> Result<Value, HelperError> {
    let params: NamedParams = parse_params(params)?;
    validate_container_name(&params.name)?;
    let output = run_docker(&[String::from("inspect"), params.name])?;
    let parsed = serde_json::from_str::<Value>(&output).map_err(|error| {
        helper_error(
            "docker_output_invalid",
            "docker inspect returned invalid JSON",
            json!({ "error": error.to_string() }),
        )
    })?;
    Ok(json!({ "resources": parsed }))
}

fn docker_list_ployz() -> Result<Value, HelperError> {
    let output = run_docker(&[
        String::from("ps"),
        String::from("-a"),
        String::from("--filter"),
        String::from("label=ployz.managed=true"),
        String::from("--format"),
        String::from("{{json .}}"),
    ])?;

    let mut resources = Vec::new();
    for line in output.lines().filter(|line| !line.trim().is_empty()) {
        let resource = serde_json::from_str::<Value>(line).map_err(|error| {
            helper_error(
                "docker_output_invalid",
                "docker list returned invalid JSON",
                json!({ "error": error.to_string() }),
            )
        })?;
        resources.push(resource);
    }

    Ok(json!({ "resources": resources }))
}

fn run_docker(args: &[String]) -> Result<String, HelperError> {
    let mut child = Command::new("docker")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            helper_error(
                "docker_unavailable",
                "failed to execute docker",
                json!({ "error": error.to_string() }),
            )
        })?;

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => break,
            Ok(None) if started.elapsed() >= DOCKER_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(helper_error(
                    "docker_timeout",
                    "docker command exceeded helper timeout",
                    json!({ "timeout_seconds": DOCKER_TIMEOUT.as_secs() }),
                ));
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                return Err(helper_error(
                    "docker_wait_failed",
                    "failed while waiting for docker",
                    json!({ "error": error.to_string() }),
                ));
            }
        }
    }

    let output = child.wait_with_output().map_err(|error| {
        helper_error(
            "docker_wait_failed",
            "failed to collect docker output",
            json!({ "error": error.to_string() }),
        )
    })?;

    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|error| {
            helper_error(
                "docker_output_invalid",
                "docker returned non-UTF-8 output",
                json!({ "error": error.to_string() }),
            )
        })
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(helper_error(
            "docker_failed",
            "docker command failed",
            json!({
                "status": output.status.code(),
                "stderr": stderr,
            }),
        ))
    }
}

fn parse_params<T>(params: Value) -> Result<T, HelperError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(params).map_err(|error| {
        helper_error(
            "invalid_params",
            "helper operation parameters are invalid",
            json!({ "error": error.to_string() }),
        )
    })
}

fn validate_container_name(name: &str) -> Result<(), HelperError> {
    validate_atomish("name", name)
}

fn validate_image(image: &str) -> Result<(), HelperError> {
    validate_string_value("image", image)
}

fn validate_string_vec(field: &str, values: &[String]) -> Result<(), HelperError> {
    for value in values {
        validate_string_value(field, value)?;
    }
    Ok(())
}

fn validate_string_map(field: &str, values: &BTreeMap<String, String>) -> Result<(), HelperError> {
    for (key, value) in values {
        validate_atomish(field, key)?;
        validate_string_value(field, value)?;
    }
    Ok(())
}

fn validate_atomish(field: &str, value: &str) -> Result<(), HelperError> {
    validate_string_value(field, value)?;
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '/'))
    {
        Ok(())
    } else {
        Err(helper_error(
            "invalid_argument",
            "helper argument contains disallowed characters",
            json!({ "field": field }),
        ))
    }
}

fn validate_string_value(field: &str, value: &str) -> Result<(), HelperError> {
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        Err(helper_error(
            "invalid_argument",
            "helper argument is empty, too long, or contains control characters",
            json!({ "field": field }),
        ))
    } else {
        Ok(())
    }
}

fn valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= 128
        && request_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-'))
}

fn helper_error(code: &str, message: &str, details: Value) -> HelperError {
    HelperError {
        code: code.to_owned(),
        message: message.to_owned(),
        details: redact(details),
    }
}

fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    if is_secret_key(&key) {
                        (key, Value::String(String::from("[REDACTED]")))
                    } else {
                        (key, redact(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact).collect()),
        scalar @ (Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)) => scalar,
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    [
        "account",
        "auth",
        "authorization",
        "bearer",
        "cert",
        "cookie",
        "credential",
        "key",
        "pass",
        "password",
        "pem",
        "private",
        "secret",
        "token",
    ]
    .iter()
    .any(|fragment| normalized.contains(fragment))
}

impl Response {
    fn ok(request_id: String, ok: Value) -> Self {
        Self {
            version: VERSION,
            request_id,
            ok: Some(ok),
            error: None,
        }
    }

    fn error(request_id: String, error: HelperError) -> Self {
        Self {
            version: VERSION,
            request_id,
            ok: None,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests;
