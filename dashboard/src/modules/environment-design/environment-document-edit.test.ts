// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import { getEnvironmentsCollection } from "#/collections/collections";
import { preloadCollection } from "#/collections/query-collection";
import { getDbClient } from "#/collections/scope";
import { renderHook } from "@testing-library/react";
import * as scopes from "#/collections/use-collection-scope";
import type { CollectionScope } from "#/collections/scope";
import { editEnvironmentDocument, editEnvironmentDocumentAfter, useEnvironmentDocumentQueue } from "./environment-document-edit";

import { emptyEnvironmentIntent } from "./saved-intent";

function getEditorForTest(scope: CollectionScope) {
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  return renderHook(() => useEnvironmentDocumentQueue("acme")).result.current;
}

const clients: QueryClient[] = [];
afterEach(async () => {
  for (const client of clients.splice(0)) { await getDbClient(client).cleanup(); client.clear(); }
  vi.restoreAllMocks();
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function setup() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity, retry: false } } });
  clients.push(queryClient);
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const row = { id: "env", projectId: "project", organizationId: "org", name: "Production", namespace: "production",
    revision: "r1", intent: { ...emptyEnvironmentIntent("production"), volumes: [{ resourceId: "vol", resourceLineageId: "lin", name: "data" }] },
    createdAt: new Date(0), updatedAt: new Date(0) };
  queryClient.setQueryData(["collections", "session", "user", "acme", "environment"], [row]);
  const environments = getEnvironmentsCollection("acme", scope);
  await preloadCollection(environments);
  const rename = (name: string) => (intent: { volumes: Array<{ name: string }> }) => { const volume = intent.volumes[0]; if (volume) volume.name = name; };
  const saved = (revision: string, name: string) => ({ data: { ...row, revision, intent: { ...row.intent, volumes: [{ ...row.intent.volumes[0], name }] } } });
  // SAFETY: the fixture row carries the fields these tests read; the collection stores rows as given.
  return { scope, environments, rename, saved: saved as (revision: string, name: string) => { data: never } };
}

it("shows the edit at once and saves queued edits against the previous save's revision", async () => {
  const test = await setup();
  const first = deferred<{ data: never }>();
  const save1 = vi.fn(() => first.promise);
  const save2 = vi.fn(async (revision: string) => test.saved(revision === "r2" ? "r3" : "stale", "logs"));

  const one = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("cache"), save: save1, failureMessage: "x" });
  expect(test.environments.get("env")?.intent.volumes[0]?.name).toBe("cache");
  const two = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("logs"), save: save2, failureMessage: "x" });
  expect(test.environments.get("env")?.intent.volumes[0]?.name).toBe("logs");

  await vi.waitFor(() => expect(save1).toHaveBeenCalledWith("r1"));
  expect(save2).not.toHaveBeenCalled();
  first.resolve(test.saved("r2", "cache"));
  await Promise.all([one.isPersisted.promise, two.isPersisted.promise]);
  expect(save2).toHaveBeenCalledWith("r2");
  // The committed row replaces the optimistic snapshot once the transaction settles.
  await vi.waitFor(() => expect(test.environments.get("env")?.revision).toBe("r3"));
  expect(test.environments.get("env")?.intent.volumes[0]?.name).toBe("logs");
});

it("rolls back and toasts when a save fails, and the next edit reuses the unchanged revision", async () => {
  const toastError = vi.spyOn(toast, "error").mockReturnValue("toast");
  const test = await setup();
  const failed = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("cache"),
    save: () => Promise.reject(new Error("Name already taken")), failureMessage: "Could not rename." });
  await expect(failed.isPersisted.promise).rejects.toThrow("Name already taken");
  expect(toastError).toHaveBeenCalledWith("Name already taken");
  expect(test.environments.get("env")?.intent.volumes[0]?.name).toBe("data");

  const save = vi.fn(async (revision: string) => test.saved(revision === "r1" ? "r2" : "stale", "logs"));
  await editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("logs"), save, failureMessage: "x" }).isPersisted.promise;
  expect(save).toHaveBeenCalledWith("r1");
});

it("keeps the queue on the last saved revision when a queued save fails", async () => {
  vi.spyOn(toast, "error").mockReturnValue("toast");
  const test = await setup();
  const first = deferred<{ data: never }>();
  const saves: string[] = [];
  const one = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("a"), failureMessage: "x",
    save: (revision) => { saves.push(revision); return first.promise; } });
  const two = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("b"), failureMessage: "x",
    save: async (revision) => { saves.push(revision); throw new Error("Invalid"); } });
  const three = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("c"), failureMessage: "x",
    save: async (revision) => { saves.push(revision); return test.saved("r3", "c"); } });
  first.resolve(test.saved("r2", "a"));
  await one.isPersisted.promise;
  await expect(two.isPersisted.promise).rejects.toThrow("Invalid");
  await three.isPersisted.promise;
  expect(saves).toEqual(["r1", "r2", "r2"]);
});

it("runs an enqueued command after queued edits, against their revision, and settles only when all are done", async () => {
  const test = await setup();
  const scope = test.scope;
  const first = deferred<{ data: never }>();
  editEnvironmentDocument("acme", scope, { environmentId: "env", apply: test.rename("a"), failureMessage: "x", save: () => first.promise });
  const discard = vi.fn(async (revision: string) => test.saved(revision === "r2" ? "r3" : "stale", "data"));
  const { enqueue, settled } = getEditorForTest(scope);
  const discarded = enqueue({ environmentId: "env", save: discard, failureMessage: "x" });
  let isSettled = false;
  const settling = settled("env").then(() => { isSettled = true; });
  await Promise.resolve();
  expect(discard).not.toHaveBeenCalled();
  expect(isSettled).toBe(false);
  first.resolve(test.saved("r2", "a"));
  await discarded.isPersisted.promise;
  await settling;
  expect(discard).toHaveBeenCalledWith("r2");
  expect(test.environments.get("env")?.revision).toBe("r3");
});

it("still saves an edit that changes nothing in memory", async () => {
  const test = await setup();
  const save = vi.fn(async (revision: string) => test.saved(revision === "r1" ? "r2" : "stale", "data"));
  await editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("data"), save, failureMessage: "x" }).isPersisted.promise;
  expect(save).toHaveBeenCalledWith("r1");
});

it("waits for an edit that is still preparing before settling or running a command", async () => {
  const test = await setup();
  const prepared = deferred<void>();
  const saves: string[] = [];
  editEnvironmentDocumentAfter("acme", test.scope, async () => {
    await prepared.promise;
    return { environmentId: "env", apply: test.rename("a"), failureMessage: "x",
      save: async (revision) => { saves.push(`edit@${revision}`); return test.saved("r2", "a"); } };
  }, "x");
  const { enqueue, settled } = getEditorForTest(test.scope);
  let isSettled = false;
  const settling = settled("env").then(() => { isSettled = true; });
  const discarded = enqueue({ environmentId: "env", failureMessage: "x",
    save: async (revision) => { saves.push(`discard@${revision}`); return test.saved("r3", "data"); } });
  await Promise.resolve();
  expect(isSettled).toBe(false);
  expect(saves).toEqual([]);
  prepared.resolve();
  await settling;
  await discarded.isPersisted.promise;
  expect(saves).toEqual(["edit@r1", "discard@r2"]);
});

it("moves the queue to the saved revision even when follow-up work fails", async () => {
  vi.spyOn(toast, "error").mockReturnValue("toast");
  const test = await setup();
  const saves: string[] = [];
  const one = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("a"), failureMessage: "x",
    save: async (revision) => { saves.push(revision); return test.saved("r2", "a"); },
    afterSave: async () => { throw new Error("Reconcile failed"); } });
  const two = editEnvironmentDocument("acme", test.scope, { environmentId: "env", apply: test.rename("b"), failureMessage: "x",
    save: async (revision) => { saves.push(revision); return test.saved("r3", "b"); } });
  await expect(one.isPersisted.promise).rejects.toThrow("Reconcile failed");
  await two.isPersisted.promise;
  expect(saves).toEqual(["r1", "r2"]);
});
