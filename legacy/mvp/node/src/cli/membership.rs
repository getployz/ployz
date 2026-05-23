use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use mvp_node::{
    NodeError, NodeResult, P2pandaEndpointConfig, admit_joiner, create_admission_request,
    create_invite, join_from_token_with_endpoint, now_ms,
};

use super::bootstrap::{parse_socket_addr, parse_state_dir_only};

pub(crate) fn admission(args: &[String]) -> NodeResult<String> {
    let state_dir = parse_state_dir_only(args)?;
    create_admission_request(state_dir)
}

pub(crate) fn admit(args: &[String]) -> NodeResult<String> {
    let parsed = AdmitArgs::parse(args)?;
    let report = admit_joiner(parsed.state_dir, &parsed.request, now_ms())?;
    Ok(format!(
        "admitted node={} principal={}",
        report.node_id, report.principal_id
    ))
}

pub(crate) fn invite(args: &[String]) -> NodeResult<String> {
    let parsed = InviteArgs::parse(args)?;
    let token = create_invite(
        parsed.state_dir,
        Duration::from_millis(parsed.ttl_ms.unwrap_or(600_000)),
    )?;
    Ok(token)
}

pub(crate) fn join(args: &[String]) -> NodeResult<String> {
    let parsed = JoinArgs::parse(args)?;
    let endpoint = parsed.p2panda_endpoint()?;
    let state = join_from_token_with_endpoint(
        parsed.state_dir,
        &parsed.token,
        parsed.node_id,
        endpoint,
        now_ms(),
    )?;
    Ok(format!(
        "joined node={} island={} state={}",
        state.node_id_str(),
        state.island_id(),
        state.paths().state_dir.display()
    ))
}

struct InviteArgs {
    state_dir: PathBuf,
    ttl_ms: Option<u64>,
}

struct JoinArgs {
    state_dir: PathBuf,
    token: String,
    node_id: Option<String>,
    p2panda_bind: Option<SocketAddr>,
    p2panda_advertise: Option<SocketAddr>,
}

struct AdmitArgs {
    state_dir: PathBuf,
    request: String,
}

impl InviteArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut ttl_ms = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--ttl-ms" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--ttl-ms" });
                    };
                    ttl_ms =
                        Some(
                            value
                                .parse::<u64>()
                                .map_err(|_| NodeError::UnknownArgument {
                                    argument: value.clone(),
                                })?,
                        );
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            state_dir: state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })?,
            ttl_ms,
        })
    }
}

impl JoinArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut token = None;
        let mut node_id = None;
        let mut p2panda_bind = None;
        let mut p2panda_advertise = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--token" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--token" });
                    };
                    token = Some(value.clone());
                }
                "--node-id" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--node-id" });
                    };
                    node_id = Some(value.clone());
                }
                "--p2panda-bind" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--p2panda-bind",
                        });
                    };
                    p2panda_bind = Some(parse_socket_addr("--p2panda-bind", value)?);
                }
                "--p2panda-advertise" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue {
                            flag: "--p2panda-advertise",
                        });
                    };
                    p2panda_advertise = Some(parse_socket_addr("--p2panda-advertise", value)?);
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            state_dir: state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })?,
            token: token.ok_or(NodeError::MissingFlagValue { flag: "--token" })?,
            node_id,
            p2panda_bind,
            p2panda_advertise,
        })
    }

    fn p2panda_endpoint(&self) -> NodeResult<Option<P2pandaEndpointConfig>> {
        match (self.p2panda_bind, self.p2panda_advertise) {
            (None, None) => Ok(None),
            (Some(bind), Some(advertise)) => Ok(Some(P2pandaEndpointConfig::new(bind, advertise))),
            (None, Some(_)) => Err(NodeError::MissingFlagValue {
                flag: "--p2panda-bind",
            }),
            (Some(_), None) => Err(NodeError::MissingFlagValue {
                flag: "--p2panda-advertise",
            }),
        }
    }
}

impl AdmitArgs {
    fn parse(args: &[String]) -> NodeResult<Self> {
        let mut state_dir = None;
        let mut request = None;
        let mut remaining = args.iter();
        while let Some(argument) = remaining.next() {
            match argument.as_str() {
                "--state" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--state" });
                    };
                    state_dir = Some(PathBuf::from(value));
                }
                "--request" => {
                    let Some(value) = remaining.next() else {
                        return Err(NodeError::MissingFlagValue { flag: "--request" });
                    };
                    request = Some(value.clone());
                }
                other => {
                    return Err(NodeError::UnknownArgument {
                        argument: other.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            state_dir: state_dir.ok_or(NodeError::MissingFlagValue { flag: "--state" })?,
            request: request.ok_or(NodeError::MissingFlagValue { flag: "--request" })?,
        })
    }
}
