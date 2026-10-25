import { describe, expect, it } from "vitest";
import type { ConfigCommand } from "@ployz/sdk";
import { DATABASE_PRESETS, GENERATED_PASSWORD, databaseCommand, type DatabasePreset } from "./database-presets";

const environment = { project: "shop", environment: "production" };
const ids = { service: "00000000-0000-4000-8000-000000000003", volume: "00000000-0000-4000-8000-000000000004",
  volumeName: "postgres-2-data" };

function preset(id: DatabasePreset["id"]) {
  const found = DATABASE_PRESETS.find((candidate) => candidate.id === id);
  if (!found) throw new Error(`no ${id} preset`);
  return found;
}

/** The Batch's commands and the value its one patch sets. */
function parts(command: ConfigCommand) {
  if (command.command !== "batch") throw new Error("expected a batch");
  const [service, volume, edit] = command.commands;
  const patch = edit?.command === "edit" ? edit.changes[0] : undefined;
  if (patch?.op !== "patch") throw new Error("expected one patch");
  return { service, volume, patch };
}

describe("databaseCommand", () => {
  it("creates the Service, its mounted Volume and its exported variables in one Batch", () => {
    const { service, volume, patch } = parts(databaseCommand(preset("postgres"),
      { ...ids, environment, name: "postgres-2" }));
    expect(service).toMatchObject({ command: "create_service", id: ids.service, name: "postgres-2",
      image: "ghcr.io/railwayapp-templates/postgres-ssl:18" });
    expect(volume).toMatchObject({ command: "create_volume", id: ids.volume, name: "postgres-2-data",
      mounts: [{ service: "postgres-2", path: "/var/lib/postgresql/data" }] });
    expect(patch.path).toBe("postgres-2");
    expect(patch.value).toMatchObject({ env: {
      POSTGRES_PASSWORD: { value: '${{ secret(32, "a-zA-Z") }}', exported: true },
      PGHOST: { value: "${{ PLOYZ_PRIVATE_DOMAIN }}", exported: true },
      DATABASE_URL: {
        value: "postgresql://${{ PGUSER }}:${{ POSTGRES_PASSWORD }}@${{ PLOYZ_PRIVATE_DOMAIN }}:5432/${{ PGDATABASE }}",
        exported: true,
      },
    } });
    expect(patch.value).not.toHaveProperty("startCommand");
  });

  it.each(DATABASE_PRESETS.map((preset) => [preset.id, preset]))("checks %s's health with a command, at the default timeout", (_, preset) => {
    const { patch } = parts(databaseCommand(preset, { ...ids, environment, name: preset.id }));
    expect(patch.value).toHaveProperty("healthcheck", { command: preset.healthcheck });
  });

  // A sealed password would be Not set in a Branch copy or PR Environment, and a carried one would be shared with
  // production. The Store generates one per Environment, so each New, empty Volume initializes with its own.
  it.each(DATABASE_PRESETS.map((preset) => [preset.id, preset]))("asks the Store to generate %s's password", (_, preset) => {
    const { patch } = parts(databaseCommand(preset, { ...ids, environment, name: preset.id }));
    const env = Object.fromEntries(Object.entries(preset.env).map(([key, value]) => [key, { value, exported: true }]));
    expect(Object.values(env)).toContainEqual({ value: GENERATED_PASSWORD, exported: true });
    expect(patch.value).toHaveProperty("env", env);
  });

  it("starts Redis with its password", () => {
    const { patch } = parts(databaseCommand(preset("redis"), { ...ids, environment, name: "redis" }));
    expect(patch.value).toMatchObject({ startCommand: expect.stringContaining("--requirepass \"$REDIS_PASSWORD\"") });
  });
});
