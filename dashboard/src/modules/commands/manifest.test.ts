import { readFileSync, writeFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { CATALOG, GATED } from "#/modules/agent/agent-tools";
import { inputSchema, manifest } from "#/modules/commands/commands.server";
import { COMMANDS } from "#/modules/commands/registry.server";

const FIXTURE = new URL("../../../../core/crates/ployz/tests/fixtures/published-manifest.json", import.meta.url);

describe("published command manifest", () => {
  it("matches the core fixture ployz is tested against, so a changed command cannot ship with a stale fixture", () => {
    const served = manifest();
    if (process.env["PLOYZ_WRITE_MANIFEST_FIXTURE"] === "1") writeFileSync(FIXTURE, `${JSON.stringify(served, null, 2)}\n`);
    const fixture: unknown = JSON.parse(readFileSync(FIXTURE, "utf8"));
    expect(served, "core's published-manifest.json is stale: run `pnpm manifest:fixture` in dashboard/ and commit it").toEqual(fixture);
  });

  it("serves each command's own input schema, so ployz never builds flags for a different input", () => {
    const served = new Map(manifest().commands.map((entry) => [entry["name"], entry]));
    for (const command of COMMANDS) {
      expect(served.get(command.name)?.["input"], command.name).toEqual(inputSchema(command));
      expect(inputSchema(command)).toMatchObject({ type: "object", additionalProperties: false });
    }
  });

  it("names only real input fields as positional, so a positional argument never lands in a field that does not exist", () => {
    for (const command of COMMANDS) {
      expect(Object.keys(command.input.fields), command.name).toEqual(expect.arrayContaining([...command.positional]));
    }
  });

  it("gives every command its own name, never a compiled tool's, so neither shadows the other in ployz or the sidebar", () => {
    const names = COMMANDS.map((command) => command.name);
    expect(new Set(names).size).toBe(names.length);
    const compiled = new Set(CATALOG.tools.map((tool) => tool.name));
    expect(names.filter((name) => compiled.has(name))).toEqual([]);
  });

  it("gates exactly the commands that ask a human, so a destructive one never runs in the sidebar unreviewed", () => {
    for (const command of COMMANDS) expect(GATED.has(command.name), command.name).toBe(command.gate === "ask");
  });
});
