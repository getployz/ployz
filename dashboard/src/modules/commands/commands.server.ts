import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { JsonValue } from "@ployz/sdk";
import type { JsonObject } from "#/db/tables";
import { Effect, Schema } from "effect";
import { asRecord, asString, canonicalJson } from "#/lib/json";
import { type CommandCall, type Ran, refused } from "#/modules/commands/command.server";
import { COMMANDS, RUNNABLE } from "#/modules/commands/registry.server";
import type { Caller } from "#/modules/identity/actor";

export type Published = (typeof COMMANDS)[number];
type Runnable = (typeof RUNNABLE)[number];
export type CommandName = Runnable["name"];
type OutputOf<N extends string> = Extract<Runnable, { name: N }>["output"]["Type"];

const EMPTY_INPUT: JsonObject = { type: "object", properties: {}, required: [], additionalProperties: false };

/** `{anyOf: [{type: "string"}, {type: "null"}]}` as `{type: ["string", "null"]}`, the one form `ployz` reads. */
function nullableAsTypes(value: JsonValue): JsonValue {
  if (Array.isArray(value)) return value.map(nullableAsTypes);
  const node = asRecord(value);
  if (node === null) return value;
  const walked = Object.fromEntries(Object.entries(node).map(([key, item]) => [key, nullableAsTypes(item)]));
  const [first, second, ...rest] = Array.isArray(walked["anyOf"]) ? walked["anyOf"] : [];
  const nonNull = asRecord(first ?? null);
  const isNull = asRecord(second ?? null);
  const type = asString(nonNull?.["type"] ?? null);
  if (rest.length > 0 || nonNull === null || isNull === null || type === null || Object.keys(nonNull).length !== 1) return walked;
  if (Object.keys(isNull).length !== 1 || isNull["type"] !== "null") return walked;
  const { anyOf: _anyOf, ...siblings } = walked;
  return { ...siblings, type: [type, "null"] };
}

function jsonSchema(schema: Schema.Top): JsonObject {
  return asRecord(nullableAsTypes(asRecord(Schema.toJsonSchemaDocument(schema).schema))) ?? {};
}

/** Effect writes an empty struct as "object or array" and drops an empty `required`; `ployz` reads neither. */
export function inputSchema(command: Published): JsonObject {
  if (Object.keys(command.input.fields).length === 0) return EMPTY_INPUT;
  const schema = jsonSchema(command.input);
  return { ...schema, required: schema["required"] ?? [] };
}

function entry(command: Published): JsonObject {
  return {
    name: command.name,
    path: [...command.path],
    about: command.about,
    examples: [...command.examples],
    kind: command.kind,
    gate: command.gate,
    removes: command.removes,
    agents: command.agents,
    typed: command.typed,
    positional: [...command.positional],
    input: inputSchema(command),
    output: jsonSchema(command.output),
    render: command.render,
  };
}

/** What `ployz` reads to learn Cloud's commands. `revision` changes whenever any entry does. */
export function manifest() {
  const commands = COMMANDS.map(entry);
  const revision = createHash("sha256").update(canonicalJson(commands)).digest("hex").slice(0, 12);
  return { format: 1, revision, commands } as const;
}

export const MANIFEST_REVISION = manifest().revision;

/** On every `/api/cli` reply, so `ployz` sees its cached manifest is stale on whatever call it makes next. */
export const MANIFEST_HEADER = "x-ployz-manifest";

export function findCommand(name: string): Runnable | undefined {
  return RUNNABLE.find((command) => command.name === name);
}

/** Runs one published command for `caller`: the CLI route and the sidebar both come through here. */
export function runCommand<N extends string>(caller: Caller, name: N, raw: JsonValue, call: CommandCall) {
  const command = findCommand(name);
  if (command === undefined) {
    return Effect.succeed(refused("unknown_command", `Cloud has no command ${name}. Update ployz to see what it offers.`));
  }
  // SAFETY: `command.name === name`, so its output is the output the name selects.
  return command.invoke(caller, raw, call) as Effect.Effect<Ran<OutputOf<N>>, Effect.Error<ReturnType<Runnable["invoke"]>>, Effect.Services<ReturnType<Runnable["invoke"]>>>;
}
