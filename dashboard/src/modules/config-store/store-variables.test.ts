import { expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { EnvironmentRef, EnvironmentView, RowId } from "@ployz/sdk";
import { serviceSettingRows } from "./store-services";
import { serviceVariables, storeVariableWriter } from "./store-variables";
import { withPendingChanges } from "./store-view.queries";

const view: EnvironmentView = {
  environment: { id: "env", project: "shop", name: "production", revision: 3 },
  settings: [
    { path: "web.registryCredential", value: { secret: true }, default: null, apply: "immediate" },
    { path: "web.env.API_KEY", value: { secret: true }, default: null, apply: "staged" },
    { path: "web.env.API_KEY.exported", value: false, default: false, apply: "staged" },
    { path: "web.env.LOG_LEVEL", value: "info", default: null, apply: "staged" },
    { path: "web.env.LOG_LEVEL.exported", value: true, default: false, apply: "staged" },
  ],
};

const variables = (shown: EnvironmentView) => serviceVariables(serviceSettingRows(shown, "web"), "web-id")
  .map(({ key, value, exported }) => ({ key, exported, value: value.type === "plain" ? value.value : "sealed" }));

it("lists a Service's variables from its rows, secrets sealed", () => {
  expect(variables(view)).toEqual([
    { key: "API_KEY", exported: false, value: "sealed" },
    { key: "LOG_LEVEL", exported: true, value: "info" },
  ]);
});

it("shows pending variable writes at once, and a new secret only as sealed", () => {
  const shown = withPendingChanges(view, [
    { op: "set", path: "web.env.DATABASE_URL", value: "postgres://db" },
    { op: "set", path: "web.env.DATABASE_URL.exported", value: true },
    { op: "set", path: "web.env.LOG_LEVEL", value: { secret: "hunter2" } },
    { op: "set", path: "web.env.TOKEN", value: { secret: "s3cret" } },
    { op: "set", path: "web.env.TOKEN.exported", value: false },
    { op: "unset", path: "web.env.API_KEY" },
    { op: "set", path: "web.registryCredential", value: { username: "ada", secret: "ghp_rotated" } },
  ]);
  expect(variables(shown)).toEqual([
    { key: "DATABASE_URL", exported: true, value: "postgres://db" },
    { key: "LOG_LEVEL", exported: true, value: "sealed" },
    { key: "TOKEN", exported: false, value: "sealed" },
  ]);
  expect(JSON.stringify(shown)).not.toMatch(/hunter2|s3cret|ghp_rotated/u);
  expect(shown.settings.find((row) => row.path === "web.registryCredential")?.value).toEqual({ secret: true });
});

it("says which secret needs a value, and keeping it keeps it so", () => {
  const valueless = { ...view, settings: [{ path: "web.env.TOKEN", value: { secret: false }, default: null, apply: "staged" as const }] };
  const needs = (shown: EnvironmentView) => serviceVariables(serviceSettingRows(shown, "web"), "web-id")
    .map(({ key, value }) => ({ key, needsValue: value.type === "sealed" && value.needsValue }));
  expect(needs(valueless)).toEqual([{ key: "TOKEN", needsValue: true }]);
  expect(needs(withPendingChanges(valueless, [{ op: "set", path: "web.env.TOKEN", value: { secret: true } }])))
    .toEqual([{ key: "TOKEN", needsValue: true }]);
  expect(needs(withPendingChanges(valueless, [{ op: "set", path: "web.env.TOKEN", value: { secret: "s3cret" } }])))
    .toEqual([{ key: "TOKEN", needsValue: false }]);
});

it("marks the variables their Environment never syncs, by their rows", () => {
  const row = (at: string) => `w:variables.${at}` as RowId;
  const marked = { ...view, settings: view.settings.map((setting) => {
    const key = /^web\.env\.([^.]+)$/u.exec(setting.path)?.[1];
    return key ? { ...setting, row: row(key) } : setting;
  }), never_synced: [row("LOG_LEVEL"), "a:variables.API_KEY" as RowId] };
  expect(serviceVariables(serviceSettingRows(marked, "web"), "web-id", new Map(), new Set(marked.never_synced))
    .map(({ key, neverSynced }) => ({ key, neverSynced }))).toEqual([
    { key: "API_KEY", neverSynced: false },
    { key: "LOG_LEVEL", neverSynced: true },
  ]);
});

it("shows a pending rename under its new name, value, seal, export and Never sync mark kept", () => {
  const row = "w:variables.API_KEY" as RowId;
  const marked = { ...view, settings: view.settings.map((setting) => setting.path === "web.env.API_KEY" ? { ...setting, row } : setting), never_synced: [row] };
  const shown = withPendingChanges(marked, [
    { op: "rename", path: "web.env.API_KEY", to: "stripe_key" },
    { op: "rename", path: "web.env.LOG_LEVEL", to: "LOG" },
  ]);
  expect(serviceVariables(serviceSettingRows(shown, "web"), "web-id", new Map(), new Set(shown.never_synced))
    .map(({ key, value, exported, neverSynced }) => ({ key, exported, neverSynced, value: value.type === "plain" ? value.value : "sealed" })))
    .toEqual([
      { key: "LOG", exported: true, neverSynced: false, value: "info" },
      { key: "STRIPE_KEY", exported: false, neverSynced: true, value: "sealed" },
    ]);
});

it("shows a rename onto a name in use as nothing, since the Store refuses it", () => {
  const shown = withPendingChanges(view, [{ op: "rename", path: "web.env.API_KEY", to: "LOG_LEVEL" }]);
  expect(variables(shown)).toEqual(variables(view));
});

it("sends a rename as one Store edit on the variable's path", async () => {
  const edit = vi.fn().mockResolvedValue(undefined);
  const environment: EnvironmentRef = { project: "shop", environment: "production" };
  const writer = storeVariableWriter(asTestDouble<Parameters<typeof storeVariableWriter>[0]>()({ edit }), environment, "web", [], new Map());
  await writer.rename("API_KEY", "STRIPE_KEY");
  expect(edit).toHaveBeenCalledExactlyOnceWith({ environment, changes: [{ op: "rename", path: "web.env.API_KEY", to: "STRIPE_KEY" }] });
});
