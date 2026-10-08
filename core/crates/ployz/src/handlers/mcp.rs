//! `ployz mcp`: the Cloud commands as MCP tools over stdio. Each call runs as a child
//! `ployz <command> --json`, so stdout carries nothing but JSON-RPC frames.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use clap::parser::ValueSource;
use clap::{ArgMatches, Command};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use rustix::process::{Pid, Signal, kill_process_group, setsid};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::signal::unix::{SignalKind, signal};

use super::catalog::{self, Approval, ArgEntry, ArgType, CommandEntry, Stdin, Surface};
use super::{Error, leaf_matches};
use crate::failure::Failure;

pub(crate) fn command() -> Command {
    Command::new("mcp").about("Serve the Cloud commands to a coding agent as MCP tools over stdio")
}

const OUTPUT_LIMIT: usize = 1 << 20;

/// Outlasts a Server install over SSH (up to 20 minutes) and matches the default build
/// limit. A Deployment or Volume run goes on in Cloud after the call's child is killed.
const CALL_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// Adding a Server installs Ployz over SSH or on this computer. A tool call cannot own that
/// SSH session or answer its prompts, so it is run from a terminal.
const TERMINAL_ONLY: [&str; 1] = ["server add"];

const CONNECTION_FLAGS: [&str; 3] = ["connect", "ssh-timeout", "ployz-config"];

pub(super) fn serve(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let globals = CONNECTION_FLAGS
        .into_iter()
        .filter(|id| matches.value_source(id) == Some(ValueSource::CommandLine))
        .flat_map(|id| {
            matches
                .get_raw(id)
                .into_iter()
                .flatten()
                .map(move |value| format!("--{id}={}", value.to_string_lossy()))
        })
        .collect();
    let server = Server::new(std::env::current_exe()?, globals);
    // With no controlling terminal, SSH that wants a password fails at once instead of
    // prompting on the user's terminal. A server that already leads a process group keeps its
    // terminal, and its calls still run in background groups that cannot read it.
    let _ = setsid();
    let runtime = super::runtime()?;
    let served = runtime.block_on(async {
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut hangup = signal(SignalKind::hangup())?;
        let running = server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(Failure::command)?;
        tokio::select! {
            quit = running.waiting() => {
                quit.map_err(Failure::command)?;
            }
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            _ = hangup.recv() => {}
        }
        Ok(())
    });
    // After a signal, a thread is still blocked reading stdin; exit without waiting for it.
    runtime.shutdown_background();
    served
}

struct Server {
    exe: PathBuf,
    globals: Vec<String>,
    deadline: Duration,
    commands: Vec<CommandEntry>,
    tools: Vec<Tool>,
}

impl Server {
    fn new(exe: PathBuf, globals: Vec<String>) -> Self {
        let commands: Vec<CommandEntry> = catalog::commands()
            .into_iter()
            .filter(|entry| {
                exposed(entry.surface)
                    && entry.json
                    && !entry.keeps_running
                    && !TERMINAL_ONLY.contains(&entry.command.as_str())
            })
            .collect();
        let tools = commands.iter().map(tool).collect();
        Self {
            exe,
            globals,
            deadline: CALL_DEADLINE,
            commands,
            tools,
        }
    }

    fn entry(&self, name: &str) -> Option<&CommandEntry> {
        self.commands
            .iter()
            .find(|entry| tool_name(&entry.command) == name)
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ployz", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!(
                "Each tool runs one `ployz` command with --json and returns its JSON result. \
                 Tools marked destructive can remove live things. A call still running after \
                 {} is stopped; its error names the tool that shows what it left running.",
                span(self.deadline)
            ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let entry = self.entry(&request.name).ok_or_else(|| {
            McpError::invalid_params(format!("no tool named {}", request.name), None)
        })?;
        let argv = argv(entry, &self.globals, request.arguments.unwrap_or_default())?;
        let mut child = KillGroupOnDrop(
            tokio::process::Command::new(&self.exe)
                .args(&argv)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| McpError::internal_error(error.to_string(), None))?,
        );
        let (stdout, stderr) = (child.0.stdout.take(), child.0.stderr.take());
        let finished = async { tokio::join!(child.0.wait(), capture(stdout), capture(stderr)) };
        let (status, stdout, stderr) = tokio::select! {
            (status, stdout, stderr) = finished => (
                status.map_err(|error| McpError::internal_error(error.to_string(), None))?,
                stdout,
                stderr,
            ),
            () = context.ct.cancelled() => {
                return Err(McpError::internal_error("the call was cancelled", None));
            }
            () = tokio::time::sleep(self.deadline) => {
                let content = vec![ContentBlock::text(overdue(entry, self.deadline))];
                return Ok(CallToolResult::error(content).into());
            }
        };
        let text = if stdout.is_empty() { stderr } else { stdout };
        let content = vec![ContentBlock::text(text)];
        Ok(if status.success() {
            CallToolResult::success(content)
        } else {
            CallToolResult::error(content)
        }
        .into())
    }
}

struct KillGroupOnDrop(tokio::process::Child);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        if let Some(group) = self
            .0
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(Pid::from_raw)
        {
            let _ = kill_process_group(group, Signal::KILL);
        }
    }
}

async fn capture(stream: Option<impl AsyncRead + Unpin>) -> String {
    let Some(mut stream) = stream else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    let mut buffer = [0u8; 8192];
    while let Ok(read @ 1..) = stream.read(&mut buffer).await {
        let chunk = buffer.get(..read).unwrap_or_default();
        let room = OUTPUT_LIMIT.saturating_sub(kept.len()).min(read);
        kept.extend_from_slice(chunk.get(..room).unwrap_or_default());
        dropped += read - room;
    }
    let mut text = String::from_utf8_lossy(&kept).into_owned();
    if dropped > 0 {
        text.push_str(&format!(
            "\n[ployz mcp: output truncated; {dropped} more bytes after the first {OUTPUT_LIMIT}]"
        ));
    }
    text
}

fn overdue(entry: &CommandEntry, deadline: Duration) -> String {
    let status = match entry.command.split(' ').next() {
        Some("server") => "server ls",
        Some("volume") => "volume runs",
        _ => "status",
    };
    format!(
        "ployz mcp stopped `ployz {}` after {}. What it started may still be running; \
         check with the `{}` tool (`ployz {status}`).",
        entry.command,
        span(deadline),
        tool_name(status),
    )
}

fn span(deadline: Duration) -> String {
    match deadline.as_secs() {
        secs if secs % 60 == 0 => format!("{} minutes", secs / 60),
        secs => format!("{secs} seconds"),
    }
}

fn exposed(surface: Surface) -> bool {
    match surface {
        Surface::Cloud => true,
        Surface::Local | Surface::Internal => false,
    }
}

fn destructive(approval: Approval) -> bool {
    match approval {
        Approval::Never => false,
        Approval::Always | Approval::Depends => true,
    }
}

fn offered(arg: &ArgEntry) -> bool {
    !arg.keeps_running && arg.stdin != Some(Stdin::Only)
}

fn tool_name(command: &str) -> String {
    command.replace(' ', "_")
}

/// The argument's key in a tool call: a flag's long name, or a positional's name.
fn property(arg: &ArgEntry) -> String {
    arg.name
        .strip_prefix("--")
        .map_or_else(|| arg.name.to_lowercase(), str::to_owned)
}

fn tool(entry: &CommandEntry) -> Tool {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for arg in entry.args.iter().filter(|arg| offered(arg)) {
        let key = property(arg);
        if arg.required {
            required.push(Value::String(key.clone()));
        }
        let conflicts: Vec<&str> = arg
            .conflicts
            .iter()
            .filter(|name| {
                entry
                    .args
                    .iter()
                    .any(|other| other.name == **name && offered(other))
            })
            .map(String::as_str)
            .collect();
        properties.insert(key, arg_schema(arg, &conflicts));
    }
    let schema: JsonObject = json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
    .as_object()
    .cloned()
    .unwrap_or_default();
    Tool::new(
        tool_name(&entry.command),
        entry.about.clone(),
        Arc::new(schema),
    )
    .annotate(ToolAnnotations::new().destructive(destructive(entry.approval)))
}

fn arg_schema(arg: &ArgEntry, conflicts: &[&str]) -> Value {
    let mut item = Map::new();
    item.insert("type".into(), json!(arg.kind));
    if arg.kind != ArgType::Boolean && !arg.values.is_empty() {
        item.insert("enum".into(), json!(arg.values));
    }
    let mut schema = if arg.multiple || arg.trailing {
        let mut array = Map::new();
        array.insert("type".into(), json!("array"));
        array.insert("items".into(), Value::Object(item));
        array
    } else {
        item
    };
    let mut sentences: Vec<String> = arg
        .help
        .iter()
        .map(|help| help.trim_end_matches('.').to_owned())
        .collect();
    if arg.stdin == Some(Stdin::OnDash) {
        sentences.push("`-` for stdin is refused".to_owned());
    }
    if !conflicts.is_empty() {
        let others: Vec<String> = conflicts.iter().map(|name| format!("`{name}`")).collect();
        sentences.push(format!("Cannot be used with {}", others.join(", ")));
    }
    if !sentences.is_empty() {
        schema.insert(
            "description".into(),
            json!(format!("{}.", sentences.join(". "))),
        );
    }
    if let (Some(default), false) = (&arg.default, arg.multiple) {
        let default = match arg.kind {
            ArgType::Integer => default
                .parse::<i64>()
                .map_or_else(|_| json!(default), |n| json!(n)),
            ArgType::Boolean => default
                .parse::<bool>()
                .map_or_else(|_| json!(default), |b| json!(b)),
            ArgType::String => json!(default),
        };
        schema.insert("default".into(), default);
    }
    Value::Object(schema)
}

/// `ployz <command> --flag=value… --json -- POSITIONAL…`, checked against the tool's schema.
fn argv(
    entry: &CommandEntry,
    globals: &[String],
    arguments: JsonObject,
) -> Result<Vec<String>, McpError> {
    let mut argv: Vec<String> = entry.command.split(' ').map(str::to_owned).collect();
    argv.extend_from_slice(globals);
    let mut positionals: Vec<(usize, Vec<String>)> = Vec::new();
    for (key, value) in arguments {
        let arg = entry
            .args
            .iter()
            .find(|arg| offered(arg) && property(arg) == key)
            .ok_or_else(|| {
                McpError::invalid_params(
                    format!("`{}` takes no argument `{key}`", tool_name(&entry.command)),
                    None,
                )
            })?;
        let values = words(arg, &key, value)?;
        if values.is_empty() {
            continue;
        }
        if arg.stdin == Some(Stdin::OnDash) && values.iter().any(|word| word == "-") {
            return Err(McpError::invalid_params(
                format!("`{key}` cannot read stdin here; give its value instead of `-`"),
                None,
            ));
        }
        match arg.index {
            Some(index) => positionals.push((index, values)),
            None if !arg.value => {
                if values.iter().any(|word| word == "true") {
                    argv.push(format!("--{key}"));
                }
            }
            None => argv.extend(values.into_iter().map(|word| format!("--{key}={word}"))),
        }
    }
    if let Some(missing) = entry
        .args
        .iter()
        .find(|arg| arg.required && !arg_given(arg, &argv, &positionals))
    {
        return Err(McpError::invalid_params(
            format!(
                "`{}` needs `{}`",
                tool_name(&entry.command),
                property(missing)
            ),
            None,
        ));
    }
    argv.push("--json".to_owned());
    positionals.sort_by_key(|(index, _)| *index);
    if !positionals.is_empty() {
        argv.push("--".to_owned());
        argv.extend(positionals.into_iter().flat_map(|(_, words)| words));
    }
    Ok(argv)
}

fn arg_given(arg: &ArgEntry, argv: &[String], positionals: &[(usize, Vec<String>)]) -> bool {
    match arg.index {
        Some(index) => positionals.iter().any(|(given, _)| *given == index),
        None => {
            let flag = format!("--{}", property(arg));
            argv.iter()
                .any(|word| word == &flag || word.starts_with(&format!("{flag}=")))
        }
    }
}

/// One argument's value as command-line words; null is the same as leaving it out.
fn words(arg: &ArgEntry, key: &str, value: Value) -> Result<Vec<String>, McpError> {
    let many = arg.multiple || arg.trailing;
    let wrong = || {
        let kind = match arg.kind {
            ArgType::Boolean => "boolean",
            ArgType::Integer => "integer",
            ArgType::String => "string",
        };
        let shape = if many {
            format!("an array of {kind}s")
        } else {
            format!("one {kind}")
        };
        McpError::invalid_params(format!("`{key}` takes {shape}"), None)
    };
    let scalar = |value: Value| {
        match arg.kind {
            ArgType::Boolean => value.as_bool().map(|flag| flag.to_string()),
            ArgType::Integer => (value.is_i64() || value.is_u64()).then(|| value.to_string()),
            ArgType::String => value.as_str().map(str::to_owned),
        }
        .ok_or_else(wrong)
        .and_then(|word| {
            if word.contains('\0') {
                Err(McpError::invalid_params(
                    format!("`{key}` cannot hold a NUL byte"),
                    None,
                ))
            } else {
                Ok(word)
            }
        })
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    match (many, value) {
        (true, Value::Array(items)) => items.into_iter().map(scalar).collect(),
        (true, _) => Err(wrong()),
        (false, value) => Ok(vec![scalar(value)?]),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use super::*;

    async fn exchange(frames: &[Value]) -> Vec<Value> {
        exchange_with(
            Server::new(PathBuf::from("/nonexistent/ployz"), Vec::new()),
            frames,
        )
        .await
    }

    async fn exchange_with(server: Server, frames: &[Value]) -> Vec<Value> {
        let mut client = Client::connect(server);
        let mut replies = Vec::new();
        for frame in frames {
            client.send(frame).await;
            if frame.get("id").is_some() {
                replies.push(client.reply().await);
            }
        }
        replies
    }

    struct Client {
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        server: tokio::task::JoinHandle<()>,
    }

    impl Client {
        fn connect(server: Server) -> Self {
            let (server_io, client_io) = tokio::io::duplex(1 << 20);
            let server = tokio::spawn(async move {
                server
                    .serve(tokio::io::split(server_io))
                    .await
                    .expect("the handshake completes")
                    .waiting()
                    .await
                    .expect("the server stops cleanly");
            });
            let (read, write) = tokio::io::split(client_io);
            Self {
                lines: BufReader::new(read).lines(),
                write,
                server,
            }
        }

        async fn send(&mut self, frame: &Value) {
            self.write
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
        }

        async fn reply(&mut self) -> Value {
            let line = self.lines.next_line().await.unwrap().expect("a reply");
            serde_json::from_str(&line).expect("a JSON-RPC frame")
        }
    }

    impl Drop for Client {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    fn request(id: u64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn handshake() -> Vec<Value> {
        vec![
            request(
                1,
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "0" },
                }),
            ),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        ]
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn tools_list_serves_the_cloud_commands_with_their_destructive_hint() {
        let mut frames = handshake();
        frames.push(request(2, "tools/list", json!({})));
        let replies = exchange(&frames).await;
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "ployz");
        let tools = replies[1]["result"]["tools"].as_array().unwrap();
        let tool = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("{name} is listed"))
        };
        assert_eq!(tool("deploy")["annotations"]["destructiveHint"], true);
        assert_eq!(tool("server_rm")["annotations"]["destructiveHint"], true);
        assert_eq!(tool("project_ls")["annotations"]["destructiveHint"], false);
        assert_eq!(tool("service_rm")["annotations"]["destructiveHint"], false);
        for absent in [
            "ctx",
            "ctx_use",
            "login",
            "completion",
            "exec",
            "build",
            "mcp",
            "service_port-forward",
            "server_add",
        ] {
            assert!(
                tools.iter().all(|tool| tool["name"] != absent),
                "{absent} is not a tool"
            );
        }
        for (name, absent) in [
            ("logs", "follow"),
            ("server_logs", "follow"),
            ("github_connect", "wait"),
            ("set", "secret"),
            ("set", "at-merge"),
            ("env_sync", "value"),
            ("volume_sync", "wait"),
            ("volume_mirror", "wait"),
            ("volume_mirror_rm", "wait"),
        ] {
            let properties = &tool(name)["inputSchema"]["properties"];
            assert!(properties.get(absent).is_none(), "{name} offers {absent}");
        }
        assert!(
            tool("set")["inputSchema"]["properties"]["patch"]["description"]
                .as_str()
                .unwrap()
                .ends_with(
                    "- reads stdin. `-` for stdin is refused. Cannot be used with `--from-env-file`."
                ),
        );
        let no_reset = &tool("server_rm")["inputSchema"]["properties"]["no-reset"];
        assert_eq!(no_reset["type"], "boolean");
        assert!(
            no_reset["description"]
                .as_str()
                .unwrap()
                .ends_with("unreachable. Cannot be used with `--accept-volume-loss`."),
            "{no_reset}"
        );
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_bad_argument_is_an_mcp_error_and_the_server_keeps_serving() {
        let mut frames = handshake();
        frames.push(request(
            2,
            "tools/call",
            json!({ "name": "service_add", "arguments": { "bogus": 1 } }),
        ));
        frames.push(request(
            3,
            "tools/call",
            json!({ "name": "server_rm", "arguments": { "no-reset": "yes" } }),
        ));
        frames.push(request(
            4,
            "tools/call",
            json!({ "name": "service_add", "arguments": { "name": null } }),
        ));
        frames.push(request(5, "tools/list", json!({})));
        let replies = exchange(&frames).await;
        assert_eq!(replies[1]["error"]["code"], -32602, "{}", replies[1]);
        assert_eq!(replies[2]["error"]["code"], -32602, "{}", replies[2]);
        assert_eq!(
            replies[3]["error"]["message"], "`service_add` needs `name`",
            "{}",
            replies[3]
        );
        assert!(replies[4]["result"]["tools"].is_array(), "{}", replies[4]);
    }

    #[test]
    fn exec_is_a_cloud_command_but_no_tool_because_it_refuses_json() {
        let exec = catalog::commands()
            .into_iter()
            .find(|entry| entry.command == "exec")
            .unwrap();
        assert_eq!(exec.surface, Surface::Cloud);
        assert!(!exec.json);
        let server = Server::new(PathBuf::from("/nonexistent/ployz"), Vec::new());
        assert!(server.entry("exec").is_none());
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn arguments_that_read_stdin_or_keep_running_are_refused() {
        let mut frames = handshake();
        let calls = [
            json!({ "name": "set", "arguments": { "assignment": ["web.env.KEY"], "secret": true } }),
            json!({ "name": "logs", "arguments": { "follow": true } }),
            json!({ "name": "set", "arguments": { "assignment": ["web"], "patch": "-" } }),
            json!({ "name": "set", "arguments": { "assignment": ["web"], "from-env-file": "-" } }),
            json!({ "name": "service_port-forward", "arguments": {} }),
            json!({ "name": "server_rm", "arguments": { "server": "a\0b" } }),
            json!({ "name": "server_add", "arguments": { "command": true } }),
        ];
        for (id, call) in (2..).zip(calls) {
            frames.push(request(id, "tools/call", call));
        }
        let replies = exchange(&frames).await;
        for reply in &replies[1..] {
            assert_eq!(reply["error"]["code"], -32602, "{reply}");
        }
        assert_eq!(
            replies[3]["error"]["message"],
            "`patch` cannot read stdin here; give its value instead of `-`"
        );
        assert_eq!(
            replies[6]["error"]["message"],
            "`server` cannot hold a NUL byte"
        );
        assert_eq!(replies[7]["error"]["message"], "no tool named server_add");
    }

    fn fake_ployz(dir: &std::path::Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("ployz");
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             case \"$1\" in\n\
             server) echo \"no such server\" >&2; exit 3 ;;\n\
             logs) head -c 3000000 /dev/zero | tr '\\0' a; exit 0 ;;\n\
             deployment) sleep 60 & echo $$ $! > \"$(dirname \"$0\")/pids\"; wait ;;\n\
             esac\n\
             echo \"$*\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_call_runs_the_command_and_a_failure_is_a_tool_error() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::new(fake_ployz(dir.path()), vec!["--connect=ssh://a".to_owned()]);
        let mut frames = handshake();
        frames.push(request(
            2,
            "tools/call",
            json!({ "name": "project_ls", "arguments": {} }),
        ));
        frames.push(request(
            3,
            "tools/call",
            json!({ "name": "server_rm", "arguments": { "server": "web-1" } }),
        ));
        frames.push(request(
            4,
            "tools/call",
            json!({ "name": "logs", "arguments": {} }),
        ));
        let replies = exchange_with(server, &frames).await;
        let ok = &replies[1]["result"];
        assert_eq!(ok["isError"], false, "{ok}");
        assert_eq!(
            ok["content"][0]["text"],
            "project ls --connect=ssh://a --json\n"
        );
        let failed = &replies[2]["result"];
        assert_eq!(failed["isError"], true, "{failed}");
        assert_eq!(failed["content"][0]["text"], "no such server\n");
        let flooded = replies[3]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            flooded.ends_with(&format!(
                "\n[ployz mcp: output truncated; {} more bytes after the first {OUTPUT_LIMIT}]",
                3_000_000 - OUTPUT_LIMIT
            )),
            "{}",
            flooded.get(flooded.len() - 120..).unwrap_or_default()
        );
        assert_eq!(flooded.find('\n'), Some(OUTPUT_LIMIT));
    }

    async fn call_pids(dir: &std::path::Path) -> Vec<String> {
        let path = dir.join("pids");
        for _ in 0..500 {
            if let Ok(pids) = std::fs::read_to_string(&path)
                && pids.ends_with('\n')
            {
                return pids.split_whitespace().map(str::to_owned).collect();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the child never started");
    }

    async fn assert_killed(pids: &[String]) {
        for pid in pids {
            assert_gone(pid).await;
        }
    }

    async fn assert_gone(pid: &str) {
        for _ in 0..500 {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => return,
                Ok(stat)
                    if stat
                        .rsplit(") ")
                        .next()
                        .is_some_and(|rest| rest.starts_with('Z')) =>
                {
                    return;
                }
                Ok(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        panic!("process {pid} is still running");
    }

    fn deployment_start() -> Value {
        request(
            2,
            "tools/call",
            json!({ "name": "deployment_start", "arguments": { "id": "dep_1" } }),
        )
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_call_past_the_deadline_is_killed_with_its_children_and_names_where_to_check() {
        let dir = tempfile::tempdir().unwrap();
        let mut server = Server::new(fake_ployz(dir.path()), Vec::new());
        server.deadline = Duration::from_secs(2);
        let mut frames = handshake();
        frames.push(deployment_start());
        let replies = exchange_with(server, &frames).await;
        let overdue = &replies[1]["result"];
        assert_eq!(overdue["isError"], true, "{overdue}");
        assert_eq!(
            overdue["content"][0]["text"],
            "ployz mcp stopped `ployz deployment start` after 2 seconds. What it started may still \
             be running; check with the `status` tool (`ployz status`)."
        );
        assert_killed(&call_pids(dir.path()).await).await;
    }

    #[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
    #[tokio::test]
    async fn a_cancelled_call_kills_its_children_and_the_server_keeps_serving() {
        let dir = tempfile::tempdir().unwrap();
        let mut client = Client::connect(Server::new(fake_ployz(dir.path()), Vec::new()));
        for frame in handshake() {
            client.send(&frame).await;
        }
        client.reply().await;
        client.send(&deployment_start()).await;
        let pids = call_pids(dir.path()).await;
        client
            .send(&json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": { "requestId": 2 },
            }))
            .await;
        assert_killed(&pids).await;
        client.send(&request(3, "tools/list", json!({}))).await;
        let listed = client.reply().await;
        assert_eq!(listed["id"], 3, "{listed}");
        assert!(listed["result"]["tools"].is_array(), "{listed}");
    }

    #[test]
    fn the_deadline_error_reads_in_minutes() {
        let entry = catalog::commands()
            .into_iter()
            .find(|entry| entry.command == "server drain")
            .unwrap();
        assert_eq!(
            overdue(&entry, CALL_DEADLINE),
            "ployz mcp stopped `ployz server drain` after 30 minutes. What it started may still \
             be running; check with the `server_ls` tool (`ployz server ls`)."
        );
    }

    #[test]
    fn a_call_becomes_flags_then_json_then_positionals() {
        let entry = catalog::commands()
            .into_iter()
            .find(|entry| entry.command == "server rm")
            .unwrap();
        let arguments = json!({ "server": "web-1", "no-reset": true, "confirm": null });
        let argv = argv(&entry, &[], arguments.as_object().cloned().unwrap()).unwrap();
        assert_eq!(
            argv,
            ["server", "rm", "--no-reset", "--json", "--", "web-1"]
        );
    }
}
