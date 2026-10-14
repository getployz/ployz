import { existsSync, readFileSync } from "node:fs";
import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { OPERATION_TOOLS } from "#/modules/operations/operations";
import { STORE_COMMANDS } from "#/modules/operations/store-commands";

const ROOT = new URL("../../../../", import.meta.url);
const SRC = new URL("../../", import.meta.url);
const read = (base: URL, path: string) => readFileSync(new URL(path, base), "utf8");

const AgentTools = Schema.fromJsonString(Schema.Struct({
  tools: Schema.Array(Schema.Struct({ name: Schema.String, read_only: Schema.Boolean })),
}));
const { tools: agentTools } = Schema.decodeUnknownSync(AgentTools)(read(ROOT, "core/crates/ployz-store/tests/snapshots/agent_tools.json"));

const payloads = read(ROOT, "core/crates/ployz-sdk/generated/payloads.d.ts");
const union = payloads.slice(payloads.indexOf("export type ConfigCommand ="));
const commands = [...union.slice(0, union.indexOf(";\n")).matchAll(/"command": "([a-z_]+)"/g)].map((match) => match[1] ?? "");

const cliCatalog = read(ROOT, "core/crates/ployz/tests/cli_shape.rs");
const pathList = cliCatalog.slice(cliCatalog.indexOf("assert_eq!(\n        paths,"));
const cliPaths = [...pathList.slice(0, pathList.indexOf("]")).matchAll(/"([a-z -]+)"/g)].map((match) => match[1] ?? "");

type Surface = { readonly paths: readonly string[] } | { readonly tools: readonly string[] } | { readonly sender: string }
  | { readonly absent: string; readonly quotedFrom: string } | { readonly gap: string };

function drift(): string[] {
  const problems: string[] = [];
  const registry: Record<string, Record<"cli" | "agent" | "ui", Surface>> = STORE_COMMANDS;
  for (const command of commands) if (!(command in registry)) problems.push(`${command}: no entry`);
  for (const name of Object.keys(registry)) if (!commands.includes(name)) problems.push(`${name}: not a Store command`);

  const claimed = new Set<string>();
  for (const [command, surfaces] of Object.entries(registry)) {
    for (const [surface, declared] of Object.entries(surfaces)) {
      if ("absent" in declared && !read(ROOT, declared.quotedFrom).includes(declared.absent)) {
        problems.push(`${command} ${surface}: ${declared.quotedFrom} does not say "${declared.absent}"`);
      }
      if ("paths" in declared) {
        for (const path of declared.paths) if (!cliPaths.includes(path)) problems.push(`${command} cli: no command "${path}"`);
      }
      if ("tools" in declared) {
        for (const tool of declared.tools) {
          claimed.add(tool);
          const found = agentTools.find(({ name }) => name === tool);
          if (found === undefined) problems.push(`${command} agent: tool ${tool} is not in agent_tools.json`);
          else if (found.read_only) problems.push(`${command} agent: tool ${tool} is read-only`);
        }
      }
      if ("sender" in declared) {
        const sends = `command: "${command}"`;
        if (!existsSync(new URL(declared.sender, SRC))) problems.push(`${command} ui: no file ${declared.sender}`);
        else if (!read(SRC, declared.sender).includes(sends)) problems.push(`${command} ui: ${declared.sender} has no ${sends}`);
      }
    }
  }

  const operations = new Set<string>(OPERATION_TOOLS);
  for (const { name, read_only } of agentTools) {
    if (!read_only && !operations.has(name) && !claimed.has(name)) problems.push(`agent tool ${name}: writes the Store but no command claims it`);
  }
  return problems;
}

describe("store command parity", () => {
  it("maps every Store command to real surfaces, cites every deliberate absence, and claims every Store-writing tool", () => {
    expect(drift()).toEqual([]);
  });

  it("reads real surfaces, so a stale registry cannot pass by reading nothing", () => {
    expect(commands).toHaveLength(45);
    expect(cliPaths).toContain("env never-sync");
    expect(agentTools.some(({ name }) => name === "config_write")).toBe(true);
  });
});
