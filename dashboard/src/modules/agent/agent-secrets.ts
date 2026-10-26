import { Schema } from "effect";
import type { BatchCommand, Change, ConfigCommand, JsonValue } from "@ployz/sdk";
import type { JsonObject } from "#/db/tables";
import { asRecord } from "#/lib/json";
import type { StoreRefusal } from "#/modules/config-store/store.contract";

const holdsText = Schema.is(Schema.Struct({ secret: Schema.String }));
const holdsCredential = Schema.is(Schema.Struct({ value: Schema.Struct({ secret: Schema.String }) }));

/** A value sealed in a browser: never an agent's, whatever else it holds. */
function sealedEnvelope(value: JsonValue | undefined): boolean {
  if (Array.isArray(value)) return value.some(sealedEnvelope);
  const object = asRecord(value);
  return object !== null && (Object.hasOwn(object, "sealedFor") || Object.values(object).some(sealedEnvelope));
}

const isText = Schema.is(Schema.String);

/** Text anywhere in `value`. */
function anyText(value: JsonValue | undefined): boolean {
  if (isText(value)) return true;
  if (Array.isArray(value)) return value.some(anyText);
  return Object.values(asRecord(value) ?? {}).some(anyText);
}

/**
 * Whether `value` holds a secret's text: text anywhere under a `secret` field, however deep, as the Rust
 * `holds_secret_text` reads it. A refusal never hands such a call back as its retry (#3473).
 */
export function holdsSecretText(value: JsonValue | undefined): boolean {
  if (Array.isArray(value)) return value.some(holdsSecretText);
  return Object.entries(asRecord(value) ?? {}).some(([field, inner]) => (field === "secret" && anyText(inner)) || holdsSecretText(inner));
}

/** A secret's value as the Rust Service tool finds one in what it sets (`tools/service.rs` `secret_value`). */
const suppliesSecret = (value: JsonValue | undefined) => holdsText(value) || holdsCredential(value);

/** The Setting path of the first secret value `change` supplies, as `SERVICE.env.KEY`. */
function suppliedPath(change: Change): string | null {
  if (change.op === "set") return suppliesSecret(change.value) ? change.path : null;
  const set = change.op === "patch" ? asRecord(change.value) : null;
  if (set === null) return null;
  const service = change.path.replace(/\.$/u, "");
  if (suppliesSecret(set["registryCredential"])) return `${service}.registryCredential`;
  const env = asRecord(set["env"]) ?? {};
  const key = Object.keys(env).find((name) => suppliesSecret(env[name]));
  return key === undefined ? null : `${service}.env.${key}`;
}

const values = (change: Change): JsonValue[] => change.op === "set" || change.op === "patch" ? [change.value] : [];

/** The first secret `command` carries that no agent may: a browser-sealed value, or a secret's value as text. */
function carried(command: ConfigCommand | BatchCommand): { path: string | null } | null {
  switch (command.command) {
    case "edit": {
      if (command.changes.some((change) => values(change).some(sealedEnvelope))) return { path: null };
      const path = command.changes.map(suppliedPath).find((found) => found !== null);
      return path === undefined ? null : { path };
    }
    case "sync": return sealedEnvelope(command.values ?? {}) ? { path: null } : null;
    case "set_merge_value": return sealedEnvelope(command.value ?? null) ? { path: null } : null;
    case "batch": return command.commands.map(carried).find((found) => found !== null) ?? null;
    default: return null;
  }
}

/**
 * Refuses an agent's write that carries a secret's value, in the words the Rust Service tool refuses one with: the
 * member sets it in the dashboard under Variables, or with `ployz set … --secret`, which goes in `details.command`
 * because the tool cuts a CLI line from the message the model hears. A Keep (`{"secret": true}`), Not set (`false`)
 * and a generated one (`{"secret": {"generate": true}}`) pass.
 */
export function agentSecretRefusal(command: ConfigCommand): StoreRefusal | null {
  const found = carried(command);
  if (found === null) return null;
  const said = found.path === null ? "A secret's value never passes through an agent." : `${found.path} is a secret, and its value never passes through an agent.`;
  const details: JsonObject = { command: `printf %s "$VALUE" | ployz set ${found.path ?? "SERVICE.env.KEY"} --secret` };
  if (found.path !== null) details["path"] = found.path;
  return { code: "invalid_argument", message: `${said} The member sets it in the dashboard under Variables.`, details };
}
