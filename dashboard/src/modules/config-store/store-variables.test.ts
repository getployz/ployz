import { expect, it } from "vitest";
import type { EnvironmentView } from "@ployz/sdk";
import { serviceSettingRows } from "./store-services";
import { serviceVariables } from "./store-variables";
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
    { op: "set", path: "web.env.DATABASE_URL", value: { value: "postgres://db", exported: true } },
    { op: "set", path: "web.env.LOG_LEVEL", value: { secret: "hunter2" } },
    { op: "set", path: "web.env.TOKEN", value: { value: { secret: "s3cret" }, exported: false } },
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
