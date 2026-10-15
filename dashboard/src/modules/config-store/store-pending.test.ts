import type { RowId } from "@ployz/sdk";
import { expect, it } from "vitest";
import { busyIn, marksOf, showPending } from "./store-pending";

const environment = { project: "shop", environment: "production" };

it("echoes a literal set and unset from an edit, marks a renamed variable by its old path, and leaves patches to the Store's rows", () => {
  expect(marksOf({ command: "edit", environment, expect: 2, changes: [
    { op: "set", path: "web.replicas", value: 3 },
    { op: "unset", path: "web.startCommand" },
    { op: "rename", path: "web.env.API_URL", to: "PUBLIC_API_URL" },
  ] })).toEqual([
    { mark: "value", environment, path: "web.replicas", value: 3 },
    { mark: "unset", environment, path: "web.startCommand" },
    { mark: "renamed", environment, node: "variable", from: "web.env.API_URL", to: "PUBLIC_API_URL" },
  ]);
});

it("marks a Discard's clicked line as leaving and the Environment as discarding", () => {
  expect(marksOf({ command: "discard", environment, path: "web.replicas", version: "4:2:1.0" })).toEqual([
    { mark: "leaving", environment, scope: { path: "web.replicas", target: "head" } },
    { mark: "busy", environment, why: "discarding" },
  ]);
  expect(marksOf({ command: "discard", environment, path: null, target: "saved", version: null })[0])
    .toEqual({ mark: "leaving", environment, scope: { path: null, target: "saved" } });
});

it("marks a removed node as leaving, a created node as arriving, and a rename by its labels", () => {
  expect(marksOf({ command: "remove_service", environment, service: "web" }))
    .toEqual([{ mark: "leaving", environment, scope: { node: "service", name: "web" } }]);
  expect(marksOf({ command: "create_service", environment, id: "svc_1", name: "api" }))
    .toEqual([{ mark: "arriving", environment, node: "service", id: "svc_1", name: "api" }]);
  expect(marksOf({ command: "rename_service", environment, service: "web", name: "site" }))
    .toEqual([{ mark: "renamed", environment, node: "service", from: "web", to: "site" }]);
  expect(marksOf({ command: "rename_project", project: "shop", name: "store" }))
    .toEqual([{ mark: "project_renamed", from: "shop", to: "store" }]);
});

it("marks a Save, a Deploy and both sides of a Sync as busy, a Cancel by its Deployment, and a Batch as the marks of its commands", () => {
  expect(marksOf({ command: "publish", environment, version: null })).toEqual([{ mark: "busy", environment, why: "saving" }]);
  const into = { project: "shop", environment: "staging" };
  expect(marksOf({ command: "sync", from: environment, into, version: "4:2:1.0" }))
    .toEqual([{ mark: "busy", environment, why: "syncing" }, { mark: "busy", environment: into, why: "syncing" }]);
  expect(marksOf({ command: "admit", admit: "deploy", id: "dep_1", environment })).toEqual([{ mark: "busy", environment, why: "deploying" }]);
  expect(marksOf({ command: "cancel", deployment: "dep_1" })).toEqual([{ mark: "cancel", deployment: "dep_1" }]);
  expect(marksOf({ command: "batch", environment, commands: [
    { command: "create_service", environment, id: "svc_1", name: "api" },
    { command: "edit", environment, expect: null, changes: [{ op: "set", path: "api.replicas", value: 2 }] },
  ] })).toEqual([
    { mark: "arriving", environment, node: "service", id: "svc_1", name: "api" },
    { mark: "value", environment, path: "api.replicas", value: 2 },
  ]);
});

it("names a pending domain by the literal it sends, and none when the Store names it", () => {
  expect(marksOf({ command: "add_domain", environment, service: "web", hostname: "shop.example.com", port: null }))
    .toEqual([{ mark: "domain", environment, service: "web", name: "shop.example.com" }]);
  expect(marksOf({ command: "add_domain", environment, service: "web", hostname: null, port: null }))
    .toEqual([{ mark: "domain", environment, service: "web", name: null }]);
});

it("marks an On merge write by its row and a PR plan write by the fields it sets", () => {
  expect(marksOf({ command: "set_merge_value", environment, row: "w:env.A" }))
    .toEqual([{ mark: "merge_value", environment, row: "w:env.A" }]);
  expect(marksOf({ command: "set_pr_plan", project: "shop", repository: "acme/web", enabled: true, start_from: null, copy: null,
    setup: null, remove_on_close: false, include_bots: null }))
    .toEqual([{ mark: "pr_plan", project: "shop", repository: "acme/web", change: { enabled: true, remove_on_close: false } }]);
});

it("shows a Never sync on the Environment's list at once, and a row synced again off it", () => {
  const id = (row: string) => row as RowId;
  const view = { view: "environment" as const, environment: { id: "env", project: "shop", name: "production", revision: 3 }, settings: [],
    never_synced: [id("w:env.A"), id("w:env.B")] };
  const marks = [
    ...marksOf({ command: "never_sync", environment, rows: ["w:env.C"] }),
    ...marksOf({ command: "never_sync", environment, rows: ["w:env.A"], off: true }),
  ];
  expect(showPending(view, marks).never_synced).toEqual(["w:env.B", "w:env.C"]);
});

it("shows an unset row at its default and leaves a renamed variable's rows to the Store's answer", () => {
  const view = { view: "environment" as const, environment: { id: "env", project: "shop", name: "production", revision: 3 }, settings: [
    { path: "web.source", value: { github: "acme/web" }, default: null, apply: "staged" as const },
    { path: "web.env.API_URL", value: "https://api", default: null, apply: "staged" as const },
    { path: "web.env.API_URL.exported", value: true, default: false, apply: "staged" as const },
  ] };
  const marks = marksOf({ command: "edit", environment, expect: 3, changes: [
    { op: "unset", path: "web.source" },
    { op: "rename", path: "web.env.API_URL", to: "PUBLIC_API_URL" },
  ] });
  expect(showPending(view, marks).settings.map(({ path, value }) => ({ path, value }))).toEqual([
    { path: "web.source", value: null },
    { path: "web.env.API_URL", value: "https://api" },
    { path: "web.env.API_URL.exported", value: true },
  ]);
});

it("reports a Deploy as pending while an older Discard in the Environment still is", () => {
  const marks = [
    ...marksOf({ command: "discard", environment, path: "web.replicas", version: "4:2:1.0" }),
    ...marksOf({ command: "admit", admit: "deploy", id: "dep_1", environment }),
  ];
  expect(busyIn(marks, environment)).toEqual(new Set(["discarding", "deploying"]));
  expect(busyIn(marks, { project: "shop", environment: "staging" })).toEqual(new Set());
});
