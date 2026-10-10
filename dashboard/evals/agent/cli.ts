import { spawn, spawnSync } from "node:child_process";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import {
  type AdapterYieldChunk,
  type DefaultMessageMetadataByModality,
  EventType,
  type ModelMessage,
  normalizeSystemPrompts,
  type TextOptions,
} from "@tanstack/ai";
import { BaseTextAdapter, type StructuredOutputResult } from "@tanstack/ai/adapters";
import { Option, Result, Schema } from "effect";

export type Call = { readonly id: string; readonly name: string; readonly input: unknown };
/** One model turn as a CLI printed it: what the model said, then the tool calls it made at once. */
export type Turn = { readonly text: string; readonly calls: ReadonlyArray<Call> };

/** Whether a CLI's output so far holds the whole turn: now, once no more arrives for a moment, or not yet. */
type Settled = "now" | "quiet" | "no";

type Files = { readonly cwd: string; readonly system: string; readonly tools: string; readonly mcp: string };

/** A coding-agent CLI run for one model turn, reading its output as `Event`s. */
type Cli<Event> = {
  readonly name: string;
  readonly command: string;
  readonly args: (model: string, files: Files) => ReadonlyArray<string>;
  readonly line: Schema.Codec<Event, string>;
  readonly settled: (events: ReadonlyArray<Event>) => Settled;
  readonly parse: (events: ReadonlyArray<Event>) => Result.Result<Turn, string>;
};

const TIMEOUT_MS = 180_000;
const QUIET_MS = 400;
const SERVER = fileURLToPath(new URL("./mcp-tools.mjs", import.meta.url));

/** `name` without the `mcp__<server>__` prefix a CLI puts on MCP tools. */
export const stripServer = (name: string) => name.replace(/^mcp__.+?__/, "");

const contentText = (content: ModelMessage["content"]) =>
  Array.isArray(content) ? content.map((part) => (part.type === "text" ? part.content : `[${part.type}]`)).join("") : (content ?? "");

const FRAMES = {
  tools: {
    preamble: "The conversation so far, oldest first. Each tool call already ran, and its result follows it.",
    closing: "Write the assistant's next turn, continuing from the last message. Act through your tools, never by writing a tool call or a transcript tag as text.",
  },
  chat: {
    preamble: "The conversation so far, oldest first.",
    closing: "Write the assistant's next message, in reply to the last user message, as plain text with no transcript tag.",
  },
};

/** `messages` as one prompt: a lone user message as itself, a longer conversation as a tagged transcript framed for `mode`. */
export const render = (messages: ReadonlyArray<ModelMessage>, mode: keyof typeof FRAMES) => {
  const [first] = messages;
  if (messages.length === 1 && first?.role === "user") return contentText(first.content);
  const turns = messages.flatMap((message) => {
    const said = contentText(message.content);
    if (message.role === "user") return [`<user>\n${said}\n</user>`];
    if (message.role === "tool") {
      return [`<tool_result id="${message.toolCallId ?? ""}"${message.error === undefined ? "" : " error"}>\n${said}\n</tool_result>`];
    }
    return [
      ...(said === "" ? [] : [`<assistant>\n${said}\n</assistant>`]),
      ...(message.toolCalls ?? []).map((call) => `<tool_call id="${call.id}" name="${call.function.name}">${call.function.arguments || "{}"}</tool_call>`),
    ];
  });
  const { preamble, closing } = FRAMES[mode];
  return `${preamble}\n\n${turns.join("\n")}\n\n${closing}`;
};

const ClaudeBlock = Schema.Union([
  Schema.Struct({ type: Schema.Literal("text"), text: Schema.String }),
  Schema.Struct({ type: Schema.Literal("tool_use"), id: Schema.String, name: Schema.String, input: Schema.Unknown }),
  Schema.Struct({ type: Schema.String }),
]);
const ClaudeEvent = Schema.Union([
  Schema.Struct({ type: Schema.Literal("assistant"), message: Schema.Struct({ id: Schema.String, content: Schema.Array(ClaudeBlock) }) }),
  Schema.Struct({
    type: Schema.Literal("result"),
    subtype: Schema.String,
    is_error: Schema.Boolean,
    result: Schema.optionalKey(Schema.String),
    errors: Schema.optionalKey(Schema.Array(Schema.String)),
  }),
]);
type ClaudeEvent = typeof ClaudeEvent.Type;
/** A line of `claude -p --output-format stream-json` output that matters to a turn. */
export const ClaudeLine = Schema.fromJsonString(ClaudeEvent);

/**
 * The first model message in `claude -p` output. Claude prints each of its blocks as its own assistant event under one
 * message id, with the denied tool results between them, so every event with that id belongs to the turn.
 */
export const parseClaude = (events: ReadonlyArray<ClaudeEvent>): Result.Result<Turn, string> => {
  let message: string | undefined;
  let text = "";
  const calls: Array<Call> = [];
  for (const event of events) {
    if (event.type === "result") {
      if (calls.length > 0 || !event.is_error) return Result.succeed({ text, calls });
      return Result.fail(event.errors?.join("; ") ?? event.result ?? event.subtype);
    }
    message ??= event.message.id;
    if (event.message.id !== message) continue;
    for (const block of event.message.content) {
      if ("id" in block) calls.push({ id: block.id, name: stripServer(block.name), input: block.input });
      else if ("text" in block) text += block.text;
    }
  }
  return calls.length > 0 ? Result.succeed({ text, calls }) : Result.fail("claude ended without a result");
};

const claudeSettled = (events: ReadonlyArray<ClaudeEvent>): Settled => (events.at(-1)?.type === "result" ? "now" : "no");

const CodexEvent = Schema.Union([
  Schema.Struct({ type: Schema.Literal("thread.started"), thread_id: Schema.String }),
  Schema.Struct({
    type: Schema.Literals(["item.started", "item.completed"]),
    item: Schema.Struct({
      id: Schema.String,
      type: Schema.String,
      text: Schema.optionalKey(Schema.String),
      tool: Schema.optionalKey(Schema.String),
      arguments: Schema.optionalKey(Schema.Unknown),
    }),
  }),
  Schema.Struct({ type: Schema.Literal("turn.completed") }),
  Schema.Struct({ type: Schema.Literal("turn.failed"), error: Schema.Struct({ message: Schema.String }) }),
  Schema.Struct({ type: Schema.Literal("error"), message: Schema.String }),
]);
type CodexEvent = typeof CodexEvent.Type;
/** A line of `codex exec --json` output that matters to a turn. */
export const CodexLine = Schema.fromJsonString(CodexEvent);

const isCall = (event: CodexEvent | undefined) => event !== undefined && "item" in event && event.item.type === "mcp_tool_call";

/**
 * The first model turn in `codex exec --json` output. Codex runs on after a denied tool call, so the turn ends at the
 * first run of tool calls: what came before them is the text, what comes after is ignored.
 */
export const parseCodex = (events: ReadonlyArray<CodexEvent>): Result.Result<Turn, string> => {
  let thread = "";
  const said: Array<string> = [];
  const calls = new Map<string, Call>();
  let warned: string | undefined;
  const turn = (): Result.Result<Turn, string> => Result.succeed({ text: said.join("\n\n"), calls: [...calls.values()] });
  for (const event of events) {
    switch (event.type) {
      case "thread.started":
        thread = event.thread_id;
        break;
      case "error":
        warned = event.message;
        break;
      case "turn.failed":
        return calls.size > 0 ? turn() : Result.fail(event.error.message);
      case "turn.completed":
        return turn();
      default: {
        const { item } = event;
        if (item.type === "mcp_tool_call") {
          if (!calls.has(item.id)) calls.set(item.id, { id: `call_${thread}_${item.id}`, name: item.tool ?? "", input: item.arguments ?? {} });
        } else if (calls.size > 0) {
          return turn();
        } else if (item.type === "agent_message" && event.type === "item.completed") {
          said.push(item.text ?? "");
        }
      }
    }
  }
  return calls.size > 0 ? turn() : Result.fail(warned ?? "codex ended without finishing the turn");
};

/** Whether `codex exec --json` output so far holds the whole first turn. */
export const codexSettled = (events: ReadonlyArray<CodexEvent>): Settled => {
  const last = events.at(-1);
  if (last?.type === "turn.completed" || last?.type === "turn.failed") return "now";
  if (!events.some(isCall)) return "no";
  return isCall(last) || last === undefined || !("item" in last) ? "quiet" : "now";
};

const CLAUDE: Cli<ClaudeEvent> = {
  name: "claude-cli",
  command: "claude",
  args: (model, files) => [
    "-p", "--setting-sources", "", "--disable-slash-commands", "--no-session-persistence", "--model", model,
    "--system-prompt-file", files.system, "--tools", "", "--mcp-config", files.mcp, "--strict-mcp-config",
    "--max-turns", "1", "--output-format", "stream-json", "--verbose",
  ],
  line: ClaudeLine,
  settled: claudeSettled,
  parse: parseClaude,
};

const CODEX_FEATURES = [
  "multi_agent", "apps", "tool_suggest", "plugins", "memories", "shell_tool", "image_generation", "view_image", "sleep_tool",
  "browser_use", "computer_use", "goals",
];
const CODEX_CONFIG = [
  "model_reasoning_effort=low", "skills.include_instructions=false", "include_environment_context=false",
  "include_permissions_instructions=false", "include_apps_instructions=false", "include_collaboration_mode_instructions=false",
  "project_doc_max_bytes=0", 'web_search="disabled"',
];

const CODEX: Cli<CodexEvent> = {
  name: "codex-cli",
  command: "codex",
  args: (model, files) => [
    "exec", "--json", "-s", "read-only", "--skip-git-repo-check", "--ephemeral", "--ignore-rules", "--ignore-user-config", "-m", model,
    ...CODEX_CONFIG.flatMap((setting) => ["-c", setting]),
    "-c", `model_instructions_file=${JSON.stringify(files.system)}`,
    "-c", 'mcp_servers.ployz.command="node"',
    "-c", `mcp_servers.ployz.args=${JSON.stringify([SERVER, files.tools])}`,
    ...CODEX_FEATURES.flatMap((feature) => ["--disable", feature]),
    "-",
  ],
  line: CodexLine,
  settled: codexSettled,
  parse: parseCodex,
};

/** The output events `cli` printed for `prompt` until its turn settled, or why it gave none. */
const run = <Event>(cli: Cli<Event>, args: ReadonlyArray<string>, cwd: string, prompt: string, signal: AbortSignal | undefined) =>
  new Promise<Result.Result<ReadonlyArray<Event>, string>>((resolve) => {
    const child = spawn(cli.command, args, { cwd, stdio: ["pipe", "pipe", "pipe"] });
    const lines: Array<Event> = [];
    let stderr = "";
    let quiet: NodeJS.Timeout | undefined;
    const abort = () => finish(Result.fail(`${cli.name} was interrupted`));
    const deadline = setTimeout(() => finish(Result.fail(`${cli.name} gave no answer within ${TIMEOUT_MS / 1000} s`)), TIMEOUT_MS);
    function finish(outcome: Result.Result<ReadonlyArray<Event>, string>) {
      clearTimeout(deadline);
      clearTimeout(quiet);
      signal?.removeEventListener("abort", abort);
      child.kill();
      resolve(outcome);
    }
    signal?.addEventListener("abort", abort);
    readline.createInterface({ input: child.stdout }).on("line", (line) => {
      const event = Schema.decodeUnknownOption(cli.line)(line);
      if (Option.isNone(event)) return;
      lines.push(event.value);
      clearTimeout(quiet);
      const settled = cli.settled(lines);
      if (settled === "now") finish(Result.succeed(lines));
      else if (settled === "quiet") quiet = setTimeout(() => finish(Result.succeed(lines)), QUIET_MS);
    });
    child.stderr.on("data", (chunk: Buffer) => {
      stderr += chunk.toString();
    });
    child.on("error", (error) => finish(Result.fail(`${cli.name}: ${error.message}`)));
    child.on("close", (code) =>
      finish(lines.length > 0 ? Result.succeed(lines) : Result.fail(`${cli.name} exited ${code}: ${stderr.trim().split("\n").at(-1) ?? ""}`)));
    child.stdin.end(prompt);
  });

type Options = TextOptions<Record<string, never>>;

const ToolInput = Schema.Struct({
  properties: Schema.optionalKey(Schema.Record(Schema.String, Schema.Unknown)),
  required: Schema.optionalKey(Schema.Array(Schema.String)),
});

/** `options` asked of `cli` once, in an empty directory of its own that is removed after. */
const ask = async <Event>(cli: Cli<Event>, model: string, options: Options): Promise<Result.Result<Turn, string>> => {
  const dir = await mkdtemp(join(tmpdir(), "ployz-eval-"));
  try {
    const files: Files = { cwd: join(dir, "cwd"), system: join(dir, "system.md"), tools: join(dir, "tools.json"), mcp: join(dir, "mcp.json") };
    const tools = (options.tools ?? []).map((tool) => {
      const { properties = {}, required = [] }: typeof ToolInput.Type = Option.getOrElse(Schema.decodeUnknownOption(ToolInput)(tool.inputSchema), () => ({}));
      return { name: tool.name, description: tool.description, inputSchema: { type: "object", properties, required } };
    });
    await mkdir(files.cwd);
    await writeFile(files.system, normalizeSystemPrompts(options.systemPrompts).map(({ content }) => content).join("\n\n"));
    await writeFile(files.tools, JSON.stringify(tools));
    await writeFile(files.mcp, JSON.stringify({ mcpServers: { ployz: { command: "node", args: [SERVER, files.tools] } } }));
    const lines = await run(cli, cli.args(model, files), files.cwd, render(options.messages, tools.length > 0 ? "tools" : "chat"), options.request?.signal ?? undefined);
    return Result.flatMap(lines, cli.parse);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
};

/** `turn` as the chunks the Anthropic adapter streams for the same reply. */
export function* chunksOf(turn: Turn, run: { readonly runId: string; readonly threadId: string; readonly model: string }): Generator<AdapterYieldChunk> {
  const { runId, threadId, model } = run;
  const timestamp = Date.now();
  const messageId = crypto.randomUUID();
  if (turn.text !== "") {
    yield { type: EventType.TEXT_MESSAGE_START, messageId, model, timestamp, role: "assistant" };
    yield { type: EventType.TEXT_MESSAGE_CONTENT, messageId, model, timestamp, delta: turn.text, content: turn.text };
    yield { type: EventType.TEXT_MESSAGE_END, messageId, model, timestamp };
  }
  for (const [index, call] of turn.calls.entries()) {
    const args = JSON.stringify(call.input);
    const named = { toolCallId: call.id, toolCallName: call.name, toolName: call.name, model, timestamp };
    yield { type: EventType.TOOL_CALL_START, ...named, parentMessageId: messageId, index };
    yield { type: EventType.TOOL_CALL_ARGS, toolCallId: call.id, model, timestamp, delta: args, args };
    yield { type: EventType.TOOL_CALL_END, ...named, input: call.input };
  }
  yield { type: EventType.RUN_FINISHED, runId, threadId, model, timestamp, finishReason: turn.calls.length > 0 ? "tool_calls" : "stop" };
}

/** A CLI as the adapter needs it: named, and asked for one turn at a time. */
type Asker = { readonly name: string; readonly command: string; readonly ask: (model: string, options: Options) => Promise<Result.Result<Turn, string>> };

const asker = <Event>(cli: Cli<Event>): Asker => ({ name: cli.name, command: cli.command, ask: (model, options) => ask(cli, model, options) });

/** A model answered by a coding-agent CLI on the member's own subscription, one process per call, with no API key. */
export class CliAdapter extends BaseTextAdapter<string, Record<string, never>, readonly ["text"], DefaultMessageMetadataByModality> {
  readonly name: string;

  constructor(private readonly cli: Asker, model: string) {
    super(undefined, model);
    this.name = cli.name;
  }

  async *chatStream(options: Options): AsyncIterable<AdapterYieldChunk> {
    const { runId = crypto.randomUUID(), threadId = crypto.randomUUID() } = options;
    const model = this.model;
    yield { type: EventType.RUN_STARTED, runId, threadId, model, timestamp: Date.now() };
    const turn = await this.cli.ask(model, options);
    if (Result.isFailure(turn)) {
      const error = { message: turn.failure, code: this.cli.name };
      yield { type: EventType.RUN_ERROR, model, timestamp: Date.now(), ...error, error };
      return;
    }
    yield* chunksOf(turn.success, { runId, threadId, model });
  }

  structuredOutput(): Promise<StructuredOutputResult<unknown>> {
    return Promise.reject(new Error(`${this.cli.name} writes no structured output.`));
  }
}

const CLIS = { "claude-cli": asker(CLAUDE), "codex-cli": asker(CODEX) };

/** Model `name` served by `provider`'s CLI, or why that CLI can't run here. */
export const cliModel = (provider: keyof typeof CLIS, name: string): Result.Result<CliAdapter, string> => {
  const cli = CLIS[provider];
  const probe = spawnSync(cli.command, ["--version"], { stdio: "ignore" });
  return probe.status === 0 ? Result.succeed(new CliAdapter(cli, name)) : Result.fail(`${name} needs the ${cli.command} CLI on PATH`);
};
