import "@tanstack/react-start/server-only";
import type { Change, ConfigCommand, JsonValue } from "@ployz/sdk";
import { Effect } from "effect";
import { asRecord } from "#/lib/json";
import { listOrganizationServers } from "#/modules/machines/connections.server";
import { StoreRefused } from "./store.contract";

type Server = Effect.Success<ReturnType<typeof listOrganizationServers>>[number];

/** A Machine ID, as `MachineId::parse` takes one. */
const MACHINE_ID = /^[0-9a-f]{32}$/;

/** Whether `path` is a Service's Preferred Builder, `SERVICE.preferredBuilder`. */
const isPreferredBuilder = (path: string) => {
  const [service, setting, ...rest] = path.split(".");
  return service !== undefined && setting === "preferredBuilder" && rest.length === 0;
};

/** Whether a Preferred Builder `value` may name a Server: text but `github`. */
const mayName = (value: JsonValue | undefined): value is string => typeof value === "string" && value !== "github";

/** Unicode white space at either end, which Rust's `str::trim` drops: unlike `String.trim`, U+0085 and not U+FEFF. */
const EDGE_SPACE = /^\p{White_Space}+|\p{White_Space}+$/gu;
const NOT_PRINTABLE = /[\p{Cc}\p{Cf}\p{Cs}\p{Co}\p{Cn}\p{Zl}\p{Zp}\p{Zs}]/u;
const GRAPHEME_EXTEND = /\p{Grapheme_Extend}/u;
const ESCAPED = new Map([["\0", "\\0"], ["\t", "\\t"], ["\r", "\\r"], ["\n", "\\n"], ["\\", "\\\\"], ["'", "\\'"], ['"', '\\"']]);

/** `text` as Rust's `str::escape_debug` writes it, which the CLI's refusals show a typed Server with. */
const escapeDebug = (text: string) => Array.from(text, (char, index) =>
  ESCAPED.get(char) ?? ((index === 0 && GRAPHEME_EXTEND.test(char)) || (char !== " " && NOT_PRINTABLE.test(char))
    ? `\\u{${(char.codePointAt(0) ?? 0).toString(16)}}`
    : char)).join("");

/**
 * `value` trimmed, as a Machine Target: refused as the CLI's `MachineTarget::parse` refuses empty text and the
 * wildcard, before any Server is read.
 */
function target(value: string): Effect.Effect<string, StoreRefused> {
  const text = value.replace(EDGE_SPACE, "");
  if (text !== "" && text !== "*") return Effect.succeed(text);
  return Effect.fail(new StoreRefused({
    code: "invalid_argument",
    message: `invalid Machine Target "${text}": a non-empty Machine identity that is not a wildcard`,
    details: null,
  }));
}

/**
 * Machine Target `text` as the Store takes it, resolved as the CLI's `set` and the `server` commands do: an exact
 * Machine ID of a listed Server, else a Server's name, refused when two Servers share it, else an ID as typed, since a
 * preference needs no live Server, else refused `not_found`.
 */
function resolve(text: string, servers: ReadonlyArray<Server>): Effect.Effect<string, StoreRefused> {
  if (servers.some((server) => server.id === text)) return Effect.succeed(text);
  const [first, ...others] = servers.filter((server) => server.name === text);
  if (first !== undefined && others.length === 0) return Effect.succeed(first.id);
  if (first !== undefined) {
    return Effect.fail(new StoreRefused({
      code: "ambiguous",
      message: `Server name ${escapeDebug(text)} is ambiguous: ${[first, ...others].map((server) => server.id).join(", ")}`,
      details: null,
    }));
  }
  if (MACHINE_ID.test(text)) return Effect.succeed(text);
  return Effect.fail(new StoreRefused({ code: "not_found", message: `Server ${escapeDebug(text)} was not found`, details: null }));
}

/** `change` with the Preferred Builder it sets, by `set` or `patch`, resolved by `to`. */
function withPreferred<E, R>(change: Change, to: (value: string) => Effect.Effect<string, E, R>): Effect.Effect<Change, E, R> {
  if (change.op === "set" && isPreferredBuilder(change.path) && mayName(change.value)) {
    return Effect.map(to(change.value), (value) => ({ ...change, value }));
  }
  const settings = change.op === "patch" && !change.path.includes(".") ? asRecord(change.value) : null;
  const preferred = settings?.["preferredBuilder"];
  if (settings === null || !mayName(preferred)) return Effect.succeed(change);
  return Effect.map(to(preferred), (value) => ({ ...change, value: { ...settings, preferredBuilder: value } }));
}

/**
 * `command` with each Service's Preferred Builder given by a Server's name sent as its Machine ID, which is all the
 * Store holds. Every surface's edit passes here, so a name works from the CLI's `set` and `--patch`, the agents and the
 * dashboard, for every role. Names come from the Servers Cloud keeps, read only when a change may name one; no Cluster
 * is dialed.
 */
export const resolvePreferredBuilders = Effect.fn("ConfigStore.resolvePreferredBuilders")(function* (
  organizationId: string, command: ConfigCommand,
) {
  if (command.command !== "edit") return command;
  const servers = yield* Effect.cached(listOrganizationServers(organizationId));
  const changes = yield* Effect.forEach(command.changes, (change) =>
    withPreferred(change, (value) => Effect.flatMap(target(value), (text) => Effect.flatMap(servers, (listed) => resolve(text, listed)))));
  return { ...command, changes };
});
