//! `ployz mcp`: the agent tools over stdio. Each call steps the registry's plan,
//! answering its reads and its one write from the Config Store and Ployz Cloud.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{ArgMatches, Command};
use futures_util::future::join_all;
use ployz_core::{RpcError, RpcErrorCode};
use ployz_store::tools::{
    self, Act, Action, Ask, Gate, INSTRUCTIONS, Place, State, Step, StoreAt, ToolResult, Write,
};
use ployz_store::{Actor, ConfigStore, OrganizationId};
use reqwest::Method;
use rmcp::model::{
    BooleanSchema, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock,
    ElicitRequest, ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema,
    Implementation, InputRequest, InputRequiredResult, InputResponses, JsonObject, ListToolsResult,
    PaginatedRequestParams, RequestId, ServerCapabilities, ServerConfig, StringSchema, Tool,
    ToolAnnotations,
};
use rmcp::service::{RequestContext, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::OnceCell;
use tokio::time::Instant;

use super::{Error, leaf_matches, store};
use crate::approval::{self, Asked, Subject, Verb};
use crate::cloud_account::{self, Credential, StoreCallError};
use crate::cloud_login::{CredentialStore, LoginError};
use crate::failure::Failure;
use crate::published::{self, Effect, Published, PublishedCommand, Setup};

pub(crate) fn command() -> Command {
    Command::new("mcp").about("Serve the agent tools to a coding agent as MCP tools over stdio")
}

/// Outlasts the longest wait a tool may ask for and matches the default build limit. A
/// Deployment or a Cloud operation goes on after ployz mcp stops waiting for it.
pub(crate) const CALL_DEADLINE: Duration = Duration::from_secs(30 * 60);

pub(super) fn serve(root: &ArgMatches) -> Result<(), Error> {
    let config = super::config_path(leaf_matches(root))?;
    let local = if store::local_mode() {
        store::backend_at(&config)?
    } else {
        None
    };
    let store = match local {
        Some(store::Backend::Local(store, who)) => Store::Local(store, who),
        _ => Store::Cloud,
    };
    let cloud = std::env::var(crate::cli::env::CLOUD_URL).ok();
    // Only Cloud runs a published command, so a local Store lists none.
    let setup = matches!(store, Store::Cloud).then(|| Setup::beside(&config, cloud.clone()));
    let linked = config.clone();
    // Like each CLI command, each call reads the link, so `ployz link` or
    // `ployz org use` takes effect on the next call.
    let place: Locate = Box::new(move |store| {
        let at = super::link::scope_from_env(&linked)?.at();
        Ok(Place {
            project: at.project,
            environment: at.environment,
            store,
        })
    });
    let server = Server::new(
        Account {
            config,
            token: std::env::var(crate::cli::env::TOKEN).ok(),
            cloud,
            organization: OnceCell::new(),
        },
        store,
        place,
    );
    let runtime = super::runtime()?;
    let server = match setup {
        Some(setup) => runtime.block_on(server.publishing(setup)),
        None => server,
    };
    let served = runtime.block_on(async {
        let mut terminate = signal(SignalKind::terminate())?;
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut hangup = signal(SignalKind::hangup())?;
        let mut pipe = signal(SignalKind::pipe())?;
        let serving = async {
            server
                .serve(Lines::new(tokio::io::stdin(), tokio::io::stdout()))
                .await
                .map_err(Failure::command)?
                .waiting()
                .await
                .map_err(Failure::command)?;
            Ok::<_, Error>(())
        };
        tokio::select! {
            quit = serving => quit?,
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            _ = hangup.recv() => {}
            _ = pipe.recv() => {}
        }
        Ok(())
    });
    // A thread may still be blocked reading stdin; exit without waiting for it.
    runtime.shutdown_background();
    served
}

/// Newline-delimited JSON-RPC over a byte stream. Unlike rmcp's own stdio transport, a
/// line that is not JSON gets the Parse error JSON-RPC 2.0 requires instead of silence.
struct Lines<R, W: AsyncWrite> {
    read: BufReader<R>,
    line: Vec<u8>,
    write: AsyncRwTransport<RoleServer, tokio::io::Empty, W>,
}

impl<R: AsyncRead, W: AsyncWrite + Send + Unpin + 'static> Lines<R, W> {
    fn new(read: R, write: W) -> Self {
        Self {
            read: BufReader::new(read),
            line: Vec::new(),
            write: AsyncRwTransport::new(tokio::io::empty(), write),
        }
    }
}

impl<R, W> Transport<RoleServer> for Lines<R, W>
where
    R: AsyncRead + Send + Unpin,
    W: AsyncWrite + Send + Unpin + 'static,
{
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.write.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            // A cancelled read leaves its partial line in `self.line` for the next call.
            if self.read.read_until(b'\n', &mut self.line).await.ok()? == 0 {
                return None;
            }
            let line = self.line.trim_ascii();
            let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
            if line.is_empty() {
                self.line.clear();
                continue;
            }
            let parsed = serde_json::from_slice::<Value>(line);
            self.line.clear();
            let (error, id) = match parsed {
                Err(_) => (McpError::parse_error("Parse error", None), None),
                Ok(value) => {
                    let notification = value.get("method").is_some() && value.get("id").is_none();
                    let id = value
                        .get("id")
                        .and_then(|id| serde_json::from_value(id.clone()).ok());
                    match serde_json::from_value(value) {
                        Ok(message) => return Some(message),
                        // A notification is never answered, even one this server doesn't know.
                        Err(_) if notification => continue,
                        Err(_) => (McpError::invalid_request("Invalid request", None), id),
                    }
                }
            };
            self.write
                .send(TxJsonRpcMessage::<RoleServer>::error(error, id))
                .await
                .ok()?;
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        self.write.close().await
    }
}

/// Where the Config Store is. Only Cloud answers Cloud asks and runs Acts.
enum Store {
    Local(Arc<ConfigStore>, Actor),
    Cloud,
}

/// How one call answers its steps.
enum Backend<'a> {
    Local(&'a ConfigStore, &'a Actor),
    Cloud(Credential),
}

impl Backend<'_> {
    async fn asks(&self, asks: &[Ask]) -> Vec<Result<Value, RpcError>> {
        join_all(asks.iter().map(|each| self.ask(each))).await
    }

    async fn ask(&self, ask: &Ask) -> Result<Value, RpcError> {
        match (self, ask) {
            (Self::Local(store, who), Ask::Store(query)) => {
                store.read(who, query).and_then(|view| value(&view))
            }
            (Self::Local(..), Ask::Cloud(_)) => Err(not_here()),
            (Self::Cloud(credential), Ask::Store(query)) => {
                called(cloud_account::config_store(credential, "read", query, None).await)
            }
            (Self::Cloud(credential), Ask::Cloud(query)) => called(
                cloud_account::approved_answer(
                    credential,
                    Method::POST,
                    "agent/ask",
                    &json!({ "ask": query }),
                    None,
                )
                .await,
            ),
        }
    }

    async fn write(&self, write: &Write, approval: Option<&str>) -> Result<Value, RpcError> {
        match (self, write) {
            (Self::Local(store, who), Write::Store(command)) => store
                .write(who, command)
                .and_then(|written| value(&written)),
            (Self::Local(..), Write::Act(_)) => Err(not_here()),
            (Self::Cloud(credential), Write::Store(command)) => {
                written(cloud_account::config_store(credential, "write", command, approval).await)
            }
            (Self::Cloud(credential), Write::Act(act)) => written(
                cloud_account::approved_answer(
                    credential,
                    Method::POST,
                    "agent/act",
                    &json!({ "act": act }),
                    approval,
                )
                .await,
            ),
        }
    }
}

fn value(answer: &impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(answer).map_err(|error| RpcError::caused(RpcErrorCode::Internal, &error))
}

fn called(answer: Result<Value, StoreCallError>) -> Result<Value, RpcError> {
    answer.map_err(|error| match error {
        StoreCallError::Refused(error) => error,
        error @ (StoreCallError::Cloud(_) | StoreCallError::Stopped(_)) => {
            Error::from(error).report()
        }
    })
}

/// A write's answer. When Cloud may have taken the write but its reply was lost or
/// unreadable, the refusal says `writes: unknown` so the retry repeats the call's key.
fn written(answer: Result<Value, StoreCallError>) -> Result<Value, RpcError> {
    let unknown = matches!(&answer, Err(StoreCallError::Cloud(error)) if may_have_landed(error));
    called(answer).map_err(|mut error| {
        if unknown {
            if !error.details.is_object() {
                error.details = json!({});
            }
            if let Value::Object(details) = &mut error.details {
                details.insert("writes".to_owned(), json!("unknown"));
            }
        }
        error
    })
}

/// A write ployz mcp stopped waiting for after sending it.
fn cut_off(tool: &str, deadline: Duration) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message: format!(
            "Stopped waiting for `{tool}` after {}; Ployz Cloud may still take what it sent",
            span(deadline)
        ),
        details: json!({ "writes": "unknown" }),
        cause: Vec::new(),
    }
}

/// Whether Cloud may have acted on a request that failed this way.
fn may_have_landed(error: &LoginError) -> bool {
    match error {
        LoginError::Unreachable { .. } | LoginError::Status { .. } | LoginError::Reply(_) => true,
        LoginError::Unsupported(_)
        | LoginError::Store { .. }
        | LoginError::Corrupt { .. }
        | LoginError::OtherCloud { .. }
        | LoginError::SignedOut
        | LoginError::Expired
        | LoginError::Denied
        | LoginError::Ended
        | LoginError::AwaitingApproval { .. }
        | LoginError::TokenRefused
        | LoginError::NotMember(_)
        | LoginError::TokenBound
        | LoginError::UnknownOrganization(_)
        | LoginError::UnknownCredential(_) => false,
    }
}

/// The registry refuses Cloud tools on a local Store before asking; this guards the rest.
fn not_here() -> RpcError {
    RpcError {
        code: RpcErrorCode::from("not_here"),
        message: "This needs Ployz Cloud; a local Config Store cannot answer it".to_owned(),
        details: Value::Null,
        cause: Vec::new(),
    }
}

/// The Project and Environment a call acts on, read when the call starts.
type Locate = Box<dyn Fn(StoreAt) -> Result<Place, Error> + Send + Sync>;

struct Server {
    account: Account,
    store: Store,
    place: Locate,
    deadline: Duration,
    session: String,
    keys: fn(&str, &RequestId, &str) -> String,
    tools: Vec<Tool>,
    gates: HashMap<&'static str, Gate>,
    waiting: Waiting,
    /// The Cloud whose published commands are listed, when the Store is Cloud.
    setup: Option<Setup>,
    /// The published commands as last read, replaced when a reply names a newer revision.
    published: Mutex<Option<Published>>,
}

#[derive(Default)]
struct Account {
    config: PathBuf,
    token: Option<String>,
    cloud: Option<String>,
    /// The Organization `token` acts in, asked of Ployz Cloud once.
    organization: OnceCell<OrganizationId>,
}

impl Server {
    fn new(account: Account, store: Store, place: Locate) -> Self {
        let catalog = tools::catalog();
        let gates = catalog.iter().map(|def| (def.name, def.gate)).collect();
        Self {
            account,
            store,
            place,
            deadline: CALL_DEADLINE,
            session: uuid::Uuid::new_v4().to_string(),
            keys: call_key,
            tools: catalog.into_iter().map(tool).collect(),
            gates,
            waiting: Waiting::default(),
            setup: None,
            published: Mutex::new(None),
        }
    }

    /// List `setup`'s published commands: fetched now within the refresh budget, else
    /// as last cached, else none.
    async fn publishing(mut self, setup: Setup) -> Self {
        let published = self.refreshed(&setup).await.or_else(|| setup.cached());
        self.setup = Some(setup);
        self.published = Mutex::new(published);
        self
    }

    async fn refreshed(&self, setup: &Setup) -> Option<Published> {
        let credential = match self.backend().await {
            Ok(Backend::Cloud(credential)) => Some(credential),
            Ok(Backend::Local(..)) | Err(_) => None,
        };
        setup.refresh(credential.as_ref()).await.ok()
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Option<Published>> {
        self.published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The compiled tools, then each agent-facing published command.
    fn tools(&self) -> Vec<Tool> {
        let mut tools = self.tools.clone();
        if let Some(published) = self.held().as_ref() {
            tools.extend(
                published
                    .commands
                    .iter()
                    .filter(|command| command.agents)
                    .map(published_tool),
            );
        }
        tools
    }

    fn published_named(&self, name: &str) -> Option<PublishedCommand> {
        self.held()
            .as_ref()?
            .commands
            .iter()
            .find(|command| command.agents && command.name == name)
            .cloned()
    }

    /// Reread the published commands when `revision` is not the one held, and tell the
    /// host the list changed.
    async fn follow(&self, revision: Option<String>, context: &RequestContext<RoleServer>) {
        let (Some(revision), Some(setup)) = (revision, &self.setup) else {
            return;
        };
        if self
            .held()
            .as_ref()
            .is_some_and(|published| published.revision == revision)
        {
            return;
        }
        let Some(refreshed) = self.refreshed(setup).await else {
            return;
        };
        *self.held() = Some(refreshed);
        let _ = context.peer.notify_tool_list_changed().await;
    }

    /// Cloud no longer has the command: drop it here and from the cache, and tell the host.
    async fn forget(&self, name: &str, context: &RequestContext<RoleServer>) {
        if let Some(published) = self.held().as_mut() {
            published.commands.retain(|command| command.name != name);
        }
        if let Some(setup) = &self.setup {
            setup.evict(name);
        }
        let _ = context.peer.notify_tool_list_changed().await;
    }

    async fn call_published(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let command = self
            .published_named(tool)
            .ok_or_else(|| McpError::invalid_params(format!("no tool named {tool}"), None))?;
        let mut input = arguments.unwrap_or_default();
        let approval = match (command.asks(), input.remove(APPROVAL)) {
            (false, Some(given)) => {
                input.insert(APPROVAL.to_owned(), given);
                None
            }
            (_, None | Some(Value::Null)) => None,
            (true, Some(Value::String(id))) => Some(id),
            (true, Some(_)) => {
                return Err(McpError::invalid_params(
                    format!("`{APPROVAL}` takes one string"),
                    None,
                ));
            }
        };
        let credential = match self.backend().await {
            Ok(Backend::Cloud(credential)) => credential,
            Ok(Backend::Local(..)) => return Ok(refused(not_here().message).into()),
            Err(text) => return Ok(refused(text).into()),
        };
        if let Some(setup) = &self.setup
            && let Err(error) = setup.acts_with(&credential)
        {
            return Ok(refused(error.to_string()).into());
        }
        if command.typed.is_some() {
            return Ok(refused(format!(
                "`{tool}` asks a human to type the Organization's name first; run `ployz {}` \
                 on a terminal instead.",
                command.path()
            ))
            .into());
        }
        let key = (self.keys)(&self.session, &context.id, tool);
        let (ran, revision) = tokio::select! {
            biased;
            ran = published::run_on(&credential, tool, Value::Object(input), approval.as_deref(), &key) => ran,
            () = context.ct.cancelled() => {
                return Err(McpError::internal_error("the call was cancelled", None));
            }
            () = tokio::time::sleep(self.deadline) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(overdue(tool, self.deadline))]).into());
            }
        };
        let result = match ran {
            Ok(published::Ran { output, .. }) => CallToolResult::structured(output),
            Err(StoreCallError::Refused(error)) => {
                if error.code.as_str() == published::UNKNOWN_COMMAND {
                    self.forget(tool, context).await;
                }
                CallToolResult::structured_error(json!(error))
            }
            Err(error @ (StoreCallError::Cloud(_) | StoreCallError::Stopped(_))) => {
                refused(Error::from(error).to_string())
            }
        };
        self.follow(revision, context).await;
        Ok(result.into())
    }

    async fn backend(&self) -> Result<Backend<'_>, String> {
        match &self.store {
            // The in-process Store sweeps as each call starts, as each CLI command does.
            Store::Local(store, who) => store::sweep(store, who)
                .map(|()| Backend::Local(store, who))
                .map_err(|error| Error::from(StoreCallError::Refused(error)).to_string()),
            Store::Cloud => cloud_account::credential(
                &CredentialStore::beside(&self.account.config),
                self.account.token.clone(),
                self.account.cloud.clone(),
            )
            .await
            .map(Backend::Cloud)
            .map_err(|error| Error::from(error).to_string()),
        }
    }

    /// Which Store a new call reaches. A device can switch Organizations
    /// between calls; an Organization Token cannot.
    async fn store_at(&self, backend: &Backend<'_>) -> Result<StoreAt, String> {
        let credential = match backend {
            Backend::Local(..) => return Ok(StoreAt::Local),
            Backend::Cloud(credential) => credential,
        };
        let acting = || async {
            let organization = cloud_account::acting_in(credential)
                .await
                .map_err(|error| Error::from(error).to_string())?;
            OrganizationId::parse(organization.id).map_err(|error| error.message)
        };
        let organization = match credential {
            Credential::Device(_) => acting().await?,
            Credential::Token { .. } => self
                .account
                .organization
                .get_or_try_init(acting)
                .await?
                .clone(),
        };
        Ok(StoreAt::Cloud { organization })
    }
}

/// MCP forbids reusing a request ID within a session, so the session and the ID name
/// one request: a replay of it keeps its key, and a restarted `ployz mcp` never reuses one.
fn call_key(session: &str, id: &RequestId, _tool: &str) -> String {
    format!("{session}:{}", json!(id))
}

fn tool(def: tools::ToolDef) -> Tool {
    let mut input = object(def.input_schema);
    if def.gate != Gate::Never
        && let Some(Value::Object(properties)) = input.get_mut("properties")
    {
        properties.insert(APPROVAL.to_owned(), approval_property());
    }
    let mut tool = Tool::new(def.name, def.description, Arc::new(input)).annotate(
        ToolAnnotations::new()
            .read_only(def.read_only)
            .destructive(def.gate != Gate::Never),
    );
    tool.output_schema = Some(Arc::new(object(def.output_schema)));
    tool
}

/// A published command as the host sees it: its input schema as published, plus
/// `approval` when Cloud may ask for one.
fn published_tool(command: &PublishedCommand) -> Tool {
    let mut input = object(command.input.clone());
    if command.asks()
        && let Some(Value::Object(properties)) = input.get_mut("properties")
    {
        properties.insert(APPROVAL.to_owned(), approval_property());
    }
    let mut tool = Tool::new(command.name.clone(), command.about.clone(), Arc::new(input))
        .annotate(
            ToolAnnotations::new()
                .read_only(command.effect == Effect::Read)
                .destructive(command.asks()),
        );
    tool.output_schema = Some(Arc::new(object(command.output.clone())));
    tool
}

fn approval_property() -> Value {
    json!({
        "type": "string",
        "description": "The approval id a refusal named, once a human approved it in Ployz Cloud.",
    })
}

fn object(schema: Value) -> JsonObject {
    if let Value::Object(object) = schema {
        object
    } else {
        JsonObject::new()
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ployz", env!("CARGO_PKG_VERSION")))
            .with_instructions(format!(
                "{INSTRUCTIONS} A call still running after {} is stopped; its error names the \
                 tool that shows what it left running.",
                span(self.deadline)
            ))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(self.tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().into_iter().find(|tool| tool.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let tool: &str = &request.name;
        let Some(gate) = self.gates.get(tool).copied() else {
            return self.call_published(tool, request.arguments, &context).await;
        };
        let mut input = request.arguments.unwrap_or_default();
        let approval = match (gate, input.remove(APPROVAL)) {
            (Gate::Never, Some(given)) => {
                input.insert(APPROVAL.to_owned(), given);
                None
            }
            (_, None | Some(Value::Null)) => None,
            (_, Some(Value::String(id))) => Some(id),
            (_, Some(_)) => {
                return Err(McpError::invalid_params(
                    format!("`{APPROVAL}` takes one string"),
                    None,
                ));
            }
        };
        let input = Value::Object(input);
        let backend = match self.backend().await {
            Ok(backend) => backend,
            Err(text) => return Ok(refused(text).into()),
        };
        let (step, state, answered, until) = match request.request_state {
            Some(state) => {
                let answer = answer(request.input_responses)?;
                let held = self.waiting.take(&state, tool, &input)?;
                if held.until <= Instant::now() {
                    let verb = verb(tool, &held.asked);
                    return Ok(refused(late(verb, &held.asked, self.deadline)).into());
                }
                let step = Step::Write { write: held.write };
                (step, held.state, Some((held.asked, answer)), held.until)
            }
            None => {
                let store = match self.store_at(&backend).await {
                    Ok(store) => store,
                    Err(text) => return Ok(refused(text).into()),
                };
                let place = match (self.place)(store) {
                    Ok(place) => place,
                    Err(error) => return Ok(refused(error.to_string()).into()),
                };
                let key = (self.keys)(&self.session, &context.id, tool);
                let (step, state) = tools::start(tool, input.clone(), &place, &key);
                (step, state, None, Instant::now() + self.deadline)
            }
        };
        let call = Call {
            tool,
            input: &input,
            backend: &backend,
            until,
        };
        tokio::select! {
            // A write past the deadline settles to its refusal before the arm below fires.
            biased;
            result = self.settle(&call, step, state, approval, answered, &context) => result,
            () = context.ct.cancelled() => {
                Err(McpError::internal_error("the call was cancelled", None))
            }
            () = tokio::time::sleep_until(until) => {
                let content = vec![ContentBlock::text(overdue(tool, self.deadline))];
                Ok(CallToolResult::error(content).into())
            }
        }
    }
}

const APPROVAL: &str = "approval";

enum Asking {
    Unable,
    InResult,
    ByRequest,
}

fn asking(context: &RequestContext<RoleServer>) -> Asking {
    let form = context
        .client_capabilities()
        .and_then(|capabilities| capabilities.elicitation)
        .is_some_and(|elicitation| elicitation.form.is_some() || elicitation.url.is_none());
    if !form {
        Asking::Unable
    } else if context
        .protocol_version()
        .is_some_and(|version| !version.has_initialize())
    {
        Asking::InResult
    } else {
        Asking::ByRequest
    }
}

/// A dialog returned in a tool result, waiting for the host to call the same tool again
/// with the human's answer before the first call's deadline.
struct Held {
    tool: String,
    input: Value,
    state: State,
    write: Box<Write>,
    asked: Asked,
    until: Instant,
}

#[derive(Default)]
struct Waiting {
    held: Mutex<HashMap<String, Held>>,
    asked: AtomicU64,
}

impl Waiting {
    /// Hold `held` under a state of its own: two calls can ask about the same approval.
    fn hold(&self, held: Held) -> String {
        let state = format!(
            "{}.{}",
            held.asked.id,
            self.asked.fetch_add(1, Ordering::Relaxed)
        );
        let mut waiting = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        waiting.retain(|_, held| held.until > now);
        waiting.insert(state.clone(), held);
        state
    }

    fn take(&self, state: &str, tool: &str, input: &Value) -> Result<Held, McpError> {
        let mut waiting = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Entry::Occupied(found) = waiting.entry(state.to_owned()) else {
            return Err(McpError::invalid_params(
                "no approval waits on this request state".to_owned(),
                None,
            ));
        };
        if found.get().tool != tool || found.get().input != *input {
            return Err(McpError::invalid_params(
                format!(
                    "this approval was asked by another call; answer it by calling `{}` again \
                     with the same arguments",
                    found.get().tool
                ),
                None,
            ));
        }
        let held = found.remove();
        let now = Instant::now();
        waiting.retain(|_, held| held.until > now);
        Ok(held)
    }
}

fn answer(responses: Option<InputResponses>) -> Result<ElicitResult, McpError> {
    let answer = responses
        .and_then(|mut responses| responses.remove(APPROVAL))
        .ok_or_else(|| {
            McpError::invalid_params(
                format!("this retry carries no answer to `{APPROVAL}`"),
                None,
            )
        })?;
    serde_json::from_value(answer).map_err(|error| {
        McpError::invalid_params(format!("the `{APPROVAL}` answer: {error}"), None)
    })
}

struct Call<'a> {
    tool: &'a str,
    input: &'a Value,
    backend: &'a Backend<'a>,
    until: Instant,
}

fn verb(tool: &str, asked: &Asked) -> Verb {
    match (&asked.subject, tool) {
        (Subject::Operation(operation), _) => operation.verb,
        (Subject::Diff(_), "publish") => Verb::Publish,
        (Subject::Diff(_), "environment_remove" | "project_remove") => Verb::Remove,
        (Subject::Diff(_), _) => Verb::Deploy,
    }
}

/// The name a human types to approve a removal: the Environment as `project/environment`,
/// the Project, or the Server.
fn typed(write: &Write) -> Option<String> {
    match write {
        Write::Store(ployz_store::Command::Remove(remove)) => Some(match &remove.environment {
            Some(environment) => format!("{}/{environment}", remove.project),
            None => remove.project.to_string(),
        }),
        Write::Act(Act {
            action: Action::RemoveServer { server, .. },
            ..
        }) => Some(server.name.to_string()),
        Write::Store(_) | Write::Act(_) => None,
    }
}

impl Server {
    async fn settle(
        &self,
        call: &Call<'_>,
        mut step: Step,
        mut state: State,
        mut approval: Option<String>,
        mut answered: Option<(Asked, ElicitResult)>,
        context: &RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        loop {
            let answers =
                match step {
                    Step::Done { result } => return Ok(rendered(result).into()),
                    Step::Read { asks } => call.backend.asks(&asks).await,
                    Step::Wait { ms, then } => {
                        tokio::time::sleep(Duration::from_millis(ms.into())).await;
                        call.backend.asks(&then).await
                    }
                    Step::Write { write } => {
                        if let Some((asked, answer)) = answered.take() {
                            let Backend::Cloud(credential) = call.backend else {
                                return Ok(refused(not_here().message).into());
                            };
                            let verb = verb(call.tool, &asked);
                            let typed = typed(&write);
                            match self
                                .decide(credential, verb, &asked, typed.as_deref(), answer)
                                .await?
                            {
                                Answer::Approved => approval = Some(asked.id),
                                Answer::Refused(refused) => return Ok(refused.into()),
                            }
                        }
                        // Stopped once the write is sent, Cloud may still take it: the
                        // refusal says so, and its retry repeats the call's key.
                        let written = tokio::select! {
                            biased;
                            written = call.backend.write(&write, approval.as_deref()) => written,
                            () = tokio::time::sleep_until(call.until) => {
                                Err(cut_off(call.tool, self.deadline))
                            }
                        };
                        let asked = match (&written, call.backend) {
                            (Err(error), Backend::Cloud(_)) => Asked::of(error),
                            _ => None,
                        };
                        let Some(asked) = asked else {
                            (step, state) = tools::resume(state, vec![written]);
                            continue;
                        };
                        let verb = verb(call.tool, &asked);
                        let form = form(verb, &asked, typed(&write).as_deref());
                        match asking(context) {
                            Asking::Unable => return Ok(cannot_ask(verb, &asked).into()),
                            Asking::InResult => {
                                let requests = BTreeMap::from([(
                                    APPROVAL.to_owned(),
                                    InputRequest::Elicitation(ElicitRequest::new(form)),
                                )]);
                                let state = self.waiting.hold(Held {
                                    tool: call.tool.to_owned(),
                                    input: call.input.clone(),
                                    state,
                                    write,
                                    asked,
                                    until: call.until,
                                });
                                return Ok(
                                    InputRequiredResult::new(Some(requests), Some(state)).into()
                                );
                            }
                            Asking::ByRequest => {
                                let answer = context.peer.create_elicitation(form).await.map_err(
                                    |error| McpError::internal_error(error.to_string(), None),
                                )?;
                                answered = Some((asked, answer));
                                step = Step::Write { write };
                                continue;
                            }
                        }
                    }
                };
            (step, state) = tools::resume(state, answers);
        }
    }

    async fn decide(
        &self,
        credential: &Credential,
        verb: Verb,
        asked: &Asked,
        typed: Option<&str>,
        answer: ElicitResult,
    ) -> Result<Answer, McpError> {
        let content = answer.content.unwrap_or_default();
        let accepted = matches!(answer.action, ElicitationAction::Accept);
        let approved = accepted && content.get("approve") == Some(&Value::Bool(true));
        if let (true, Some(name)) = (approved, typed)
            && content.get("name").and_then(Value::as_str).map(str::trim) != Some(name)
        {
            return Ok(Answer::Refused(refused(format!(
                "The human did not type `{name}`, so the answer was not recorded and nothing \
                 was {past}. The approval of {what} stays pending in Ployz Cloud; call the \
                 tool again to ask again.",
                past = verb.past(),
                what = asked.what(verb),
            ))));
        }
        let reason = if accepted { reason(&content) } else { None };
        let decision = match (approved, &reason) {
            (true, _) => json!({ "approve": { "digest": asked.digest } }),
            (false, Some(reason)) => json!({ "reject": { "reason": reason } }),
            (false, None) => json!({ "reject": {} }),
        };
        match approval::decide(credential, &asked.id, &decision).await {
            Ok(()) => {}
            Err(StoreCallError::Refused(error)) if error.code == RpcErrorCode::Conflict => {
                return Ok(Answer::Refused(refused(format!(
                    "{} This answer was not recorded, and nothing was {} by this call.",
                    error.message,
                    verb.past()
                ))));
            }
            Err(error) => return Ok(Answer::Refused(refused(Error::from(error).to_string()))),
        }
        if approved {
            return Ok(Answer::Approved);
        }
        let because = reason.map_or_else(String::new, |reason| format!(": {reason}"));
        Ok(Answer::Refused(refused(format!(
            "The human denied this {noun}{because}. Nothing was {past}; do not retry it \
             unless they ask.",
            noun = verb.noun(),
            past = verb.past(),
        ))))
    }
}

fn rendered(result: ToolResult) -> CallToolResult {
    match result {
        ToolResult::Ok(value) => CallToolResult::structured(value),
        ToolResult::Refused(refusal) => CallToolResult::structured_error(json!(refusal)),
    }
}

/// Cloud keeps a denial's reason up to this many UTF-16 code units.
const REASON_LIMIT: u16 = 1024;

fn reason(content: &Value) -> Option<String> {
    let mut units = 0;
    let reason: String = content
        .get("reason")?
        .as_str()?
        .trim()
        .chars()
        .take_while(|character| {
            units += character.len_utf16();
            units <= usize::from(REASON_LIMIT)
        })
        .collect();
    (!reason.is_empty()).then_some(reason)
}

fn late(verb: Verb, asked: &Asked, deadline: Duration) -> String {
    format!(
        "The human answered more than {span} after {what} was called, so the answer was not \
         recorded and nothing was {past}. The approval of {what} stays pending in Ployz \
         Cloud; call the tool again to ask again.",
        what = asked.what(verb),
        past = verb.past(),
        span = span(deadline),
    )
}

fn form(verb: Verb, asked: &Asked, typed: Option<&str>) -> ElicitRequestParams {
    let schema = ElicitationSchema::builder()
        .required_bool_property("approve", |schema: BooleanSchema| {
            schema
                .title("Approve")
                .description(format!("Go ahead with this {}", verb.noun()))
        })
        .string_property("reason", |schema: StringSchema| {
            schema
                .title("Reason")
                .description("Why not, if you leave Approve unticked")
                .max_length(u32::from(REASON_LIMIT))
        });
    let schema = match typed {
        Some(name) => schema.string_property("name", |schema: StringSchema| {
            schema
                .title("Name")
                .description(format!("Type {name} to approve"))
        }),
        None => schema,
    };
    ElicitRequestParams::FormElicitationParams {
        meta: None,
        message: format!(
            "Approve {}?\n{}",
            asked.what(verb),
            asked.review(verb).join("\n")
        ),
        requested_schema: schema.build_unchecked(),
    }
}

fn cannot_ask(verb: Verb, asked: &Asked) -> CallToolResult {
    refused(format!(
        "A human must approve {what} first, and this agent cannot ask \
         them. Show them what it destroys, then ask them to approve {what} in the Ployz Cloud \
         sidebar. Once they approve, call this tool again with `approval` set to `{id}`.\n\
         {review}",
        what = asked.what(verb),
        id = asked.id,
        review = asked.review(verb).join("\n"),
    ))
}

fn refused(text: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text)])
}

enum Answer {
    Approved,
    Refused(CallToolResult),
}

fn overdue(tool: &str, deadline: Duration) -> String {
    let status = match tool.split('_').next() {
        Some("server") => "server_show",
        Some("volume") => "volume_show",
        Some("service") => "service_status",
        _ => "deployment_show",
    };
    format!(
        "ployz mcp stopped `{tool}` after {}. What it started may still be running; check \
         with `{status}`.",
        span(deadline),
    )
}

fn span(deadline: Duration) -> String {
    match deadline.as_secs() {
        secs if secs % 60 == 0 => format!("{} minutes", secs / 60),
        secs => format!("{secs} seconds"),
    }
}

#[cfg(test)]
mod tests;
