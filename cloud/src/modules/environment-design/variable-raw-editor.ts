import { Data, Result } from "effect";
import { asRecord, asString } from "#/lib/json";
import type { VariableRecord } from "#/modules/environment-design/variables";

const ENV_LINE_RE = /^([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$/;
const VARIABLE_KEY_RE = /^[A-Za-z_][A-Za-z0-9_]*$/;
const SAFE_UNQUOTED_RE = /^[A-Za-z0-9_./:@-]*$/;

export type ParsedEntry = { key: string; value: string };

export class RawEditorParseError extends Data.TaggedError("RawEditorParseError")<{
  message: string;
}> {
  constructor(message: string) {
    super({ message });
  }
}

function quoteEnvValue(value: string): string {
  if (SAFE_UNQUOTED_RE.test(value) && value !== "") {
    return value;
  }
  const escaped = value
    .replaceAll("\\", "\\\\")
    .replaceAll('"', '\\"')
    .replaceAll("\n", "\\n")
    .replaceAll("\r", "\\r")
    .replaceAll("\t", "\\t");
  return `"${escaped}"`;
}

function variableToEntry(variable: VariableRecord): ParsedEntry | null {
  if (variable.value.type !== "plain") {
    return null;
  }

  return {
    key: variable.key,
    value: variable.value.value,
  };
}

function variablesToEntries(variables: VariableRecord[]) {
  return variables.flatMap((variable) => {
    const entry = variableToEntry(variable);
    return entry ? [entry] : [];
  });
}

function isPlainVariable(
  variable: VariableRecord,
): variable is VariableRecord & { value: { type: "plain"; value: string } } {
  return variable.value.type === "plain";
}

export function serializeEntriesToEnv(entries: ParsedEntry[]): string {
  return entries
    .map((entry) => `${entry.key}=${quoteEnvValue(entry.value)}`)
    .join("\n");
}

export function serializeEntriesToJson(entries: ParsedEntry[]): string {
  const out: Record<string, string> = {};
  for (const entry of entries) {
    out[entry.key] = entry.value;
  }
  return JSON.stringify(out, null, 2);
}

export function serializeVariablesToEnv(variables: VariableRecord[]): string {
  return serializeEntriesToEnv(variablesToEntries(variables));
}

export function serializeVariablesToJson(variables: VariableRecord[]): string {
  return serializeEntriesToJson(variablesToEntries(variables));
}

export function getSealedVariableCollisionMessage(key: string) {
  return `You already have a sealed variable named ${key}. Please rename the variable or delete the sealed variable.`;
}

export function findSealedVariableNameCollisions(
  parsed: ParsedEntry[],
  sealedVariables: VariableRecord[],
): string[] {
  const sealedKeys = new Set(
    sealedVariables.flatMap((variable) =>
      variable.value.type === "sealed" ? [variable.key] : [],
    ),
  );
  const seen = new Set<string>();
  const collisions: string[] = [];

  for (const entry of parsed) {
    if (!sealedKeys.has(entry.key) || seen.has(entry.key)) continue;
    seen.add(entry.key);
    collisions.push(entry.key);
  }

  return collisions;
}

function parseQuotedValueOrThrow(
  input: string,
  quote: '"' | "'",
  lineNo: number,
): string {
  let result = "";
  let i = 1;
  while (i < input.length) {
    const ch = input[i];
    if (ch === "\\" && quote === '"' && i + 1 < input.length) {
      const next = input[i + 1];
      if (next === "n") result += "\n";
      else if (next === "r") result += "\r";
      else if (next === "t") result += "\t";
      else result += next;
      i += 2;
      continue;
    }
    if (ch === quote) {
      const tail = input.slice(i + 1).trim();
      if (tail !== "" && !tail.startsWith("#")) {
        throw new RawEditorParseError(
          `Line ${lineNo}: unexpected text after closing ${quote}.`,
        );
      }
      return result;
    }
    result += ch;
    i += 1;
  }
  throw new RawEditorParseError(
    `Line ${lineNo}: unterminated ${quote === '"' ? "double" : "single"}-quoted value.`,
  );
}

function parseEnvValueOrThrow(rawValue: string, lineNo: number): string {
  const value = rawValue.trim();
  if (value === "") return "";
  const first = value[0];
  if (first === '"' || first === "'") {
    return parseQuotedValueOrThrow(value, first, lineNo);
  }
  // Bare value — strip an inline comment if present.
  const commentIdx = value.indexOf(" #");
  const trimmed = commentIdx >= 0 ? value.slice(0, commentIdx) : value;
  return trimmed.trim();
}

export function parseEnv(
  text: string,
): Result.Result<ParsedEntry[], RawEditorParseError> {
  try {
    const entries: ParsedEntry[] = [];
    const indexByKey = new Map<string, number>();
    const lines = text.split(/\r?\n/);
    for (let i = 0; i < lines.length; i += 1) {
      const lineNo = i + 1;
      const raw = lines[i] ?? "";
      const trimmed = raw.trim();
      if (trimmed === "" || trimmed.startsWith("#")) continue;
      const match = ENV_LINE_RE.exec(trimmed);
      if (!match) {
        throw new RawEditorParseError(`Line ${lineNo}: expected KEY=VALUE.`);
      }
      const key = (match[1] ?? "").toUpperCase();
      const value = parseEnvValueOrThrow(match[2] ?? "", lineNo);
      const existingIndex = indexByKey.get(key);
      if (existingIndex === undefined) {
        indexByKey.set(key, entries.length);
        entries.push({ key, value });
      } else {
        // Duplicate key: last value wins (dotenv semantics), keeping the
        // entry's original position. findDuplicateEnvKeys surfaces a
        // non-blocking notice so the merge isn't silent.
        entries[existingIndex] = { key, value };
      }
    }
    return Result.succeed(entries);
  } catch (cause) {
    if (cause instanceof RawEditorParseError) return Result.fail(cause);
    throw cause;
  }
}

export function parseJson(
  text: string,
): Result.Result<ParsedEntry[], RawEditorParseError> {
  try {
    const trimmed = text.trim();
    if (trimmed === "") return Result.succeed([]);
    let parsed: ReturnType<typeof JSON.parse>;
    try {
      parsed = JSON.parse(trimmed);
    } catch (cause) {
      throw new RawEditorParseError(
        cause instanceof Error ? cause.message : "Invalid JSON.",
      );
    }
    const data = asRecord(parsed);
    if (data === null) {
      throw new RawEditorParseError(
        "Expected a JSON object of key/value pairs.",
      );
    }

    const entries: ParsedEntry[] = [];
    const indexByKey = new Map<string, number>();
    for (const [rawKey, value] of Object.entries(data)) {
      const key = rawKey.toUpperCase();
      if (!VARIABLE_KEY_RE.test(key)) {
        throw new RawEditorParseError(
          `"${rawKey}" is not a valid variable key. Use letters, numbers, and underscores; start with a letter or underscore.`,
        );
      }
      const text = asString(value);
      if (text === null) {
        throw new RawEditorParseError(`"${rawKey}" must be a string.`);
      }
      // Distinct JSON keys can collide once upper-cased (e.g. "foo"/"FOO").
      // Last value wins, mirroring the ENV path; findDuplicateJsonKeys notes it.
      const existingIndex = indexByKey.get(key);
      if (existingIndex === undefined) {
        indexByKey.set(key, entries.length);
        entries.push({ key, value: text });
      } else {
        entries[existingIndex] = { key, value: text };
      }
    }
    return Result.succeed(entries);
  } catch (cause) {
    if (cause instanceof RawEditorParseError) return Result.fail(cause);
    throw cause;
  }
}

/**
 * Keys (upper-cased) that appear more than once in ENV text. Used to surface a
 * non-blocking "merged duplicate keys" notice; parsing keeps the last value.
 * Mirrors parseEnv's line handling but never throws — it ignores invalid lines
 * so the notice stays quiet while the user is mid-edit.
 */
export function findDuplicateEnvKeys(text: string): string[] {
  const counts = new Map<string, number>();
  for (const raw of text.split(/\r?\n/)) {
    const trimmed = raw.trim();
    if (trimmed === "" || trimmed.startsWith("#")) continue;
    const match = ENV_LINE_RE.exec(trimmed);
    if (!match) continue;
    const key = (match[1] ?? "").toUpperCase();
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return [...counts.entries()].flatMap(([key, n]) => (n > 1 ? [key] : []));
}

/**
 * Keys that collide once upper-cased in JSON text (e.g. "foo" and "FOO").
 * JSON.parse already collapses exact-duplicate keys, so only case collisions
 * are observable here. Returns [] for invalid/non-object JSON.
 */
export function findDuplicateJsonKeys(text: string): string[] {
  const trimmed = text.trim();
  if (trimmed === "") return [];
  let parsed: ReturnType<typeof JSON.parse>;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return [];
  }
  const data = asRecord(parsed);
  if (data === null) {
    return [];
  }
  const counts = new Map<string, number>();
  for (const rawKey of Object.keys(data)) {
    const key = rawKey.toUpperCase();
    counts.set(key, (counts.get(key) ?? 0) + 1);
  }
  return [...counts.entries()].flatMap(([key, n]) => (n > 1 ? [key] : []));
}

export type RawEditorDiff = {
  creates: { id: string; key: string; value: string }[];
  updates: { variableId: string; key: string; value: string }[];
  deletes: string[];
};

export function diffVariables(
  parsed: ParsedEntry[],
  current: VariableRecord[],
): RawEditorDiff {
  const editableCurrent = current.filter(isPlainVariable);
  const currentByKey = new Map(editableCurrent.map((v) => [v.key, v]));
  const parsedKeys = new Set(parsed.map((p) => p.key));

  const creates: RawEditorDiff["creates"] = [];
  const updates: RawEditorDiff["updates"] = [];

  for (const entry of parsed) {
    const existing = currentByKey.get(entry.key);
    if (!existing) {
      // Assign the id here so the optimistic insert and the persisted row share
      // a key once the server honours it (see RawEditorDiff / createServiceVariable).
      creates.push({ id: crypto.randomUUID(), key: entry.key, value: entry.value });
      continue;
    }
    if (existing.value.value === entry.value) continue;
    updates.push({
      variableId: existing.id,
      key: entry.key,
      value: entry.value,
    });
  }

  const deletes = editableCurrent.flatMap((variable) =>
    parsedKeys.has(variable.key) ? [] : [variable.id],
  );

  return { creates, updates, deletes };
}
