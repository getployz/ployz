import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { JsonValue } from "@ployz/sdk";
import type { JsonObject } from "#/db/tables";
import { Effect } from "effect";
import { canonicalJson } from "#/lib/json";
import { type Called, type CommandCall, inputSchema, jsonSchema } from "#/modules/commands/command.server";
import { COMMANDS, RUNNABLE } from "#/modules/commands/registry.server";
import type { Caller } from "#/modules/identity/actor";
import { PERMISSIONS } from "#/modules/organization/permissions";

export type Published = (typeof COMMANDS)[number];
type Runnable = (typeof RUNNABLE)[number];
export type CommandName = Runnable["name"];
type OutputOf<N extends string> = Extract<Runnable, { name: N }>["output"]["Type"];

function entry(command: Published): JsonObject {
  return {
    name: command.name,
    path: [...command.path],
    about: command.about,
    examples: [...command.examples],
    requires: command.requires,
    kind: command.kind,
    gate: command.gate,
    removes: command.removes,
    agents: command.agents,
    typed: command.typed,
    positional: [...command.positional],
    input: inputSchema(command.input),
    output: jsonSchema(command.output),
    render: command.render,
  };
}

/** What `ployz` reads to learn Cloud's commands. `revision` changes whenever any entry does. */
export function manifest() {
  const commands = COMMANDS.map(entry);
  const revision = createHash("sha256").update(canonicalJson(commands)).digest("hex").slice(0, 12);
  return { format: 1, revision, permissions: PERMISSIONS, commands } as const;
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
    const message = `Cloud has no command ${name}. Update ployz to see what it offers.`;
    return Effect.succeed({ ok: false, refusal: { code: "unknown_command", message, details: null, writes: "none" } } as const);
  }
  // SAFETY: `command.name === name`, so its output is the output the name selects.
  return command.invoke(caller, raw, call) as Effect.Effect<Called<OutputOf<N>>, Effect.Error<ReturnType<Runnable["invoke"]>>, Effect.Services<ReturnType<Runnable["invoke"]>>>;
}
