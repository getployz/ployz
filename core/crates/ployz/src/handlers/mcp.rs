//! `ployz mcp`: the Cloud commands as MCP tools over stdio. Each call runs as a child
//! `ployz <command> --json`, so stdout carries nothing but JSON-RPC frames.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use clap::{ArgMatches, Command};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value, json};

use super::Error;
use super::catalog::{self, Approval, ArgEntry, ArgType, CommandEntry, Surface};
use crate::failure::Failure;

pub(crate) fn command() -> Command {
    Command::new("mcp").about("Serve the Cloud commands to a coding agent as MCP tools over stdio")
}

pub(super) fn serve(_root: &ArgMatches) -> Result<(), Error> {
    let server = Server::new(std::env::current_exe()?);
    super::runtime()?.block_on(async {
        server
            .serve(rmcp::transport::stdio())
            .await
            .map_err(Failure::command)?
            .waiting()
            .await
            .map_err(Failure::command)?;
        Ok(())
    })
}

struct Server {
    exe: PathBuf,
    commands: Vec<CommandEntry>,
    tools: Vec<Tool>,
}

impl Server {
    fn new(exe: PathBuf) -> Self {
        let commands: Vec<CommandEntry> = catalog::commands()
            .into_iter()
            .filter(|entry| exposed(entry.surface) && entry.json)
            .collect();
        let tools = commands.iter().map(tool).collect();
        Self {
            exe,
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
            .with_instructions(
                "Each tool runs one `ployz` command with --json and returns its JSON result. \
                 Tools marked destructive can remove live things.",
            )
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
        let argv = argv(entry, request.arguments.unwrap_or_default())?;
        let child = tokio::process::Command::new(&self.exe)
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let output = tokio::select! {
            output = child.wait_with_output() => output
                .map_err(|error| McpError::internal_error(error.to_string(), None))?,
            () = context.ct.cancelled() => {
                return Err(McpError::internal_error("the call was cancelled", None));
            }
        };
        let text = if output.stdout.is_empty() {
            output.stderr
        } else {
            output.stdout
        };
        let content = vec![ContentBlock::text(String::from_utf8_lossy(&text))];
        Ok(if output.status.success() {
            CallToolResult::success(content)
        } else {
            CallToolResult::error(content)
        }
        .into())
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
    for arg in &entry.args {
        let key = property(arg);
        if arg.required {
            required.push(Value::String(key.clone()));
        }
        properties.insert(key, arg_schema(arg));
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

fn arg_schema(arg: &ArgEntry) -> Value {
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
    let mut description = arg.help.clone().unwrap_or_default();
    if !arg.conflicts.is_empty() {
        let others: Vec<String> = arg
            .conflicts
            .iter()
            .map(|name| format!("`{name}`"))
            .collect();
        description = format!("{description} Cannot be used with {}.", others.join(", "))
            .trim_start()
            .to_owned();
    }
    if !description.is_empty() {
        schema.insert("description".into(), json!(description));
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
fn argv(entry: &CommandEntry, arguments: JsonObject) -> Result<Vec<String>, McpError> {
    let mut argv: Vec<String> = entry.command.split(' ').map(str::to_owned).collect();
    let mut positionals: Vec<(usize, Vec<String>)> = Vec::new();
    for (key, value) in arguments {
        let arg = entry
            .args
            .iter()
            .find(|arg| property(arg) == key)
            .ok_or_else(|| {
                McpError::invalid_params(
                    format!("`{}` takes no argument `{key}`", tool_name(&entry.command)),
                    None,
                )
            })?;
        let values = words(arg, &key, value)?;
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
    argv.push("--json".to_owned());
    positionals.sort_by_key(|(index, _)| *index);
    if !positionals.is_empty() {
        argv.push("--".to_owned());
        argv.extend(positionals.into_iter().flat_map(|(_, words)| words));
    }
    Ok(argv)
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

    /// Writes each frame to a server over an in-memory pipe and reads one reply per request.
    async fn exchange(frames: &[Value]) -> Vec<Value> {
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        let server = tokio::spawn(async move {
            Server::new(PathBuf::from("/nonexistent/ployz"))
                .serve(tokio::io::split(server_io))
                .await
                .expect("the handshake completes")
                .waiting()
                .await
                .expect("the server stops cleanly");
        });
        let (read, mut write) = tokio::io::split(client_io);
        let mut lines = BufReader::new(read).lines();
        let mut replies = Vec::new();
        for frame in frames {
            write
                .write_all(format!("{frame}\n").as_bytes())
                .await
                .unwrap();
            if frame.get("id").is_some() {
                let line = lines.next_line().await.unwrap().expect("a reply");
                replies.push(serde_json::from_str(&line).expect("a JSON-RPC frame"));
            }
        }
        server.abort();
        replies
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
        ] {
            assert!(
                tools.iter().all(|tool| tool["name"] != absent),
                "{absent} is not a Cloud tool"
            );
        }
        let no_reset = &tool("server_rm")["inputSchema"]["properties"]["no-reset"];
        assert_eq!(no_reset["type"], "boolean");
        assert!(
            no_reset["description"]
                .as_str()
                .unwrap()
                .contains("`--accept-volume-loss`"),
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
        frames.push(request(4, "tools/list", json!({})));
        let replies = exchange(&frames).await;
        assert_eq!(replies[1]["error"]["code"], -32602, "{}", replies[1]);
        assert_eq!(replies[2]["error"]["code"], -32602, "{}", replies[2]);
        assert!(replies[3]["result"]["tools"].is_array(), "{}", replies[3]);
    }

    #[test]
    fn a_call_becomes_flags_then_json_then_positionals() {
        let entry = catalog::commands()
            .into_iter()
            .find(|entry| entry.command == "server rm")
            .unwrap();
        let arguments = json!({ "server": "web-1", "no-reset": true, "confirm": null });
        let argv = argv(&entry, arguments.as_object().cloned().unwrap()).unwrap();
        assert_eq!(
            argv,
            ["server", "rm", "--no-reset", "--json", "--", "web-1"]
        );
    }
}
