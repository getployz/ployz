use std::env;
use std::process::ExitCode;

use mvp_node::{NodeError, NodeResult};

mod cli;

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(1)
        }
    }
}

fn run(args: Vec<String>) -> NodeResult<String> {
    let [command, rest @ ..] = args.as_slice() else {
        return Ok(help());
    };
    match command.as_str() {
        "init" => cli::bootstrap::init(rest),
        "bootstrap" => cli::bootstrap::bootstrap(rest),
        "status" => cli::bootstrap::status(rest),
        "invite" => cli::membership::invite(rest),
        "join" => cli::membership::join(rest),
        "admission" => cli::membership::admission(rest),
        "admit" => cli::membership::admit(rest),
        "daemon" => cli::daemon::daemon(rest),
        "daemon-status" => cli::control::daemon_status(rest),
        "deploy" => cli::deploy::deploy(rest),
        "deploy-status" => cli::deploy::deploy_status(rest),
        "acme-issue" => cli::serving::acme_issue(rest),
        "runtime-http" => cli::serving::runtime_http(rest),
        "gateway" => cli::serving::gateway(rest),
        "dns" => cli::serving::dns(rest),
        "--help" | "-h" | "help" => Ok(help()),
        other => Err(NodeError::UnsupportedCommand {
            command: other.to_string(),
        }),
    }
}

fn help() -> String {
    [
        "mvp-node <command> [options]",
        "",
        "Commands:",
        "  init --state <dir> [--island <id>] [--node-id <id>]",
        "  bootstrap --state <dir> [--island <id>] [--node-id <id>]",
        "  status --state <dir>",
        "  daemon --state <dir> [--run-for-ms <ms>] [--linux-wireguard-ifname <ifname>] [--linux-wireguard-listen-port <port>] [--runtime process|docker --image <ref> [--service-port <port>] [--container-command <cmd>]]",
        "  invite --state <dir> [--ttl-ms <ms>]",
        "  join --state <dir> --token <json> [--node-id <id>]",
        "  admission --state <dir>",
        "  admit --state <dir> --request <json>",
        "  deploy (--state <dir> | --control <socket>) --target-node <id> [--deploy-id <id>] [--service <name>] [--revision <rev>] [--hostname <name>] [--runtime process|docker --image <ref> [--service-port <port>] [--container-command <cmd>]",
        "  deploy-status --state <dir> [--deploy-id <id>]",
        "  acme-issue --state <dir> --hostname <host> --gateway <url> [--gateway-control <socket>] [--issuer-holder <id>] [--account-path <path>]",
        "  gateway --state <dir> --listen <addr> --control <socket> [--tls-listen <addr>] [--stale-after-ms <ms>]",
        "  dns --state <dir> --listen <addr> --control <socket> [--stale-after-ms <ms>]",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::cli::bootstrap::ParsedArgs;
    use super::cli::daemon::DaemonArgs;
    use super::cli::runtime::DeployRuntimeArgs;
    use super::run;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    #[test]
    fn init_and_status_round_trip_through_cli_surface() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("node-a");

        let init = run(vec![
            "init".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("init");
        let status = run(vec![
            "status".to_string(),
            "--state".to_string(),
            state.display().to_string(),
        ])
        .expect("status");

        assert!(init.contains("initialized node=node-a island=prod"));
        assert!(status.contains("node=node-a island=prod principal=node:node-a"));
    }

    #[test]
    fn daemon_args_accept_docker_runtime_surface() {
        let parsed = DaemonArgs::parse(&[
            "--state".to_string(),
            "node-a".to_string(),
            "--run-for-ms".to_string(),
            "60000".to_string(),
            "--control".to_string(),
            "daemon.sock".to_string(),
            "--linux-wireguard-ifname".to_string(),
            "ployz-node-a".to_string(),
            "--linux-wireguard-listen-port".to_string(),
            "51821".to_string(),
            "--runtime".to_string(),
            "docker".to_string(),
            "--image".to_string(),
            "busybox:latest".to_string(),
            "--service-port".to_string(),
            "8080".to_string(),
            "--container-command".to_string(),
            "mkdir -p /www && httpd -f -p 8080 -h /www".to_string(),
        ])
        .expect("parse daemon args");

        assert_eq!(parsed.state_dir, std::path::PathBuf::from("node-a"));
        assert_eq!(parsed.run_for_ms, Some(60_000));
        assert_eq!(
            parsed.linux_wireguard_ifname.as_deref(),
            Some("ployz-node-a")
        );
        assert_eq!(parsed.linux_wireguard_listen_port, Some(51821));
        match parsed.runtime {
            DeployRuntimeArgs::Docker {
                image,
                service_port,
                command,
            } => {
                assert_eq!(image, "busybox:latest");
                assert_eq!(service_port, 8080);
                assert_eq!(
                    command,
                    Some(vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "mkdir -p /www && httpd -f -p 8080 -h /www".to_string()
                    ])
                );
            }
            DeployRuntimeArgs::Process => panic!("expected docker runtime"),
        }
    }

    #[test]
    fn bootstrap_is_idempotent_and_reports_product_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("node-a");

        let first = run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("bootstrap");
        let second = run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("bootstrap again");
        let first: serde_json::Value = serde_json::from_str(&first).expect("first json");
        let second: serde_json::Value = serde_json::from_str(&second).expect("second json");

        assert_eq!(first["status"], "bootstrapped");
        assert_eq!(first["node_id"], "node-a");
        assert_eq!(first["island"], "prod");
        assert_eq!(first["identity"], second["identity"]);
        assert!(state.join("runtime").is_dir());
        assert!(state.join("control").is_dir());
        assert!(state.join("wireguard").join("private.key").exists());
        assert_eq!(
            first["role_defaults"]["gateway_control_socket"],
            serde_json::Value::String(
                state
                    .join("control")
                    .join("gateway.sock")
                    .display()
                    .to_string()
            )
        );
    }

    #[test]
    fn bootstrap_refuses_conflicting_existing_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = temp.path().join("node-a");
        run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("bootstrap");

        let error = run(vec![
            "bootstrap".to_string(),
            "--state".to_string(),
            state.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-b".to_string(),
        ])
        .expect_err("conflicting node id fails");

        assert!(error.to_string().contains("existing node has 'node-a'"));
    }

    #[test]
    fn invite_join_admission_and_admit_round_trip_through_cli_surface() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        run(vec![
            "init".to_string(),
            "--state".to_string(),
            node_a.display().to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("init node a");

        let invite = run(vec![
            "invite".to_string(),
            "--state".to_string(),
            node_a.display().to_string(),
        ])
        .expect("invite");
        let joined = run(vec![
            "join".to_string(),
            "--state".to_string(),
            node_b.display().to_string(),
            "--token".to_string(),
            invite,
            "--node-id".to_string(),
            "node-b".to_string(),
        ])
        .expect("join node b");
        let admission = run(vec![
            "admission".to_string(),
            "--state".to_string(),
            node_b.display().to_string(),
        ])
        .expect("admission");
        let admitted = run(vec![
            "admit".to_string(),
            "--state".to_string(),
            node_a.display().to_string(),
            "--request".to_string(),
            admission,
        ])
        .expect("admit node b");

        assert!(joined.contains("joined node=node-b island=prod"));
        assert!(admitted.contains("admitted node=node-b principal=node:node-b"));
    }

    #[test]
    fn parses_state_island_and_node_id_flags() {
        let parsed = ParsedArgs::parse(&[
            "--state".to_string(),
            "/tmp/node".to_string(),
            "--island".to_string(),
            "prod".to_string(),
            "--node-id".to_string(),
            "node-a".to_string(),
        ])
        .expect("parse args");

        assert_eq!(
            parsed.state_dir.expect("state").display().to_string(),
            "/tmp/node"
        );
        assert_eq!(parsed.island.as_deref(), Some("prod"));
        assert_eq!(parsed.node_id.as_deref(), Some("node-a"));
    }

    #[test]
    fn status_rejects_init_only_flags() {
        let error = run(vec![
            "status".to_string(),
            "--state".to_string(),
            "/tmp/node".to_string(),
            "--island".to_string(),
            "prod".to_string(),
        ])
        .expect_err("status rejects island");

        assert!(matches!(
            error,
            mvp_node::NodeError::UnknownArgument { argument } if argument == "--island"
        ));
    }

    #[test]
    fn deploy_control_rejects_non_deploy_daemon_response() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp.path().join("daemon.sock");
        let listener = UnixListener::bind(socket.as_path()).expect("bind listener");
        let server = thread::spawn(move || {
            let (mut stream, _addr) = listener.accept().expect("accept");
            let mut request = Vec::new();
            stream.read_to_end(&mut request).expect("read request");
            assert!(!request.is_empty());
            stream
                .write_all(br#"{"status":"ready","node":"founder"}"#)
                .expect("write response");
        });

        let error = run(vec![
            "deploy".to_string(),
            "--control".to_string(),
            socket.display().to_string(),
            "--target-node".to_string(),
            "peer-a".to_string(),
        ])
        .expect_err("ready is not deployed");
        server.join().expect("server thread");

        assert!(matches!(
            error,
            mvp_node::NodeError::NodeAgentRpc { message }
                if message.contains("unexpected status 'ready'")
        ));
    }
}
