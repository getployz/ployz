// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { JsonValue, RowId } from "@ployz/sdk";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import { useStoreWriter } from "#/modules/config-store/store-write";
import * as lens from "#/modules/runtime/use-runtime-lens";
import type { PlacementLoss, PlacementRow } from "#/modules/volume-placement/volume-placement";
import * as placements from "#/modules/volume-placement/volume-placement.functions";
import { useStagedPlacements } from "#/modules/volume-placement/volume-placement.hooks";
import * as runs from "#/modules/volume-run/volume-run.functions";
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

function Deploy({ placementLoss, version = "2:1:0" }: { placementLoss: PlacementLoss; version?: string }) {
  const { deploy, save, discard, neverSync, dialog } = useStoreChangeActions("acme", ref, version, (id) => admittedIds.push(id), placementLoss);
  const writer = useStoreWriter("acme");
  return <>
    <button type="button" onClick={() => deploy("  Ship the api  ")}>Deploy now</button>
    <button type="button" onClick={() => deploy("Keep the rest", { noPrune: true })}>Deploy, keep the rest</button>
    <button type="button" onClick={() => writer.edit({ environment: ref, changes: [{ op: "set", path: "web.replicas", value: 3 }] })}>Stage replicas</button>
    <button type="button" onClick={() => writer.commit({ command: "set_volume_storage", environment: ref, volume: "pg-data", storage: { kind: "docker" } })}>Stage storage</button>
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
  const { rerender } = render(<Deploy placementLoss={placementLoss} />, { wrapper });
  const admits = () => write.mock.calls.map(([call]) => call?.data.command);
  const shows = (version: string) => rerender(<Deploy placementLoss={placementLoss} version={version} />);
  return { write, admits, shows, queryClient };
}

/** A pointer press, as a browser sends it: down, then the click. */
function press(text: string) {
  act(() => {
    fireEvent.pointerDown(screen.getByText(text));
    fireEvent.click(screen.getByText(text));
  });
}

const edited = { ok: true, value: { written: "edited", environment: { revision: 3 } } } as never;

it("sends nothing for a Deploy pressed while a field's draft still stages, then deploys the review that shows it", async () => {
  const test = setup();
  let land: (value: typeof edited) => void = () => {};
  test.write.mockReturnValueOnce(new Promise<typeof edited>((resolve) => { land = resolve; })).mockResolvedValueOnce(admitted);

  // Pressing Deploy leaves the field, which stages its draft; the click lands in the same gesture.
  act(() => {
    fireEvent.click(screen.getByText("Stage replicas"));
    fireEvent.click(screen.getByText("Deploy now"));
  });
  land(edited);
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  expect(test.admits()).toMatchObject([{ command: "edit", changes: [{ path: "web.replicas", value: 3 }] }]);
  expect(admittedIds).toEqual([]);

  // The bar now shows the review with the draft; the next press ships exactly that.
  test.shows("3:1:0");
  press("Deploy now");
  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0", message: "Ship the api" });
  expect(toast.error).not.toHaveBeenCalled();
});

it("sends nothing for a Deploy whose own press staged a draft that saved before the click, then deploys the next press", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(edited).mockResolvedValueOnce(admitted);

  // The press begins, then the field it leaves stages its draft, which saves and shows before the button comes up.
  fireEvent.pointerDown(screen.getByText("Deploy now"));
  act(() => { fireEvent.click(screen.getByText("Stage replicas")); });
  await waitFor(() => expect(test.queryClient.isMutating()).toBe(0));
  test.shows("3:1:0");
  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await act(() => Promise.resolve());
  expect(test.admits()).toMatchObject([{ command: "edit" }]);
  expect(admittedIds).toEqual([]);

  press("Deploy now");
  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0" });
});

it("sends nothing for a Deploy held down while a draft saves, then deploys the next press", async () => {
  const test = setup();
  let land: (value: typeof edited) => void = () => {};
  test.write.mockReturnValueOnce(new Promise<typeof edited>((resolve) => { land = resolve; })).mockResolvedValueOnce(admitted);
  press("Stage replicas");

  // The press begins while the draft still saves, which lands and shows before the button comes up.
  fireEvent.pointerDown(screen.getByText("Deploy now"));
  land(edited);
  await waitFor(() => expect(test.queryClient.isMutating()).toBe(0));
  test.shows("3:1:0");
  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await act(() => Promise.resolve());
  expect(test.admits()).toMatchObject([{ command: "edit" }]);
  expect(admittedIds).toEqual([]);

  press("Deploy now");
  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0" });
});

it("sends nothing for a Deploy whose Space is held while a draft saves", async () => {
  const test = setup();
  let land: (value: typeof edited) => void = () => {};
  test.write.mockReturnValueOnce(new Promise<typeof edited>((resolve) => { land = resolve; }));
  press("Stage replicas");

  const deploy = screen.getByText("Deploy now");
  fireEvent.keyDown(deploy, { key: " " });
  land(edited);
  await waitFor(() => expect(test.queryClient.isMutating()).toBe(0));
  test.shows("3:1:0");
  fireEvent.keyDown(deploy, { key: " ", repeat: true });
  act(() => { fireEvent.click(deploy); });
  await act(() => Promise.resolve());
  expect(test.admits()).toMatchObject([{ command: "edit" }]);
  expect(admittedIds).toEqual([]);
});

it("judges each press with no pointer or key before it once, so the one after a staging press deploys", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(edited).mockResolvedValueOnce(admitted);
  press("Stage replicas");
  await waitFor(() => expect(test.queryClient.isMutating()).toBe(0));
  test.shows("3:1:0");

  // Assistive tech activates the button without a pointer or key.
  act(() => { screen.getByText("Deploy now").click(); });
  await act(() => Promise.resolve());
  expect(admittedIds).toEqual([]);
  act(() => { screen.getByText("Deploy now").click(); });
  await waitFor(() => expect(admittedIds).toHaveLength(1));
  expect(test.admits()[1]).toMatchObject({ command: "admit", version: "3:1:0" });
});

it("sends nothing for a Deploy pressed while a Volume's storage still saves", async () => {
  const test = setup();
  let land: (value: never) => void = () => {};
  test.write.mockReturnValueOnce(new Promise((resolve) => { land = resolve; }));
  press("Stage storage");
  press("Deploy now");
  land({ ok: true, value: { written: "volume", environment: { revision: 3 } } } as never);
  await waitFor(() => expect(test.queryClient.isMutating()).toBe(0));
  expect(test.admits()).toMatchObject([{ command: "set_volume_storage" }]);
  expect(admittedIds).toEqual([]);
});

it("reports a Save pressed while a field's draft still stages as not saved, and sends nothing", async () => {
  const test = setup();
  test.write.mockReturnValueOnce(new Promise(() => {}));
  act(() => {
    fireEvent.click(screen.getByText("Stage replicas"));
    fireEvent.click(screen.getByText("Save now"));
  });
  await waitFor(() => expect(saves).toEqual([false]));
  expect(test.admits()).toMatchObject([{ command: "edit" }]);
  expect(toast.error).not.toHaveBeenCalled();
});

it("leaves a Deploy the Store refused for a review that moved elsewhere unadmitted, saying so", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(refusal("conflict", { revision: 5 }));
  act(() => { fireEvent.click(screen.getByText("Deploy now")); });
  await waitFor(() => expect(toast.error).toHaveBeenCalledWith("refused: conflict"));
  expect(test.admits()).toMatchObject([{ command: "admit", version: "2:1:0" }]);
  expect(admittedIds).toEqual([]);
});

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

it("keeps other services through the ask before a Deploy deletes Volume data", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce(admitted).mockResolvedValueOnce(refusal("confirmation_required", loss)).mockResolvedValueOnce(admitted);

  press("Deploy now");
  await waitFor(() => expect(admittedIds).toHaveLength(1));
  press("Deploy, keep the rest");
  await screen.findByText("pg-data");
  fireEvent.change(screen.getByPlaceholderText("shop/production"), { target: { value: "shop/production" } });
  fireEvent.click(screen.getByRole("button", { name: "Deploy" }));

  await waitFor(() => expect(admittedIds).toHaveLength(2));
  expect(test.admits()).toMatchObject([
    { admit: "deploy", no_prune: false },
    { admit: "deploy", accept_volume_loss: [], no_prune: true },
    { admit: "deploy", accept_volume_loss: ["pg-data"], no_prune: true },
  ]);
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

describe("an open Data loss confirm across midnight", () => {
  const point = new Date(2026, 9, 11, 23, 55);
  const clock = new Intl.DateTimeFormat("en-GB", { hour: "2-digit", minute: "2-digit" });
  const dated = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" });
  beforeEach(() => {
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    vi.setSystemTime(new Date(2026, 9, 11, 23, 59));
  });
  afterEach(() => vi.useRealTimers());

  function StagedDeploy() {
    const { loss } = useStagedPlacements("acme", ref, "env-1", [{ id: "vol-restored", name: "data-2355", change: null }]);
    const { deploy, dialog } = useStoreChangeActions("acme", ref, "2:1:0", () => undefined, loss);
    return <>
      <button type="button" onClick={() => deploy(null)}>{`Deploy ${loss.items.length}`}</button>
      {dialog}
    </>;
  }

  it("dates a staged restore's point once the day turns", async () => {
    const test = setup();
    cleanup();
    const restore: PlacementRow = {
      volumeId: "vol-restored", environmentId: "env-1", mirrors: [], unreached: false, deploymentId: null,
      staged: {
        primary: "web-1".padEnd(32, "0") as never, mirrors: [], deletes: [],
        seed: { sourceVolumeId: "vol-source", sourceName: "data", dockerVolume: "shop_vol-restored", point: { guid: "1", name: "p", at: point.getTime() / 1000 } },
      },
    };
    vi.spyOn(placements, "listPlacementsServerFn").mockResolvedValue([restore] as never);
    vi.spyOn(runs, "inspectVolumeCopiesServerFn").mockResolvedValue([] as never);
    render(<QueryClientProvider client={test.queryClient}><StagedDeploy /></QueryClientProvider>);
    fireEvent.click(await screen.findByText("Deploy 1"));

    expect(await screen.findByText(`changes after ${clock.format(point)}`)).toBeTruthy();
    act(() => vi.advanceTimersByTime(2 * 60_000));
    expect(screen.getByText(`changes after ${dated.format(point)}`)).toBeTruthy();
  });

  it("dates the point of a restore Cloud refused once the day turns", async () => {
    const test = setup();
    const restore = { volumes: [{ name: "pg-data", deletes: [{ machine_id: "m1" }], loss: "writes", after: point.getTime() / 1000 }], accept: ["pg-data"], version: "9:1:0.1" };
    test.write.mockResolvedValueOnce(refusal("confirmation_required", restore));
    act(() => { fireEvent.click(screen.getByText("Deploy now")); });

    expect(await screen.findByText(`hetzner-1 · changes after ${clock.format(point)}`)).toBeTruthy();
    act(() => vi.advanceTimersByTime(2 * 60_000));
    expect(screen.getByText(`hetzner-1 · changes after ${dated.format(point)}`)).toBeTruthy();
  });
});

it("discards a Setting by its Store path, through the Environment's queue", async () => {
  const test = setup();
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "discarded", environment: { revision: 4 } } } as never);

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
