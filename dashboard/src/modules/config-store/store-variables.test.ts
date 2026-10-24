import { expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { ClientSealedValue, EnvironmentRef, EnvironmentView, RowId, ServiceListing } from "@ployz/sdk";
import { serviceSettingRows } from "./store-services";
import {
  notSetVariables,
  receivingService,
  receivingValues,
  serviceVariables,
  storeVariableWriter,
  variableReferrers,
} from "./store-variables";
import { SealError } from "#/modules/sealing/seal-error";
import { withPendingChanges } from "./store-pending";

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
    { op: "set", path: "web.registryCredential", value: { username: "ada", secret: "ghp_rotated" } },
  ]);
  expect(variables(shown)).toEqual([
    { key: "API_KEY", exported: false, value: "sealed" },
    { key: "DATABASE_URL", exported: true, value: "postgres://db" },
    { key: "LOG_LEVEL", exported: true, value: "sealed" },
    { key: "TOKEN", exported: false, value: "sealed" },
  ]);
  expect(JSON.stringify(shown)).not.toMatch(/hunter2|s3cret|ghp_rotated/u);
  expect(shown.settings.find((row) => row.path === "web.registryCredential")?.value).toEqual({ secret: true });
});

it("says which secret is Not set, and keeping it keeps it so", () => {
  const valueless = { ...view, settings: [{ path: "web.env.TOKEN", value: { secret: false }, default: null, apply: "staged" as const }] };
  const needs = (shown: EnvironmentView) => serviceVariables(serviceSettingRows(shown, "web"), "web-id")
    .map(({ key, value }) => ({ key, notSet: value.type === "sealed" && value.notSet }));
  expect(needs(valueless)).toEqual([{ key: "TOKEN", notSet: true }]);
  expect(needs(withPendingChanges(valueless, [{ op: "set", path: "web.env.TOKEN", value: { secret: true } }])))
    .toEqual([{ key: "TOKEN", notSet: true }]);
  expect(needs(withPendingChanges(valueless, [{ op: "set", path: "web.env.TOKEN", value: { secret: "s3cret" } }])))
    .toEqual([{ key: "TOKEN", notSet: false }]);
});

it("names the secrets a deploy skips, until a change to them is staged", () => {
  const valueless = { ...view, settings: [...view.settings, { path: "web.env.TOKEN", value: { secret: false }, default: null, apply: "staged" as const }] };
  expect(notSetVariables(serviceSettingRows(valueless, "web"), new Map())).toEqual(["TOKEN"]);
  expect(notSetVariables(serviceSettingRows(valueless, "web"), new Map([["env.TOKEN", {}]]))).toEqual([]);
  // Exporting it sets no value: the deploy still skips it.
  expect(notSetVariables(serviceSettingRows(valueless, "web"), new Map([["env.TOKEN.exported", true]]))).toEqual(["TOKEN"]);
  // History staged it back without its value: that change sets nothing either.
  expect(notSetVariables(serviceSettingRows(valueless, "web"),
    new Map([["env.TOKEN", { before: null, after: { secret: false } }]]))).toEqual(["TOKEN"]);
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

it("shows a deleted variable gone, and a pending rename under its old name until the Store names it", () => {
  const shown = withPendingChanges(view, [{ op: "rename", path: "web.env.API_KEY", to: "stripe_key" }, { op: "unset", path: "web.env.LOG_LEVEL" }]);
  expect(variables(shown)).toEqual([{ key: "API_KEY", exported: false, value: "sealed" }]);
});

const unsealed = { prepare: vi.fn(), target: { environment: "env", service: "web-id" } };

it("sends a rename as one Store edit on the variable's path", async () => {
  const edit = vi.fn().mockResolvedValue(undefined);
  const environment: EnvironmentRef = { project: "shop", environment: "production" };
  const writer = storeVariableWriter(asTestDouble<Parameters<typeof storeVariableWriter>[0]>()({ edit }), environment, "web", [], new Map(), unsealed);
  await writer.rename("API_KEY", "STRIPE_KEY");
  expect(edit).toHaveBeenCalledExactlyOnceWith({ environment, changes: [{ op: "rename", path: "web.env.API_KEY", to: "STRIPE_KEY" }] });
});

const referenced: EnvironmentView = {
  ...view,
  settings: [
    ...view.settings,
    { path: "web.env.UPSTREAM", value: "${{ LOG_LEVEL }}/v1", default: null, apply: "staged" },
    { path: "worker.env.BACKEND", value: "${{ web.LOG_LEVEL }} ${{ api.LOG_LEVEL }}", default: null, apply: "staged" },
    { path: "worker.env.LITERAL", value: "$${{ web.LOG_LEVEL }} LOG_LEVEL", default: null, apply: "staged" },
    { path: "api.env.LOG_LEVEL", value: "debug", default: null, apply: "staged" },
    { path: "api.env.SELF", value: "${{ LOG_LEVEL }}", default: null, apply: "staged" },
  ],
};

it("finds the variables that reference a variable, in either form, and nothing that only looks like one", () => {
  expect(variableReferrers(referenced, "web", "LOG_LEVEL")).toEqual(["web.UPSTREAM", "worker.BACKEND"]);
  expect(variableReferrers(referenced, "api", "LOG_LEVEL")).toEqual(["worker.BACKEND", "api.SELF"]);
  expect(variableReferrers(referenced, "web", "API_KEY")).toEqual([]);
});

it("sends update_references only when asked", async () => {
  const edit = vi.fn().mockResolvedValue(undefined);
  const environment: EnvironmentRef = { project: "shop", environment: "production" };
  const writer = storeVariableWriter(asTestDouble<Parameters<typeof storeVariableWriter>[0]>()({ edit }), environment, "web", [], new Map(), unsealed);
  await writer.rename("LOG_LEVEL", "LEVEL", true);
  expect(edit).toHaveBeenCalledExactlyOnceWith({
    environment, changes: [{ op: "rename", path: "web.env.LOG_LEVEL", to: "LEVEL", update_references: true }],
  });
});

it("shows a Sealed value only as sealing, and sends what sealing prepares in its place, in order", async () => {
  const edit = vi.fn().mockResolvedValue(undefined);
  const environment: EnvironmentRef = { project: "shop", environment: "production" };
  const sealed = { sealedFor: "env", value: { sealed: "c2VhbGVk", scope: { service: "web-id" }, key: "TOKEN" } };
  const prepare = vi.fn(async (items: readonly { key: string }[]) => items.map(() => sealed));
  const writer = storeVariableWriter(asTestDouble<Parameters<typeof storeVariableWriter>[0]>()({ edit }), environment, "web", [], new Map(),
    { prepare, target: { environment: "env", service: "web-id" } });

  void writer.replace([{ key: "LOG_LEVEL", value: "info" }, { key: "TOKEN", value: "tok_live_123456", sealed: true }, { key: "SALT", value: "salt_value_1", sealed: true }], ["OLD"]);

  const [{ changes, prepare: prepared }] = edit.mock.calls[0] ?? [];
  expect(changes).toEqual([
    { op: "set", path: "web.env.LOG_LEVEL", value: "info" },
    { op: "set", path: "web.env.TOKEN", value: { secret: { sealing: true } } },
    { op: "set", path: "web.env.SALT", value: { secret: { sealing: true } } },
    { op: "unset", path: "web.env.OLD" },
  ]);
  expect(JSON.stringify(changes)).not.toMatch(/tok_live|salt_value/u);
  expect(prepare).not.toHaveBeenCalled();
  const sent = await prepared();
  expect(prepare).toHaveBeenCalledExactlyOnceWith([
    { key: "TOKEN", value: "tok_live_123456", target: { environment: "env", service: "web-id" } },
    { key: "SALT", value: "salt_value_1", target: { environment: "env", service: "web-id" } },
  ]);
  expect(sent).toEqual([
    { op: "set", path: "web.env.LOG_LEVEL", value: "info" },
    { op: "set", path: "web.env.TOKEN", value: { secret: sealed } },
    { op: "set", path: "web.env.SALT", value: { secret: sealed } },
    { op: "unset", path: "web.env.OLD" },
  ]);
  expect(JSON.stringify(sent)).not.toContain("sealing");
});

it("writes a plain variable as before, with nothing to prepare", () => {
  const edit = vi.fn().mockResolvedValue(undefined);
  const environment: EnvironmentRef = { project: "shop", environment: "production" };
  const writer = storeVariableWriter(asTestDouble<Parameters<typeof storeVariableWriter>[0]>()({ edit }), environment, "web", [], new Map(), unsealed);

  void writer.create("LOG_LEVEL", "info", false, true);
  expect(edit).toHaveBeenCalledExactlyOnceWith({ environment, changes: [
    { op: "set", path: "web.env.LOG_LEVEL", value: "info" },
    { op: "set", path: "web.env.LOG_LEVEL.exported", value: true },
  ] });
  expect(unsealed.prepare).not.toHaveBeenCalled();
});

it("finds where a Sync row lands by its node's lineage, never by name, and not in a Service being removed", () => {
  const listing = (row: string, id: string, name: string, change: ServiceListing["change"] = null) =>
    asTestDouble<ServiceListing>()({ row: row as RowId, id, name, change });
  const services = [listing("a:node", "api-there", "web"), listing("b:node", "api-namesake", "api"), listing("c:node", "going", "jobs", "delete")];
  expect(receivingService("a:variables.TOKEN" as RowId, services)?.id).toBe("api-there");
  expect(receivingService("c:variables.TOKEN" as RowId, services)).toBeUndefined();
  expect(receivingService("w:variables.KEY" as RowId, services)).toBeUndefined();
});

it("seals a Sync row for the Service its lineage lands in, keyed by the row's variable, and drops or refuses one that lands nowhere", async () => {
  const services = [asTestDouble<ServiceListing>()({ row: "a:node" as RowId, id: "api-there", name: "web", change: null })];
  const seal = vi.fn(async ({ key, value, target }: { key: string; value: string; target: { environment: string; service: string } }) =>
    asTestDouble<ClientSealedValue>()({ sealedFor: `${key}=${value}@${target.environment}/${target.service}` }));
  const sealing = { sealer: async () => Object.assign(seal, { publicKey: "key" }) };
  const receiver = async () => ({ environment: "env-there", services });
  const typed = [{ row: "a:variables.TOKEN" as RowId, value: "one" }, { row: "b:variables.TOKEN" as RowId, value: "two" }];

  expect(await receivingValues(sealing, typed, receiver, "drop")).toEqual([{ row: "a:variables.TOKEN", value: { sealedFor: "TOKEN=one@env-there/api-there" } }]);
  await expect(receivingValues(sealing, typed, receiver, "refuse")).rejects.toBeInstanceOf(SealError);
  await expect(receivingValues(sealing, [{ row: "a:variables." as RowId, value: "x" }], receiver, "drop")).rejects.toThrow("is not a variable");
  await expect(receivingValues(sealing, [{ row: "a:configs.app" as RowId, value: "x" }], receiver, "drop")).rejects.toThrow("is not a variable");
  expect(await receivingValues({ sealer: async () => null }, typed, receiver, "refuse")).toEqual(typed);
});

