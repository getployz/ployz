import { readFileSync } from "node:fs";
import { EventType, type ModelMessage } from "@tanstack/ai";
import { Option, Result, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { chunksOf, ClaudeLine, CodexLine, codexSettled, parseClaude, parseCodex, render, stripServer } from "./cli";

const recorded = <Event>(name: string, line: Schema.Codec<Event, string>): ReadonlyArray<Event> =>
  readFileSync(new URL(`./cli-fixtures/${name}.jsonl`, import.meta.url), "utf8")
    .trim()
    .split("\n")
    .flatMap((text) => Option.toArray(Schema.decodeUnknownOption(line)(text)));

const setting = (service: string) => ({ service, path: "LOG_LEVEL", value: "debug" });

describe("parseClaude", () => {
  it("keeps every parallel tool call of the first message, without the server prefix", () => {
    expect(parseClaude(recorded("claude-calls", ClaudeLine))).toEqual(Result.succeed({
      text: "",
      calls: [
        { id: "toolu_01Br13NjMAAQhfE4XBBpeqzG", name: "set_setting", input: setting("web") },
        { id: "toolu_019kuEJ394Sqi2yJNBnjHssp", name: "set_setting", input: setting("worker") },
      ],
    }));
  });

  it("reads a reply with no tool call as text", () => {
    expect(parseClaude(recorded("claude-text", ClaudeLine))).toEqual(Result.succeed({
      text: "I can change the settings of the services I'm connected to, by setting a specified value at a given path within a named service.",
      calls: [],
    }));
  });

  it("fails a run that errored before any tool call", () => {
    const failed = Schema.decodeUnknownSync(ClaudeLine)(JSON.stringify({ type: "result", subtype: "error_during_execution", is_error: true, errors: ["overloaded"] }));
    expect(parseClaude([failed])).toEqual(Result.fail("overloaded"));
    expect(parseClaude([])).toEqual(Result.fail("claude ended without a result"));
  });
});

describe("parseCodex", () => {
  it("ends the turn at its tool calls, ignoring what codex says after they are denied", () => {
    const thread = "01a125e1-2dd4-7bd0-9dda-b1d66889c631";
    expect(parseCodex(recorded("codex-calls", CodexLine))).toEqual(Result.succeed({
      text: "",
      calls: [
        { id: `call_${thread}_item_0`, name: "set_setting", input: setting("web") },
        { id: `call_${thread}_item_1`, name: "set_setting", input: setting("worker") },
      ],
    }));
  });

  it("reads a reply with no tool call as text", () => {
    expect(parseCodex(recorded("codex-text", CodexLine))).toEqual(Result.succeed({
      text: "I can find, organize, and update your services, and help with related tasks.",
      calls: [],
    }));
  });

  it("fails a failed or unfinished turn", () => {
    const started = recorded("codex-text", CodexLine).slice(0, 1);
    const failed = Schema.decodeUnknownSync(CodexLine)(JSON.stringify({ type: "turn.failed", error: { message: "usage limit reached" } }));
    expect(parseCodex([...started, failed])).toEqual(Result.fail("usage limit reached"));
    expect(parseCodex(started)).toEqual(Result.fail("codex ended without finishing the turn"));
  });
});

describe("codexSettled", () => {
  it("waits for more calls after a call, and stops at the first item after the calls", () => {
    const lines = recorded("codex-calls", CodexLine);
    expect(lines.map((_, index) => codexSettled(lines.slice(0, index + 1)))).toEqual(["no", "quiet", "quiet", "quiet", "quiet", "now", "now"]);
  });

  it("waits for a text reply to finish", () => {
    const lines = recorded("codex-text", CodexLine);
    expect(lines.map((_, index) => codexSettled(lines.slice(0, index + 1)))).toEqual(["no", "no", "now"]);
  });
});

describe("stripServer", () => {
  it("removes only the MCP server prefix", () => {
    expect(stripServer("mcp__ployz__service_ls")).toBe("service_ls");
    expect(stripServer("service_ls")).toBe("service_ls");
  });
});

describe("render", () => {
  it("sends a lone user message as itself", () => {
    expect(render([{ role: "user", content: "set web's memory limit to 1 GiB" }], "tools")).toBe("set web's memory limit to 1 GiB");
  });

  it("writes a longer conversation as a transcript with every call, its arguments and its result", () => {
    const messages: Array<ModelMessage> = [
      { role: "user", content: "list services and deploy" },
      {
        role: "assistant",
        content: "Listing first.",
        toolCalls: [
          { id: "toolu_1", type: "function", function: { name: "service_ls", arguments: "" } },
          { id: "toolu_2", type: "function", function: { name: "deploy", arguments: "{\"env\":\"staging\"}" } },
        ],
      },
      { role: "tool", toolCallId: "toolu_1", content: "{\"ok\":true}" },
      { role: "tool", toolCallId: "toolu_2", content: "denied", error: "denied" },
      { role: "user", content: [{ type: "text", content: "why?" }] },
    ];
    expect(render(messages, "tools")).toBe(`The conversation so far, oldest first. Each tool call already ran, and its result follows it.

<user>
list services and deploy
</user>
<assistant>
Listing first.
</assistant>
<tool_call id="toolu_1" name="service_ls">{}</tool_call>
<tool_call id="toolu_2" name="deploy">{"env":"staging"}</tool_call>
<tool_result id="toolu_1">
{"ok":true}
</tool_result>
<tool_result id="toolu_2" error>
denied
</tool_result>
<user>
why?
</user>

Write the assistant's next turn, continuing from the last message. Act through your tools, never by writing a tool call or a transcript tag as text.`);
  });

  it("frames a conversation with no tools as a reply to its last user message", () => {
    const messages: Array<ModelMessage> = [
      { role: "user", content: "(The sidebar opens.)" },
      { role: "assistant", content: "set DB_PASSWORD on web" },
      { role: "user", content: "It is staged in production." },
    ];
    expect(render(messages, "chat")).toBe(`The conversation so far, oldest first.

<user>
(The sidebar opens.)
</user>
<assistant>
set DB_PASSWORD on web
</assistant>
<user>
It is staged in production.
</user>

Write the assistant's next message, in reply to the last user message, as plain text with no transcript tag.`);
  });
});

describe("chunksOf", () => {
  const run = { runId: "run", threadId: "thread", model: "claude-haiku-5-5" };

  it("streams text, then each call under it, then finishes on the calls", () => {
    const chunks = [...chunksOf({ text: "On it.", calls: [{ id: "a", name: "set", input: { x: 1 } }, { id: "b", name: "deploy", input: {} }] }, run)];
    expect(chunks.map((chunk) => chunk.type)).toEqual([
      EventType.TEXT_MESSAGE_START, EventType.TEXT_MESSAGE_CONTENT, EventType.TEXT_MESSAGE_END,
      EventType.TOOL_CALL_START, EventType.TOOL_CALL_ARGS, EventType.TOOL_CALL_END,
      EventType.TOOL_CALL_START, EventType.TOOL_CALL_ARGS, EventType.TOOL_CALL_END,
      EventType.RUN_FINISHED,
    ]);
    expect(chunks.filter((chunk) => chunk.type === EventType.TOOL_CALL_END)).toMatchObject([
      { toolCallId: "a", toolName: "set", input: { x: 1 } },
      { toolCallId: "b", toolName: "deploy", input: {} },
    ]);
    expect(chunks.at(-1)).toMatchObject({ type: EventType.RUN_FINISHED, runId: "run", threadId: "thread", finishReason: "tool_calls" });
  });

  it("finishes a text reply with stop", () => {
    const chunks = [...chunksOf({ text: "Done.", calls: [] }, run)];
    expect(chunks.map((chunk) => chunk.type)).toEqual([
      EventType.TEXT_MESSAGE_START, EventType.TEXT_MESSAGE_CONTENT, EventType.TEXT_MESSAGE_END, EventType.RUN_FINISHED,
    ]);
    expect(chunks.at(-1)).toMatchObject({ finishReason: "stop" });
  });
});
