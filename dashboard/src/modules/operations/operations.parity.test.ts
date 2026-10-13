import { readFileSync } from "node:fs";
import { Schema } from "effect";
import { describe, expect, it } from "vitest";
import { OPERATION_TOOLS, OPERATIONS } from "#/modules/operations/operations";

const AGENT_TOOLS = new URL("../../../../core/crates/ployz-store/tests/snapshots/agent_tools.json", import.meta.url);
const CLI_HANDLER = new URL("../../routes/api/cli/-cli.handler.ts", import.meta.url);

/** Fields that address or key a call rather than carry an option. */
const ADDRESSING = {
  agent: new Set(["request_key"]),
  cli: new Set(["kind"]),
  // `stops` is what a Drain's confirm showed as stopping: the dialog's approval, like `approvalId`.
  ui: new Set(["organizationSlug", "requestId", "approvalId", "stops"]),
} as const;

const AgentTools = Schema.fromJsonString(Schema.Struct({
  tools: Schema.Array(Schema.Struct({
    name: Schema.String,
    read_only: Schema.Boolean,
    input_schema: Schema.Struct({ properties: Schema.optional(Schema.Record(Schema.String, Schema.Struct({}))) }),
  })),
}));
const { tools: agentTools } = Schema.decodeUnknownSync(AgentTools)(readFileSync(AGENT_TOOLS, "utf8"));
const cliHandler = readFileSync(CLI_HANDLER, "utf8");

type Surface = "cli" | "agent" | "ui";
type Carried = { readonly via: string | readonly string[] } | { readonly omitted: string };
type Present = { readonly options: Readonly<Record<string, Carried>> };

const vias = (carried: Carried) => ("via" in carried ? [carried.via].flat() : []);

/** The fields one surface of one operation has, or why it has none. */
function fieldsOf(surface: Surface, present: Present) {
  if (surface === "agent") {
    const { tool } = present as Present & { readonly tool: string };
    const found = agentTools.find(({ name }) => name === tool);
    return { fields: Object.keys(found?.input_schema.properties ?? {}), where: `agent tool ${tool}` };
  }
  if (surface === "cli") {
    const { route, body } = present as Present & { readonly route: string; readonly body: { readonly fields: object } | null };
    const params = route.split(/[ /]/).filter((segment) => segment.startsWith(":"));
    return { fields: [...params, ...Object.keys(body?.fields ?? {})], where: `CLI ${route}` };
  }
  const { fn, input } = present as Present & { readonly fn: string; readonly input: { readonly fields: object } };
  return { fields: Object.keys(input.fields), where: `${fn}'s input` };
}

/** Every way a surface disagrees with the registry, one line each naming the operation, surface and option. */
function drift(): string[] {
  const problems: string[] = [];
  const agentClaims = new Map<string, Set<string>>();
  for (const [operation, definition] of Object.entries(OPERATIONS)) {
    const options = Object.keys(definition.options.fields);
    for (const surface of ["cli", "agent", "ui"] as const) {
      const declared = definition.surfaces[surface];
      if ("absent" in declared) continue;
      const present = declared as Present;
      const { fields, where } = fieldsOf(surface, present);
      const claimed = new Set<string>();
      for (const option of options) {
        const carried = present.options[option];
        if (carried === undefined) {
          problems.push(`${operation} ${surface} ${option}: no mapping; carry it with via or declare it omitted`);
          continue;
        }
        for (const field of vias(carried)) {
          claimed.add(field);
          if (!fields.includes(field)) problems.push(`${operation} ${surface} ${option}: via "${field}" is not a field of ${where}`);
        }
      }
      if (surface === "agent") {
        const { tool } = present as Present & { readonly tool: string };
        const tools = agentClaims.get(tool) ?? new Set();
        for (const field of claimed) tools.add(field);
        agentClaims.set(tool, tools);
        const found = agentTools.find(({ name }) => name === tool);
        if (found === undefined) problems.push(`${operation} agent: tool ${tool} is not in agent_tools.json`);
        else if (found.read_only) problems.push(`${operation} agent: tool ${tool} is read-only`);
        continue;
      }
      if (surface === "cli") {
        const { route } = present as Present & { readonly route: string };
        if (!cliHandler.includes(`case "${route}"`)) problems.push(`${operation} cli: -cli.handler.ts has no case "${route}"`);
      }
      for (const field of fields) {
        if (!claimed.has(field) && !ADDRESSING[surface].has(field)) {
          problems.push(`${operation} ${surface}: ${where} has "${field}", which carries no option`);
        }
      }
    }
  }
  const idempotent = new Set<unknown>(Object.values(OPERATIONS).map(({ idempotency }) => idempotency));
  for (const tool of OPERATION_TOOLS) {
    if (!idempotent.has(tool)) problems.push(`operation tool ${tool}: no operation is idempotent under it`);
  }
  const operationTools = new Set<string>(OPERATION_TOOLS);
  for (const tool of agentTools) {
    if (tool.read_only || !operationTools.has(tool.name)) continue;
    const claimed = agentClaims.get(tool.name);
    if (claimed === undefined) {
      problems.push(`agent tool ${tool.name}: no operation carries it`);
      continue;
    }
    for (const field of Object.keys(tool.input_schema.properties ?? {})) {
      if (!claimed.has(field) && !ADDRESSING.agent.has(field)) {
        problems.push(`agent tool ${tool.name}: "${field}" carries no option of any operation`);
      }
    }
  }
  return problems;
}

describe("operation parity", () => {
  it("carries every option of every operation on each surface, or says why not, and leaves no surface field unclaimed", () => {
    expect(drift()).toEqual([]);
  });

  it("reads real surfaces, so a stale registry cannot pass by reading nothing", () => {
    const swept = agentTools.filter(({ name, read_only }) => !read_only && (OPERATION_TOOLS as readonly string[]).includes(name)).map(({ name }) => name);
    expect(swept).toEqual(expect.arrayContaining(["server_forget", "service_restart", "service_start", "service_stop"]));
    expect(cliHandler).toContain(`case "POST servers/:id/forget"`);
  });
});
