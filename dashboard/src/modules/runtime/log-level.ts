import { Option, Schema } from "effect";

export type LogLevel = "error" | "warn" | "info" | "debug";

const NAMED = new Map<string, LogLevel>([
  ["error", "error"], ["err", "error"], ["fatal", "error"], ["critical", "error"], ["crit", "error"],
  ["panic", "error"], ["alert", "error"], ["emerg", "error"], ["emergency", "error"],
  ["warn", "warn"], ["warning", "warn"],
  ["info", "info"], ["notice", "info"], ["information", "info"],
  ["debug", "debug"], ["trace", "debug"], ["dbug", "debug"],
]);

const named = (value: string) => NAMED.get(value.toLowerCase());
const trimAscii = (text: string) => text.replace(/^[ \t\n\r\f]+|[ \t\n\r\f]+$/g, "");

const decodeFields = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Record(Schema.String, Schema.Unknown)));
const isText = Schema.is(Schema.String);

function jsonLevel(line: string) {
  if (!line.startsWith("{")) return undefined;
  const fields = decodeFields(line);
  if (Option.isNone(fields)) return undefined;
  const entries = Object.entries(fields.value);
  for (const key of ["level", "lvl", "severity"]) {
    const value = entries.find(([name]) => name.toLowerCase() === key)?.[1];
    const level = isText(value) ? named(value) : undefined;
    if (level !== undefined) return level;
  }
  return undefined;
}

function logfmtLevel(line: string) {
  for (const pair of line.split(/[ \t\n\r\f]/)) {
    const at = pair.indexOf("=");
    if (at < 0 || pair.slice(0, at).toLowerCase() !== "level") continue;
    const value = pair.slice(at + 1);
    const opened = value.startsWith('"') ? value.slice(1) : value;
    const level = named(opened.endsWith('"') ? opened.slice(0, -1) : value);
    if (level !== undefined) return level;
  }
  return undefined;
}

const isTimestamp = (token: string) => /^[0-9]/.test(token) && /[-/:]/.test(token) && /^[0-9\-/:.,+TZ]+$/.test(token);

function skipTimestamp(line: string) {
  for (;;) {
    const at = line.indexOf(" ");
    if (at < 0 || !isTimestamp(line.slice(0, at))) return line;
    line = line.slice(at + 1).replace(/^[ \t\n\r\f]+/, "");
  }
}

function leadingLevel(line: string) {
  const rest = skipTimestamp(line);
  if (rest.startsWith("[")) {
    const close = rest.indexOf("]");
    return close < 0 ? undefined : named(rest.slice(1, close));
  }
  const word = /^[A-Za-z]*/.exec(rest)?.[0] ?? "";
  const next = rest[word.length];
  return next !== undefined && /[A-Za-z0-9]/.test(next) ? undefined : named(word);
}

/**
 * The severity shown for one line of container output, by the rules the Machine's reader uses
 * (`core/crates/ployz-core/src/log_level.rs`): a JSON `level`, `lvl` or `severity` key, a logfmt `level=`, then a
 * leading level word or `[tag]` after any timestamp. Anything else is `info`, whichever stream it came from.
 */
export function logLevel(text: string): LogLevel {
  const line = trimAscii(text);
  return jsonLevel(line) ?? logfmtLevel(line) ?? leadingLevel(line) ?? "info";
}
