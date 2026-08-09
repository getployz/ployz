//! Shared machine-one founding fixtures and bounded Corrosion observations.

use super::{DindMachine, ExecOutcome, exec_in_container, write_file_in_container};
use bollard::Docker;
use ployz_core::corrosion::{QueryEvent, SqliteValue};
use std::collections::BTreeMap;

pub const ARTIFACT_ROOT: &str = "/opt/ployz/artifacts";
pub const RELEASE_MANIFEST: &str = "/run/ployz-dind-release.env";

pub async fn install_local_release_channel(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<String, String> {
    let required = [
        "ployzd",
        "ployz-ebpf-tc",
        "ployz-ebpf-ctl",
        "corrosion",
        "corrosion-schema-v1.sql",
    ];
    let mut digests = BTreeMap::new();
    for name in required {
        let path = format!("{ARTIFACT_ROOT}/{name}");
        let outcome = exec_ok(docker, machine, &["sha256sum", &path]).await?;
        let Some(digest) = outcome.stdout.split_whitespace().next() else {
            return Err(format!("sha256sum returned no digest for {path}"));
        };
        require(
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
            format!("sha256sum returned invalid digest for {path}: {digest:?}"),
        )?;
        digests.insert(name, digest.to_owned());
    }
    let manifest = render_release_manifest(&digests)?;
    write_release_manifest(docker, machine, &manifest).await?;
    Ok(manifest)
}

pub async fn write_release_manifest(
    docker: &Docker,
    machine: &DindMachine,
    manifest: &str,
) -> Result<(), String> {
    write_file_in_container(
        docker,
        &machine.container_id,
        RELEASE_MANIFEST,
        manifest,
        "0600",
    )
    .await
    .map_err(|error| error.to_string())
}

pub fn render_release_manifest(digests: &BTreeMap<&str, String>) -> Result<String, String> {
    let digest = |name| {
        digests
            .get(name)
            .map(String::as_str)
            .ok_or_else(|| format!("missing local artifact digest for {name}"))
    };
    let url = |name| format!("{ARTIFACT_ROOT}/{name}");
    Ok(format!(
        "PLOYZ_RELEASE_PLATFORM=linux-amd64\n\
PLOYZ_VERSION=0.1.0\n\
PLOYZD_URL={}\n\
PLOYZD_SHA256={}\n\
PLOYZ_EBPF_TC_URL={}\n\
PLOYZ_EBPF_TC_SHA256={}\n\
PLOYZ_EBPF_CTL_URL={}\n\
PLOYZ_EBPF_CTL_SHA256={}\n\
PLOYZ_CORROSION_EMBEDDED_VERSION=corrosion 0.2.0-beta.0\n\
PLOYZ_CORROSION_URL={}\n\
PLOYZ_CORROSION_SHA256={}\n\
PLOYZ_CORROSION_SCHEMA_URL={}\n\
PLOYZ_CORROSION_SCHEMA_SHA256={}\n",
        url("ployzd"),
        digest("ployzd")?,
        url("ployz-ebpf-tc"),
        digest("ployz-ebpf-tc")?,
        url("ployz-ebpf-ctl"),
        digest("ployz-ebpf-ctl")?,
        url("corrosion"),
        digest("corrosion")?,
        url("corrosion-schema-v1.sql"),
        digest("corrosion-schema-v1.sql")?,
    ))
}

pub async fn corrosion_access(
    docker: &Docker,
    machine: &DindMachine,
) -> Result<(String, String), String> {
    let env = exec_ok(docker, machine, &["cat", "/var/lib/ployz/ployzd.env"])
        .await?
        .stdout;
    let address = env_value(&env, "PLOYZ_CORROSION_API_ADDR")?;
    let token = exec_ok(docker, machine, &["cat", "/var/lib/ployz/corrosion-token"])
        .await?
        .stdout
        .trim()
        .to_owned();
    Ok((address, token))
}

pub fn env_value(contents: &str, key: &str) -> Result<String, String> {
    contents
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("founding environment omitted {key}"))
}

pub async fn corrosion_query(
    docker: &Docker,
    machine: &DindMachine,
    address: &str,
    token: &str,
    statement: &str,
) -> Result<Vec<Vec<SqliteValue>>, String> {
    let body = serde_json::to_string(statement).map_err(|error| error.to_string())?;
    let url = format!("http://{address}/v1/queries");
    let authorization = format!("Authorization: Bearer {token}");
    let outcome = exec_ok(
        docker,
        machine,
        &[
            "curl",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "3",
            "-H",
            &authorization,
            "-H",
            "Accept: application/json",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &body,
            &url,
        ],
    )
    .await?;
    parse_query_rows(&outcome.stdout)
}

fn parse_query_rows(ndjson: &str) -> Result<Vec<Vec<SqliteValue>>, String> {
    let mut rows = Vec::new();
    let mut saw_columns = false;
    let mut saw_end = false;
    for line in ndjson.lines().filter(|line| !line.trim().is_empty()) {
        let event: QueryEvent = serde_json::from_str(line)
            .map_err(|error| format!("invalid Corrosion query frame {line:?}: {error}"))?;
        match event {
            QueryEvent::Columns(_) => {
                require(
                    !saw_columns && !saw_end,
                    "Corrosion columns frame was out of order",
                )?;
                saw_columns = true;
            }
            QueryEvent::Row(_, values) => {
                require(
                    saw_columns && !saw_end,
                    "Corrosion row frame was out of order",
                )?;
                rows.push(values);
            }
            QueryEvent::EndOfQuery(_) => {
                require(
                    saw_columns && !saw_end,
                    "Corrosion end frame was out of order",
                )?;
                saw_end = true;
            }
            QueryEvent::Error(message) => return Err(format!("Corrosion query failed: {message}")),
        }
    }
    require(saw_columns && saw_end, "Corrosion query did not complete")?;
    Ok(rows)
}

pub async fn exec_ok(
    docker: &Docker,
    machine: &DindMachine,
    command: &[&str],
) -> Result<ExecOutcome, String> {
    let outcome = exec_in_container(docker, &machine.container_id, command)
        .await
        .map_err(|error| error.to_string())?;
    if outcome.success() {
        Ok(outcome)
    } else {
        Err(format!(
            "{} command {command:?} failed: {outcome:?}",
            machine.name
        ))
    }
}

pub fn require(condition: bool, message: impl Into<String>) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parser_requires_a_complete_ordered_stream() {
        let rows = parse_query_rows(
            "{\"columns\":[\"count\"]}\n{\"row\":[1,[1]]}\n{\"eoq\":{\"time\":0.1}}\n",
        )
        .expect("complete query");
        assert_eq!(rows, vec![vec![SqliteValue::Integer(1)]]);
        assert!(parse_query_rows("{\"columns\":[\"count\"]}\n").is_err());
        assert!(parse_query_rows("{\"row\":[1,[1]]}\n{\"eoq\":{\"time\":0.1}}\n").is_err());
    }

    #[test]
    fn founding_environment_parser_requires_exact_nonempty_keys() {
        let env = "PLOYZ_API_LISTEN_ADDR=[fd00::1]:2020\nPLOYZ_CORROSION_API_ADDR=127.0.0.1:8080\n";
        assert_eq!(
            env_value(env, "PLOYZ_API_LISTEN_ADDR").expect("API address"),
            "[fd00::1]:2020"
        );
        assert_eq!(
            env_value(env, "PLOYZ_CORROSION_API_ADDR").expect("Corrosion address"),
            "127.0.0.1:8080"
        );
        assert!(env_value(env, "PLOYZ_API_ADDR").is_err());
    }
}
