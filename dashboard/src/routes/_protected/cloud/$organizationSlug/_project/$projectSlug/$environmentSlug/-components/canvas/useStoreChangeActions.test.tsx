// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { JsonValue, RowId } from "@ployz/sdk";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import * as lens from "#/modules/runtime/use-runtime-lens";
import { useStoreChangeActions } from "./useStoreChangeActions";

afterEach(() => {
  admittedIds.length = 0;
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

function Deploy() {
  const { deploy, discard, neverSync, dialog } = useStoreChangeActions("acme", ref, "2:1:0", (id) => admittedIds.push(id));
  return <>
    <button type="button" onClick={() => deploy("  Ship the api  ")}>Deploy now</button>
    <button type="button" onClick={() => discard("web.replicas")}>Discard replicas</button>
    <button type="button" onClick={() => neverSync("api.env.CACHE_TTL", "a:variables.CACHE_TTL" as RowId)}>Never sync CACHE_TTL</button>
    {dialog}
  </>;
}

function setup() {
  const queryClient = new QueryClient();
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  // SAFETY: the hook reads only the Servers' ids and names.
  vi.spyOn(lens, "useRuntimeLens").mockReturnValue({ machines: [{ id: "m1", name: "hetzner-1" }] } as never);
  // SAFETY: after a write the writer refetches views these tests never read.
  vi.spyOn(functions, "readStoreViewServerFn").mockResolvedValue({ ok: true, value: {} } as never);
  const write = vi.spyOn(functions, "writeStoreServerFn");
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  render(<Deploy />, { wrapper });
  const admits = () => write.mock.calls.map(([call]) => call?.data.command);
  return { write, admits };
}

it("asks before a Deploy deletes Volume data, then admits accepting exactly what the user read", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("confirmation_required", loss)).mockResolvedValueOnce(admitted);

  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  // Every Volume that goes, on the Servers holding it; the user types where.
  await screen.findByText("pg-data");
  expect(screen.getByText("hetzner-1")).toBeTruthy();
  fireEvent.change(screen.getByPlaceholderText("shop/production"), { target: { value: "shop/production" } });
  fireEvent.click(screen.getByRole("button", { name: "Deploy" }));

  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.admits()).toMatchObject([
    { accept_volume_loss: [], version: "2:1:0", message: "Ship the api" },
    { accept_volume_loss: ["pg-data"], version: "9:1:0.1", message: "Ship the api" },
  ]);
  await waitFor(() => expect(screen.queryByText("pg-data")).toBeNull());
  expect(toast.error).not.toHaveBeenCalled();
  // The admitted Deploy, the second admission's own id, opens.
  expect(admittedIds).toHaveLength(1);
  expect(test.admits()[1]).toMatchObject({ id: admittedIds[0] });
});

it("discards a Setting by its Store path, through the Environment's queue", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "discarded" } } as never);

  act(() => { fireEvent.click(screen.getByText("Discard replicas")); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  expect(test.admits()).toEqual([{ command: "discard", environment: ref, path: "web.replicas", version: "2:1:0" }]);
});

it("discards an arrived change, then marks its row Never sync here, so it goes and nothing follows into it again", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "discarded" } } as never)
    .mockResolvedValueOnce({ ok: true, value: { written: "never_synced" } } as never);

  act(() => { fireEvent.click(screen.getByText("Never sync CACHE_TTL")); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.admits()).toEqual([
    { command: "discard", environment: ref, path: "api.env.CACHE_TTL", version: "2:1:0" },
    { command: "never_sync", environment: ref, rows: ["a:variables.CACHE_TTL"] },
  ]);
});

it("marks nothing when the discard is refused", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("conflict", null));

  act(() => { fireEvent.click(screen.getByText("Never sync CACHE_TTL")); });
  await waitFor(() => expect(toast.error).toHaveBeenCalled());
  expect(test.write).toHaveBeenCalledTimes(1);
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

it("shows the Store's reason a Deploy waits, naming each secret without a value", async () => {
  const test = setup();
  const message = "production has secrets without a value: set api.env.KEY, web.env.API_KEY before deploying";
  test.write.mockResolvedValueOnce({ ok: false, refusal: { code: "conflict", message, details: { secrets: ["api.env.KEY", "web.env.API_KEY"] } } });

  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await waitFor(() => expect(toast.error).toHaveBeenCalledWith(message));
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
