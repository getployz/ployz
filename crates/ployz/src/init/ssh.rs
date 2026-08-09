//! SSH bootstrap delivery using mesh-owned operator identity and context state.

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use wait_timeout::ChildExt as _;

use crate::commands::{InitCommand, InitDriver, SshTarget, render_service_urls, render_storage};
use crate::init::presentation;
use crate::mesh::context::{SSH_CONTEXT_HANDOFF_PREFIX, SshContextHandoff};
pub use crate::mesh::context::{SshPeerKey, default_config_home};

const SSH_PROCESS_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const ENDPOINT_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_SSH_OUTPUT_LINE_BYTES: usize = 64 * 1024;
const MAX_SSH_OUTPUT_LINES: usize = 10_000;

impl SshPeerKey {
    #[must_use]
    pub fn remote_script(&self, command: &InitCommand) -> String {
        let mut args = vec!["ployz".to_owned(), "init".to_owned()];
        args.extend([
            "--storage".to_owned(),
            render_storage(command.storage).to_owned(),
        ]);
        args.extend([
            "--container-network".to_owned(),
            command.container_network.as_string(),
            "--service-urls".to_owned(),
            render_service_urls(&command.service_urls),
        ]);
        push_option(&mut args, "--cluster-name", command.cluster_name.as_deref());
        push_option(&mut args, "--machine-name", command.machine_name.as_deref());
        let inferred_endpoint = match &command.driver {
            InitDriver::SshTarget(target) => target.inferred_wireguard_endpoint(),
            InitDriver::OnHost | InitDriver::Cloud(_) | InitDriver::SshPeer(_) => None,
        };
        if let Some(endpoint) = command.wireguard_endpoint.or(inferred_endpoint) {
            args.extend(["--wireguard-endpoint".to_owned(), endpoint.to_string()]);
        }
        args.extend([
            "--driver-peer-id".to_owned(),
            self.peer_id.to_string(),
            "--driver-peer-public-key".to_owned(),
            self.public_key.as_str().to_owned(),
        ]);
        let init_line = args
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "curl -fsSL --connect-timeout 10 --max-time 120 --retry 3 https://ployz.sh | sh && {init_line}"
        )
    }

    pub fn run(
        &self,
        target: &SshTarget,
        command: &InitCommand,
    ) -> Result<SshContextHandoff, SshInitError> {
        let script = self.remote_script(command);
        let remote_command = format!("sh -lc {}", shell_quote(&script));
        let mut child = Command::new("ssh")
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=3",
                "--",
                target.as_str(),
                &remote_command,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(SshInitError::Launch)?;
        wait_for_context_handoff(&mut child, target, SSH_PROCESS_TIMEOUT)
    }
}

pub async fn resolve_command_endpoint(command: &mut InitCommand) -> Result<(), SshInitError> {
    if command.wireguard_endpoint.is_some() {
        return Ok(());
    }
    let InitDriver::SshTarget(target) = &command.driver else {
        return Ok(());
    };
    command.wireguard_endpoint = Some(resolve_endpoint(target, ENDPOINT_RESOLUTION_TIMEOUT).await?);
    Ok(())
}

async fn resolve_endpoint(
    target: &SshTarget,
    timeout: Duration,
) -> Result<SocketAddr, SshInitError> {
    if let Some(endpoint) = target.inferred_wireguard_endpoint() {
        return Ok(endpoint);
    }
    let target_name = target.clone();
    let host = target
        .as_str()
        .strip_prefix("root@")
        .expect("SshTarget always contains the root@ prefix")
        .to_owned();
    let resolution = tokio::time::timeout(
        timeout,
        tokio::net::lookup_host((
            host.as_str(),
            ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
        )),
    )
    .await;
    match resolution {
        Ok(Ok(addresses)) => choose_resolved_endpoint(target_name, addresses.collect()),
        Ok(Err(error)) => Err(SshInitError::EndpointResolution {
            target: target_name,
            reason: error.to_string(),
        }),
        Err(_) => Err(SshInitError::EndpointResolutionTimedOut {
            target: target_name,
        }),
    }
}

fn choose_resolved_endpoint(
    target: SshTarget,
    mut addresses: Vec<SocketAddr>,
) -> Result<SocketAddr, SshInitError> {
    addresses.sort_unstable();
    addresses.dedup();
    match addresses.as_slice() {
        [] => Err(SshInitError::EndpointResolution {
            target,
            reason: "name resolved to no addresses".to_owned(),
        }),
        [endpoint] => Ok(*endpoint),
        [_, _, ..] => Err(SshInitError::EndpointResolutionAmbiguous { target, addresses }),
    }
}

fn wait_for_context_handoff(
    child: &mut Child,
    target: &SshTarget,
    timeout: Duration,
) -> Result<SshContextHandoff, SshInitError> {
    let Some(stdout) = child.stdout.take() else {
        return Err(SshInitError::MissingStdoutPipe);
    };
    let reader = thread::spawn(move || {
        let stdout_writer = std::io::stdout();
        let mut stdout_writer = stdout_writer.lock();
        collect_ssh_stdout(stdout, &mut stdout_writer)
    });
    let status = match child.wait_timeout(timeout).map_err(SshInitError::Launch)? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(SshInitError::TimedOut);
        }
    };
    let handoff = reader.join().map_err(|_| SshInitError::ReaderPanicked)?;
    if !status.success() {
        return Err(SshInitError::RemoteFailed {
            code: status.code(),
        });
    }
    let Some(context) = handoff? else {
        return Err(SshInitError::ContextHandoffUnavailable {
            target: target.clone(),
        });
    };
    Ok(context)
}

fn collect_ssh_stdout(
    mut reader: impl Read,
    writer: &mut impl Write,
) -> Result<Option<SshContextHandoff>, SshInitError> {
    let mut buffer = [0_u8; 4096];
    let mut line = Vec::new();
    let mut line_oversized = false;
    let mut line_count = 0_usize;
    let mut context = None;
    let mut first_error = None;

    loop {
        let read = reader.read(&mut buffer).map_err(SshInitError::OutputRead)?;
        if read == 0 {
            break;
        }
        for &byte in buffer
            .get(..read)
            .expect("Read cannot return more bytes than the supplied buffer")
        {
            if !line_oversized {
                if line.len() == MAX_SSH_OUTPUT_LINE_BYTES {
                    line_oversized = true;
                } else {
                    line.push(byte);
                }
            }
            if byte == b'\n' {
                line_count = line_count.saturating_add(1);
                finish_output_line(
                    &line,
                    line_oversized,
                    line_count,
                    writer,
                    &mut context,
                    &mut first_error,
                )?;
                line.clear();
                line_oversized = false;
            }
        }
    }
    if !line.is_empty() || line_oversized {
        line_count = line_count.saturating_add(1);
        finish_output_line(
            &line,
            line_oversized,
            line_count,
            writer,
            &mut context,
            &mut first_error,
        )?;
    }
    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(context)
    }
}

fn finish_output_line(
    line: &[u8],
    oversized: bool,
    line_count: usize,
    writer: &mut impl Write,
    context: &mut Option<SshContextHandoff>,
    first_error: &mut Option<SshInitError>,
) -> Result<(), SshInitError> {
    if line_count > MAX_SSH_OUTPUT_LINES {
        first_error.get_or_insert(SshInitError::TooManyOutputLines {
            limit: MAX_SSH_OUTPUT_LINES,
        });
        return Ok(());
    }
    if oversized {
        first_error.get_or_insert(SshInitError::OutputLineTooLong {
            limit: MAX_SSH_OUTPUT_LINE_BYTES,
        });
        return Ok(());
    }
    if line.starts_with(SSH_CONTEXT_HANDOFF_PREFIX.as_bytes()) {
        let record = std::str::from_utf8(trim_line_ending(line))
            .map_err(|error| SshInitError::MalformedContextHandoff(error.to_string()));
        match record.and_then(|record| {
            SshContextHandoff::decode_handoff(record)
                .map_err(|error| SshInitError::MalformedContextHandoff(error.to_string()))
        }) {
            Ok(found) if context.is_none() => *context = Some(found),
            Ok(_) => {
                first_error.get_or_insert(SshInitError::DuplicateContextHandoff);
            }
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
        return Ok(());
    }
    writer.write_all(line).map_err(SshInitError::OutputWrite)?;
    writer.flush().map_err(SshInitError::OutputWrite)
}

fn trim_line_ending(mut line: &[u8]) -> &[u8] {
    if let Some(without_newline) = line.strip_suffix(b"\n") {
        line = without_newline;
    }
    line.strip_suffix(b"\r").unwrap_or(line)
}

fn push_option(args: &mut Vec<String>, flag: &str, value: Option<&str>) {
    if let Some(value) = value {
        args.extend([flag.to_owned(), value.to_owned()]);
    }
}

#[must_use]
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[derive(Debug, thiserror::Error)]
pub enum SshInitError {
    #[error(
        "could not resolve WireGuard endpoint for {target}: {reason}; run `ployz init {target} --wireguard-endpoint <ip>:51820`"
    )]
    EndpointResolution { target: SshTarget, reason: String },
    #[error(
        "WireGuard endpoint resolution for {target} returned multiple addresses {addresses:?}; run `ployz init {target} --wireguard-endpoint <ip>:51820`"
    )]
    EndpointResolutionAmbiguous {
        target: SshTarget,
        addresses: Vec<SocketAddr>,
    },
    #[error(
        "WireGuard endpoint resolution for {target} exceeded its bounded deadline; run `ployz init {target} --wireguard-endpoint <ip>:51820`"
    )]
    EndpointResolutionTimedOut { target: SshTarget },
    #[error("could not launch ssh: {0}")]
    Launch(std::io::Error),
    #[error("could not read ssh output: {0}")]
    OutputRead(std::io::Error),
    #[error("could not forward ssh output: {0}")]
    OutputWrite(std::io::Error),
    #[error("ssh output line exceeded the {limit}-byte limit")]
    OutputLineTooLong { limit: usize },
    #[error("ssh output exceeded the {limit}-line limit")]
    TooManyOutputLines { limit: usize },
    #[error("SSH context handoff is malformed: {0}")]
    MalformedContextHandoff(String),
    #[error("remote init emitted more than one SSH context handoff")]
    DuplicateContextHandoff,
    #[error("ssh context reader stopped unexpectedly")]
    ReaderPanicked,
    #[error("ssh stdout was not piped")]
    MissingStdoutPipe,
    #[error("SSH init exceeded its bounded deadline")]
    TimedOut,
    #[error("remote init failed{code_suffix}", code_suffix = code.map(|code| format!(" with exit code {code}")).unwrap_or_default())]
    RemoteFailed { code: Option<i32> },
    #[error("{}", presentation::context_handoff_unavailable(target.as_str()))]
    ContextHandoffUnavailable { target: SshTarget },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{Command as CliCommand, parse_command};
    use ployz_core::corrosion::{MachineTransport, MeshProvider, derive_builtin_wireguard_member};
    use ployz_core::ids::ClusterName;
    use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
    use std::fs;

    fn init(args: &[&str]) -> InitCommand {
        let CliCommand::Init(command) =
            parse_command(args.iter().map(ToString::to_string)).expect("init parses")
        else {
            panic!("expected init")
        };
        *command
    }

    #[test]
    fn key_is_persisted_and_reused_for_retry() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let first = SshPeerKey::load_or_create(temp.path(), &target, "laptop".to_owned())
            .expect("first key");
        let second = SshPeerKey::load_or_create(temp.path(), &target, "ignored".to_owned())
            .expect("same key");
        assert_eq!(first, second);
        assert_eq!(
            fs::read_dir(temp.path().join("mesh/init-peers"))
                .expect("key directory")
                .count(),
            1
        );
    }

    #[test]
    fn remote_line_stages_then_runs_exact_on_host_primitive() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key = SshPeerKey::load_or_create(temp.path(), &target, "nicks-laptop".to_owned())
            .expect("key");
        let script = key.remote_script(&init(&[
            "init",
            "root@203.0.113.7",
            "--storage",
            "plain",
            "--service-urls",
            "custom:apps.example.com",
        ]));
        assert!(script.starts_with(
            "curl -fsSL --connect-timeout 10 --max-time 120 --retry 3 https://ployz.sh | sh && 'ployz' 'init'"
        ));
        assert!(script.contains("'--driver-peer-public-key'"));
        assert!(script.contains("'--wireguard-endpoint' '203.0.113.7:51820'"));
        assert!(script.contains("'nicks-laptop'"));
        let encoded = serde_json::to_value(&key).expect("key JSON");
        let private_key = encoded
            .get("private_key")
            .and_then(serde_json::Value::as_str)
            .expect("private key");
        assert!(!script.contains(private_key));
        assert!(!format!("{key:?}").contains(private_key));
    }

    #[test]
    fn shell_quoting_cannot_open_a_second_command() {
        assert_eq!(shell_quote("x'; reboot #"), "'x'\"'\"'; reboot #'");
    }

    #[test]
    fn explicit_wireguard_endpoint_wins_over_ssh_ip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let key =
            SshPeerKey::load_or_create(temp.path(), &target, "laptop".to_owned()).expect("key");
        let script = key.remote_script(&init(&[
            "init",
            "root@203.0.113.7",
            "--wireguard-endpoint",
            "198.51.100.8:51999",
        ]));
        assert!(script.contains("'--wireguard-endpoint' '198.51.100.8:51999'"));
        assert!(!script.contains("203.0.113.7:51820"));
    }

    fn handoff() -> SshContextHandoff {
        let cluster_id = ClusterName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster id");
        let public_key =
            WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("machine public key");
        SshContextHandoff {
            machine_transport: MachineTransport::Wireguard {
                addr_v6: derive_builtin_wireguard_member(&cluster_id, &public_key)
                    .bind_address()
                    .get(),
                pubkey: public_key,
                endpoint: Some("203.0.113.7:51820".parse().expect("endpoint")),
                subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24").expect("machine subnet"),
            },
            cluster_id,
            provider: MeshProvider::BuiltinWireguard,
        }
    }

    #[test]
    fn ssh_stdout_forwards_human_lines_and_filters_one_handoff() {
        let record = handoff().encode_handoff().expect("record");
        let input = format!("preparing\n{record}ready\n");
        let mut output = Vec::new();
        assert_eq!(
            collect_ssh_stdout(input.as_bytes(), &mut output)
                .expect("collect")
                .expect("handoff"),
            handoff()
        );
        assert_eq!(output, b"preparing\nready\n");
    }

    #[test]
    fn malformed_duplicate_and_oversized_handoffs_are_typed() {
        let mut output = Vec::new();
        let malformed = format!("{SSH_CONTEXT_HANDOFF_PREFIX}{{not json}}\n");
        assert!(matches!(
            collect_ssh_stdout(malformed.as_bytes(), &mut output),
            Err(SshInitError::MalformedContextHandoff(_))
        ));

        let record = handoff().encode_handoff().expect("record");
        assert!(matches!(
            collect_ssh_stdout(format!("{record}{record}").as_bytes(), &mut output),
            Err(SshInitError::DuplicateContextHandoff)
        ));

        let oversized = vec![b'x'; MAX_SSH_OUTPUT_LINE_BYTES + 1];
        assert!(matches!(
            collect_ssh_stdout(oversized.as_slice(), &mut output),
            Err(SshInitError::OutputLineTooLong { .. })
        ));

        let too_many_lines = vec![b'\n'; MAX_SSH_OUTPUT_LINES + 1];
        assert!(matches!(
            collect_ssh_stdout(too_many_lines.as_slice(), &mut Vec::new()),
            Err(SshInitError::TooManyOutputLines { .. })
        ));
    }

    #[test]
    fn successful_remote_without_handoff_is_release_skew() {
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let mut child = Command::new("sh")
            .args(["-c", "printf 'old remote output\\n'"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("child");
        let error = wait_for_context_handoff(&mut child, &target, Duration::from_secs(1))
            .expect_err("missing handoff");
        assert!(matches!(
            &error,
            SshInitError::ContextHandoffUnavailable { .. }
        ));
        assert_eq!(
            error.to_string(),
            presentation::context_handoff_unavailable(target.as_str())
        );
    }

    #[test]
    fn stalled_remote_is_killed_at_the_caller_thread_deadline() {
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let mut child = Command::new("sh")
            .args(["-c", "exec sleep 60"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("child");
        assert!(matches!(
            wait_for_context_handoff(&mut child, &target, Duration::from_millis(20)),
            Err(SshInitError::TimedOut)
        ));
        assert!(child.try_wait().expect("reaped child").is_some());
    }

    #[tokio::test]
    async fn ssh_ip_endpoint_is_resolved_before_remote_script_construction() {
        let target: SshTarget = "root@203.0.113.7".parse().expect("target");
        let mut command = init(&["init", target.as_str()]);
        assert_eq!(command.wireguard_endpoint, None);

        resolve_command_endpoint(&mut command)
            .await
            .expect("resolve IP target");
        let endpoint = "203.0.113.7:51820".parse().expect("endpoint");
        assert_eq!(command.wireguard_endpoint, Some(endpoint));

        let temp = tempfile::tempdir().expect("temp dir");
        let key =
            SshPeerKey::load_or_create(temp.path(), &target, "laptop".to_owned()).expect("key");
        assert_eq!(
            key.remote_script(&command)
                .matches("'--wireguard-endpoint' '203.0.113.7:51820'")
                .count(),
            1
        );
    }

    #[test]
    fn single_hostname_resolution_selects_the_endpoint() {
        let target: SshTarget = "root@machine.example".parse().expect("target");
        let endpoint = "203.0.113.7:51820".parse().expect("endpoint");
        assert_eq!(
            choose_resolved_endpoint(target, vec![endpoint]).expect("single endpoint"),
            endpoint
        );
    }

    #[test]
    fn ambiguous_hostname_resolution_names_the_explicit_endpoint_repair() {
        let target: SshTarget = "root@machine.example".parse().expect("target");
        let error = choose_resolved_endpoint(
            target.clone(),
            vec![
                "203.0.113.8:51820".parse().expect("first endpoint"),
                "203.0.113.7:51820".parse().expect("second endpoint"),
            ],
        )
        .expect_err("multiple endpoints require an explicit choice");
        assert!(matches!(
            &error,
            SshInitError::EndpointResolutionAmbiguous { .. }
        ));
        assert!(
            error
                .to_string()
                .contains("ployz init root@machine.example --wireguard-endpoint <ip>:51820")
        );
    }
}
