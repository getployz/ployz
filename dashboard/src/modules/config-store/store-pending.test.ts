import { expect, it } from "vitest";
import { marksOf } from "./store-pending";

const environment = { project: "shop", environment: "production" };

it("echoes only a literal set from an edit, marks a renamed variable by its old path, and leaves unsets and patches to the Store's rows", () => {
  expect(marksOf({ command: "edit", environment, expect: 2, changes: [
    { op: "set", path: "web.replicas", value: 3 },
    { op: "unset", path: "web.startCommand" },
    { op: "rename", path: "web.env.API_URL", to: "PUBLIC_API_URL" },
  ] })).toEqual([
    { mark: "value", environment, path: "web.replicas", value: 3 },
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

it("marks a Save and both sides of a Sync as busy, a Cancel as nothing, and a Batch as the marks of its commands", () => {
  expect(marksOf({ command: "publish", environment, version: null })).toEqual([{ mark: "busy", environment, why: "saving" }]);
  const into = { project: "shop", environment: "staging" };
  expect(marksOf({ command: "sync", from: environment, into, version: "4:2:1.0" }))
    .toEqual([{ mark: "busy", environment, why: "syncing" }, { mark: "busy", environment: into, why: "syncing" }]);
  expect(marksOf({ command: "cancel", deployment: "dep_1" })).toEqual([]);
  expect(marksOf({ command: "batch", environment, commands: [
    { command: "create_service", environment, id: "svc_1", name: "api" },
    { command: "edit", environment, expect: null, changes: [{ op: "set", path: "api.replicas", value: 2 }] },
  ] })).toEqual([
    { mark: "arriving", environment, node: "service", id: "svc_1", name: "api" },
    { mark: "value", environment, path: "api.replicas", value: 2 },
  ]);
});

it("shows nothing for a command whose row only the Store can name", () => {
  expect(marksOf({ command: "add_domain", environment, service: "web", hostname: "shop.example.com", port: null })).toEqual([]);
});
