// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { JsonValue, RowId } from "@ployz/sdk";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import { diffQuery, environmentSettingsQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import * as lens from "#/modules/runtime/use-runtime-lens";
import type { PlacementLoss } from "#/modules/volume-placement/volume-placement";
import { useStoreChangeActions } from "./useStoreChangeActions";

afterEach(() => {
  admittedIds.length = 0;
  saves.length = 0;
  cleanup();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});

const ref = { project: "shop", environment: "production" };
const refusal = (code: string, details: JsonValue) => ({ ok: false as const, refusal: { code, message: `refused: ${code}`, details } });
const loss = { volumes: [{ id: "v", name: "pg-data", docker_volume: "ns_vol-v", deletes: [{ machine_id: "m1", name: "ns_vol-v" }] }], accept: ["pg-data"], version: "9:1:0.1" };
// SAFETY: the hook reads nothing of an admitted Deployment but that it was.
const admitted = { ok: true, value: { written: "deployment", id: "d" } } as never;

const admittedIds: string[] = [];
const saves: boolean[] = [];
const NO_LOSS: PlacementLoss = { items: [], losesWrites: false, accept: [], version: (version) => version };
const shown = (version: string) => `${version}#loss-shown`;

function Deploy({ placementLoss }: { placementLoss: PlacementLoss }) {
  const { deploy, save, discard, neverSync, dialog } = useStoreChangeActions("acme", ref, "2:1:0", (id) => admittedIds.push(id), placementLoss);
  const writer = useStoreWriter("acme");
  return <>
    <button type="button" onClick={() => void writer.edit({ environment: ref, changes: [] }).isPersisted.promise.catch(() => {})}>Stage</button>
    <button type="button" onClick={() => void writer.edit({ environment: { project: "shop", environment: "staging" }, changes: [] }).isPersisted.promise.catch(() => {})}>Stage elsewhere</button>
    <button type="button" onClick={() => deploy("  Ship the api  ")}>Deploy now</button>
    <button type="button" onClick={() => void save("  Save this draft  ").then((saved) => saves.push(saved))}>Save now</button>
    <button type="button" onClick={() => discard("web.replicas")}>Discard replicas</button>
    <button type="button" onClick={() => neverSync("api.env.CACHE_TTL", "a:variables.CACHE_TTL" as RowId)}>Never sync CACHE_TTL</button>
    {dialog}
  </>;
}

function setup(placementLoss = NO_LOSS) {
  const queryClient = new QueryClient();
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  // SAFETY: the hook reads only the Servers' ids and names.
  vi.spyOn(lens, "useRuntimeLens").mockReturnValue({ machines: [{ id: "m1", name: "hetzner-1" }] } as never);
  // SAFETY: after a write the writer refetches views these tests never read.
  vi.spyOn(functions, "readStoreViewServerFn").mockResolvedValue({ ok: true, value: {} } as never);
  const write = vi.spyOn(functions, "writeStoreServerFn");
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  // The Settings this tab shows, at the revision the review "2:1:0" names: what its edits expect.
  const settings = storeViewOptions("acme", { queryClient, sessionId: "session", userId: "user" }, environmentSettingsQuery(ref)).queryKey;
  queryClient.setQueryData(settings, { ok: true, value: { view: "environment", environment: { id: "env", project: "shop", name: "production", revision: 2 }, settings: [] } } as never);
  render(<Deploy placementLoss={placementLoss} />, { wrapper });
  const admits = () => write.mock.calls.map(([call]) => call?.data.command);
  return { write, admits };
}

it("asks before a Deploy deletes Volume data, then admits accepting exactly what the user read", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("confirmation_required", loss)).mockResolvedValueOnce(admitted);

  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await screen.findByText("pg-data");
  expect(screen.getByText("hetzner-1")).toBeTruthy();
  expect(screen.getByText("Deletes data")).toBeTruthy();
  fireEvent.change(screen.getByPlaceholderText("shop/production"), { target: { value: "shop/production" } });
  fireEvent.click(screen.getByRole("button", { name: "Deploy" }));

  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.admits()).toMatchObject([
    { accept_volume_loss: [], version: "2:1:0", message: "Ship the api" },
    { accept_volume_loss: ["pg-data"], version: "9:1:0.1", message: "Ship the api" },
  ]);
  await waitFor(() => expect(screen.queryByText("pg-data")).toBeNull());
  expect(toast.error).not.toHaveBeenCalled();
  expect(admittedIds).toHaveLength(1);
  expect(test.admits()[1]).toMatchObject({ id: admittedIds[0] });
});

it("asks before a Deploy deletes a copy, before anything reaches the Store, then admits accepting the Volume", async () => {
  const test = setup({ items: [{ kind: "volume", name: "pg-data", detail: "web-2" }], losesWrites: false, accept: ["pg-data"], version: shown });
  test.write.mockResolvedValueOnce(admitted);

  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await screen.findByText("web-2");
  expect(screen.getByText("Deletes data")).toBeTruthy();
  expect(test.write).not.toHaveBeenCalled();
  fireEvent.change(screen.getByPlaceholderText("shop/production"), { target: { value: "shop/production" } });
  fireEvent.click(screen.getByRole("button", { name: "Deploy" }));

  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()).toMatchObject([{ admit: "deploy", accept_volume_loss: ["pg-data"], version: "2:1:0#loss-shown", message: "Ship the api" }]);
});

it("titles the ask Data loss when a restore drops writes", async () => {
  setup({ items: [{ kind: "volume", name: "pg-data", detail: "web-1" }], losesWrites: true, accept: ["pg-data"], version: shown });
  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await screen.findByText("Data loss");
  expect(screen.queryByText("Deletes data")).toBeNull();
});

it("titles the ask Data loss when Cloud refuses a Deploy whose staged restore drops writes", async () => {
  const test = setup();
  const restore = { volumes: [{ name: "pg-data", deletes: [{ machine_id: "m1" }], loss: "writes" }], accept: ["pg-data"], version: "9:1:0.1" };
  test.write.mockResolvedValueOnce(refusal("confirmation_required", restore));

  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await screen.findByText("pg-data");
  expect(screen.getByText("Data loss")).toBeTruthy();
  expect(screen.queryByText("Deletes data")).toBeNull();
});

it("discards a Setting by its Store path, through the Environment's queue", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "discarded", environment: { id: "env", revision: 4 } } } as never);

  act(() => { fireEvent.click(screen.getByText("Discard replicas")); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  expect(test.admits()).toEqual([{ command: "discard", environment: ref, path: "web.replicas", version: "2:1:0", target: "head" }]);
});

it("marks an arrived change's row Never sync here and discards it in one Batch, so it goes and nothing follows into it again", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "batch", results: [] } } as never);

  act(() => { fireEvent.click(screen.getByText("Never sync CACHE_TTL")); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  expect(test.admits()).toEqual([{ command: "batch", environment: ref, commands: [
    { command: "never_sync", environment: ref, rows: ["a:variables.CACHE_TTL"] },
    { command: "discard", environment: ref, path: "api.env.CACHE_TTL", version: "2:1:0" },
  ] }]);
});

it("fails closed when the Servers can't be checked: nothing to accept, and the Store's reason shows", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("unavailable", { volumes: ["pg-data"] }));

  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await waitFor(() => expect(toast.error).toHaveBeenCalledWith("refused: unavailable"));
  expect(screen.queryByPlaceholderText("shop/production")).toBeNull();
  expect(test.write).toHaveBeenCalledTimes(1);
  expect(admittedIds).toEqual([]);
});

it("admits one Deployment for a double click", async () => {
  const test = setup();
  let admit: (value: typeof admitted) => void = () => {};
  test.write.mockReturnValueOnce(new Promise<typeof admitted>((resolve) => { admit = resolve; }));

  act(() => {
    fireEvent.click(screen.getByText("Deploy now"));
    fireEvent.click(screen.getByText("Deploy now"));
  });
  admit(admitted);
  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.write).toHaveBeenCalledTimes(1);
});

it("saves the optional message without admitting a deployment", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "published" } } as never);
  fireEvent.click(screen.getByText("Save now"));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  expect(test.admits()).toEqual([{ command: "publish", environment: ref, version: "2:1:0", accept_volume_loss: [], message: "Save this draft" }]);
  expect(admittedIds).toEqual([]);
  await waitFor(() => expect(saves).toEqual([true]));
});

it("reports a Save the Store refused as not saved", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("unavailable", { volumes: ["pg-data"] }));
  fireEvent.click(screen.getByText("Save now"));
  await waitFor(() => expect(saves).toEqual([false]));
  expect(toast.error).toHaveBeenCalledWith("refused: unavailable");
});

it("reports a Save as saved once the user accepts the data it deletes, and not saved when they cancel", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("confirmation_required", loss)).mockResolvedValueOnce({ ok: true, value: { written: "published" } } as never);
  fireEvent.click(screen.getByText("Save now"));
  await screen.findByText("pg-data");
  expect(saves).toEqual([]);
  fireEvent.change(screen.getByPlaceholderText("shop/production"), { target: { value: "shop/production" } });
  fireEvent.click(screen.getByRole("button", { name: "Save only" }));
  await waitFor(() => expect(saves).toEqual([true]));

  test.write.mockResolvedValueOnce(refusal("confirmation_required", loss));
  fireEvent.click(screen.getByText("Save now"));
  await screen.findByText("pg-data");
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(saves).toEqual([true, false]));
});

it("reports an earlier Save as not saved when a later one asks again", async () => {
  setup().write.mockResolvedValue(refusal("confirmation_required", loss));
  fireEvent.click(screen.getByText("Save now"));
  await screen.findByText("pg-data");
  fireEvent.click(screen.getByText("Save now"));
  await waitFor(() => expect(saves).toEqual([false]));
});

// Pressing Deploy leaves a field, which stages its draft in the same gesture. The Store's answer carries the review
// after the edit: `at` is the revision the edit made, `seen` the one the review then shows (newer when someone else wrote).
const edited = (at: number, seen = at) => ({ ok: true, value: { written: "edited", environment: { id: "env", revision: at } },
  answers: [{ query: diffQuery(ref), result: { ok: true, value: { version: `${seen}:1:0` } } }] }) as never;

it("deploys the review that includes a draft staged on the way to Deploy", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(edited(3)).mockResolvedValueOnce(admitted);

  act(() => {
    fireEvent.click(screen.getByText("Stage"));
    fireEvent.click(screen.getByText("Deploy now"));
  });

  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0" });
});

it("keeps the shown review when that draft is refused, so the Store judges what the user saw", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("conflict", null)).mockResolvedValueOnce(admitted);

  act(() => {
    fireEvent.click(screen.getByText("Stage"));
    fireEvent.click(screen.getByText("Deploy now"));
  });

  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "2:1:0" });
});

it("ships nothing this tab never reviewed: a change made elsewhere right after the draft leaves the Deploy at the draft's revision", async () => {
  const test = setup();
  // The edit made revision 3; by the time the Store answered, the CLI had made 4, which the review now shows.
  test.write.mockResolvedValueOnce(edited(3, 4)).mockResolvedValueOnce(admitted);

  act(() => {
    fireEvent.click(screen.getByText("Stage"));
    fireEvent.click(screen.getByText("Deploy now"));
  });

  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0" });
});

it("queues the Deploy at the click: a draft staged after it goes behind it, not into it", async () => {
  const test = setup();
  let first: (value: never) => void = () => {};
  const edits = { count: 0 };
  test.write.mockImplementation(({ data }) => {
    if (data.command.command !== "edit") return Promise.resolve(admitted);
    return edits.count++ === 0 ? new Promise((resolve) => { first = resolve; }) : Promise.resolve(edited(4));
  });

  act(() => {
    fireEvent.click(screen.getByText("Stage"));
    fireEvent.click(screen.getByText("Deploy now"));
    fireEvent.click(screen.getByText("Stage"));
  });
  // The first draft answers only once the Deploy and the second draft are queued behind it.
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  first(edited(3));

  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(3));
  expect(test.admits().map((command) => command?.command)).toEqual(["edit", "admit", "edit"]);
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0" });
});

it("waits on no edit in another Environment", async () => {
  const test = setup();
  // A draft in staging whose save never answers.
  test.write.mockImplementation(({ data }) => data.command.command === "edit" ? new Promise(() => {}) : Promise.resolve(admitted));

  act(() => {
    fireEvent.click(screen.getByText("Stage elsewhere"));
    fireEvent.click(screen.getByText("Deploy now"));
  });

  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()[1]).toMatchObject({ command: "admit", environment: ref, version: "2:1:0" });
});
