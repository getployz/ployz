import type { ConfigCommand, JsonValue, RowId } from "@ployz/sdk";
import { expect, it } from "vitest";
import { agentSecretRefusal, holdsSecretText } from "./agent-secrets";

const production = { project: "shop", environment: "production" };
const preview = { project: "shop", environment: "pr-1" };
const sealed = { sealedFor: "0e7c2f3a-5b1d-4c8e-9f60-1a2b3c4d5e6f", value: { sealed: "AAAA", scope: { service: "7d4e9a12-3c5b-4f6e-8a90-b1c2d3e4f5a6" }, key: "TOKEN" } };
const row = "a:variables.TOKEN" as RowId;
const set = (path: string, value: JsonValue): ConfigCommand => ({ command: "edit", environment: production, expect: null, changes: [{ op: "set", path, value }] });
const patch = (value: JsonValue): ConfigCommand => ({ command: "edit", environment: production, expect: null, changes: [{ op: "patch", path: "web", value }] });

const SEALED = {
  code: "invalid_argument",
  message: "A secret's value never passes through an agent. The member sets it in the dashboard under Variables.",
  details: { command: "printf %s \"$VALUE\" | ployz set SERVICE.env.KEY --secret" },
};

it("refuses a value sealed in a browser wherever a command carries one", () => {
  for (const command of [
    set("web.env.TOKEN", { secret: sealed }),
    patch({ env: { TOKEN: { secret: { ...sealed, value: null } } } }),
    { command: "sync", from: preview, into: production, picks: [row], values: { [row]: sealed }, version: "1:a", id: "s", close_after: false },
    { command: "set_merge_value", environment: preview, into: production, row, value: sealed },
    { command: "batch", environment: production, commands: [{ command: "never_sync", environment: production, rows: [row], off: false }, { command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "web.env.TOKEN", value: { secret: sealed } }] }] },
  ] satisfies ConfigCommand[]) {
    expect(agentSecretRefusal(command), command.command).toEqual(SEALED);
  }
});

it("refuses a secret's value as text where the Service tool finds one, naming its path", () => {
  expect(agentSecretRefusal(set("web.env.TOKEN", { secret: "tok_live" }))).toEqual({
    code: "invalid_argument",
    message: "web.env.TOKEN is a secret, and its value never passes through an agent. The member sets it in the dashboard under Variables.",
    details: { path: "web.env.TOKEN", command: "printf %s \"$VALUE\" | ployz set web.env.TOKEN --secret" },
  });
  expect(agentSecretRefusal(patch({ env: { LOG: "info", TOKEN: { secret: "tok_live" } } }))?.details).toMatchObject({ path: "web.env.TOKEN" });
  expect(agentSecretRefusal(patch({ registryCredential: { value: { secret: "pass" } } }))?.details).toMatchObject({ path: "web.registryCredential" });
});

it("passes a Keep, a Not set, a generated secret and plain values", () => {
  for (const command of [
    set("web.env.TOKEN", { secret: true }),
    set("web.env.TOKEN", { secret: false }),
    set("web.env.TOKEN", { secret: { generate: true } }),
    set("web.env.LOG", "info"),
    patch({ env: { LOG: "info", TOKEN: { secret: true } }, replicas: 2 }),
    { command: "set_merge_value", environment: preview, into: production, row, value: "http://c" },
    { command: "set_merge_value", environment: preview, into: production, row },
  ] satisfies ConfigCommand[]) {
    expect(agentSecretRefusal(command)).toBeNull();
  }
});

it("finds a secret's text anywhere a call holds one, so no refusal hands that call back (#3473)", () => {
  const held: JsonValue[] = [
    { env: { TOKEN: { secret: "SENTINEL" } } },
    { env: { TOKEN: { value: { secret: "SENTINEL" }, exported: true } } },
    { role: 7, credential: [{ secret: { sealedFor: "x", value: "SENTINEL" } }] },
  ];
  const clear: JsonValue[] = [
    { env: { TOKEN: { secret: true } } },
    { env: { TOKEN: { secret: { generate: true } } } },
    { project: "shop", environment: "production" },
  ];
  for (const value of held) expect(holdsSecretText(value), JSON.stringify(value)).toBe(true);
  for (const value of clear) expect(holdsSecretText(value), JSON.stringify(value)).toBe(false);
});
