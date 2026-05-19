use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use mvp_node::{InitOptions, NodeError, NodeResult, init_node, load_node};

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
        "init" => init(rest),
        "status" => status(rest),
        "invite" | "join" | "daemon" | "gateway" | "dns" | "deploy" => {
            Err(NodeError::CommandNotWired {
                command: command.clone(),
            })
        }
        "--help" | "-h" | "help" => Ok(help()),
        other => Err(NodeError::UnsupportedCommand {
            command: other.to_string(),
        }),
    }
}

fn init(args: &[String]) -> NodeResult<String> {
    let parsed = ParsedArgs::parse(args)?;
    let Some(state_dir) = parsed.state_dir else {
        return Err(NodeError::MissingFlagValue { flag: "--state" });
    };
    let mut options = InitOptions::new(state_dir);
    if let Some(island) = parsed.island {
        options = options.with_island(island);
    }
    if let Some(node_id) = parsed.node_id {
        options = options.with_node_id(node_id);
    }
    let state = init_node(options)?;
    Ok(format!(
        "initialized node={} island={} state={}",
        state.node_id_str(),
        state.island_id(),
        state.paths().state_dir.display()
    ))
}

fn status(args: &[String]) -> NodeResult<String> {
    let state_dir = parse_state_dir_only(args)?;
    let state = load_node(state_dir)?;
    Ok(format!(
        "node={} island={} principal={} facts={} projection={} gateway_snapshot={} dns_snapshot={}",
        state.node_id_str(),
        state.island_id(),
        state.principal_id(),
        state.paths().fact_store.display(),
        state.paths().projection_db.display(),
        state.paths().gateway_snapshot.display(),
        state.paths().dns_snapshot.display()
    ))
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedArgs {
    state_dir: Option<PathBuf>,
    island: Option<String>,
    node_id: Option<String>,
}

impl ParsedArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut parsed = Self {
            state_dir: None,
            island: None,
            node_id: None,
        };
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    parsed.state_dir = Some(PathBuf::from(value));
                }
                "--island" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--island" });
                    };
                    parsed.island = Some(value.clone());
                }
                "--node-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--node-id" });
                    };
                    parsed.node_id = Some(value.clone());
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(parsed)
    }
}

fn parse_state_dir_only(args: &[String]) -> NodeResult<PathBuf> {
    let mut state_dir = None;
    let mut remaining = args.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--state" => {
                let Some(value) = remaining.next() else {
                    return Err(NodeError::MissingFlagValue { flag: "--state" });
                };
                state_dir = Some(PathBuf::from(value));
            }
            other => {
                return Err(NodeError::UnknownArgument {
                    argument: other.to_string(),
                });
            }
        }
    }
    state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })
}

fn help() -> String {
    [
        "mvp-node <command> [options]",
        "",
        "Commands:",
        "  init --state <dir> [--island <id>] [--node-id <id>]",
        "  status --state <dir>",
        "  invite|join|daemon|gateway|dns|deploy  (planned product-vertical commands)",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::{ParsedArgs, run};

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
}
