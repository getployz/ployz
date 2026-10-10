// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ConfigListing, DiffView, ServiceListing, VolumeListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import * as writers from "./store-write";
import { StoreConfigMounts } from "./StoreConfigMounts";
import { StoreVolumeMounts } from "./StoreVolumeMounts";

const service = asTestDouble<ServiceListing>()({ id: "service-id", name: "web", change: null });
const config = asTestDouble<ConfigListing>()({ id: "config-id", name: "app", change: null, files: [], mounts: [{ service: "web", dir: "/etc/app" }] });
const volume = asTestDouble<VolumeListing>()({ id: "volume-id", name: "data", shared_writes: false, storage: { kind: "docker" }, change: null, mounts: [{ service: "web", path: "/data" }] });
const common = { organizationSlug: "acme", environment: { project: "shop", environment: "production" }, services: [service],
  diff: asTestDouble<DiffView>()({ changes: [] }) };

async function menuAction(name: "Edit directory" | "Unmount") {
  fireEvent.click(screen.getByRole("button", { name: /Actions for .* mount/ }));
  fireEvent.click(await screen.findByRole("menuitem", { name }));
}

function setup() {
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<void>((yes, no) => { resolve = yes; reject = no; });
  const edit = vi.fn(() => ({ isPersisted: { promise } }));
  const commit = vi.fn(() => ({ isPersisted: { promise } }));
  vi.spyOn(writers, "useStoreWriter").mockReturnValue(asTestDouble<ReturnType<typeof writers.useStoreWriter>>()({ edit, commit }));
  return { edit, commit, resolve, reject };
}
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("edits Config directories using current identities from the resource panel", async () => {
  const write = setup();
  const resourceId = config.id;
  const view = render(<StoreConfigMounts {...common} resourceId={resourceId} configs={[config]} />);
  await menuAction("Edit directory");
  fireEvent.change(screen.getByRole("textbox", { name: "Directory" }), { target: { value: "/etc/new" } });
  view.rerender(<StoreConfigMounts {...common} resourceId={resourceId} services={[{ ...service, name: "renamed" }]}
    configs={[{ ...config, name: "renamed-config", mounts: [{ service: "renamed", dir: "/etc/app" }] }]} />);
  fireEvent.click(screen.getByRole("button", { name: "Save directory" }));
  expect(write.commit).toHaveBeenCalledWith({ command: "attach_config", environment: common.environment, service: "renamed", config: "renamed-config", dir: "/etc/new" }, ["invalid", "conflict"]);
  await act(async () => write.reject(new Error("Offline")));
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Directory" }).value).toBe("/etc/new");
  expect(screen.getByRole("alert").textContent).toContain("Try again");
});

it("keeps an unmount error visible after the optimistic Volume row disappears in the resource panel", async () => {
  const write = setup();
  const resourceId = volume.id;
  const view = render(<StoreVolumeMounts {...common} resourceId={resourceId} volumes={[volume]} replicasOf={() => 1} />);
  await menuAction("Unmount");
  expect(write.edit).toHaveBeenCalledWith({ environment: common.environment, changes: [{ op: "unset", path: "web.mounts.data" }] });
  view.rerender(<StoreVolumeMounts {...common} resourceId={resourceId} volumes={[{ ...volume, mounts: [] }]} replicasOf={() => 1} />);
  expect(screen.queryByRole("button", { name: "Unmount" })).toBeNull();
  expect(screen.getByRole("status").textContent).toContain("Saving unmount");
  await act(async () => write.reject(new Error("Offline")));
  expect(screen.getByRole("alert").textContent).toContain("Try again");
});

it("rechecks Volume writer eligibility and collisions while an edit form is open", async () => {
  const write = setup();
  const props = { ...common, resourceId: volume.id, replicasOf: () => 1 };
  const view = render(<StoreVolumeMounts {...props} volumes={[volume]} />);
  await menuAction("Edit directory");
  fireEvent.change(screen.getByRole("textbox", { name: "Directory" }), { target: { value: "/new" } });
  view.rerender(<StoreVolumeMounts {...props} replicasOf={() => 2} volumes={[volume]} />);
  fireEvent.click(screen.getByRole("button", { name: "Save directory" }));
  expect(screen.getByRole("alert").textContent).toContain("Runs 2 replicas");
  expect(write.edit).not.toHaveBeenCalled();
  view.rerender(<StoreVolumeMounts {...props} volumes={[volume, { ...volume, id: "other", name: "other", mounts: [{ service: "web", path: "/new" }] }]} />);
  fireEvent.click(screen.getByRole("button", { name: "Save directory" }));
  expect(screen.getByRole("alert").textContent).toContain("Another volume");
  expect(write.edit).not.toHaveBeenCalled();
});

it("refuses a stale Config counterpart without clearing the directory or writing", async () => {
  const write = setup();
  const props = { ...common, resourceId: config.id, configs: [config] };
  const view = render(<StoreConfigMounts {...props} />);
  await menuAction("Edit directory");
  fireEvent.change(screen.getByRole("textbox", { name: "Directory" }), { target: { value: "/retained" } });
  view.rerender(<StoreConfigMounts {...props} services={[{ ...service, change: "delete" }]} />);
  fireEvent.click(screen.getByRole("button", { name: "Save directory" }));
  expect(screen.getByRole("alert").textContent).toContain("being removed");
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Directory" }).value).toBe("/retained");
  expect(write.commit).not.toHaveBeenCalled();
});

it.each(["Config", "Volume"])("returns focus to the surviving menu trigger after closing the %s edit form", async (entry) => {
  setup();
  if (entry.startsWith("Config")) {
    render(<StoreConfigMounts {...common} resourceId={config.id} configs={[config]} />);
  } else {
    render(<StoreVolumeMounts {...common} resourceId={volume.id} volumes={[volume]} replicasOf={() => 1} />);
  }
  const action = screen.getByRole("button", { name: /Actions for .* mount/ });
  await menuAction("Edit directory");
  await act(async () => { await new Promise((resolve) => requestAnimationFrame(resolve)); });
  const directory = screen.getByRole("textbox", { name: "Directory" });
  expect(document.activeElement).toBe(directory);
  expect(screen.queryByRole("menu")).toBeNull();
  fireEvent.keyDown(directory, { key: "Escape" });
  expect(screen.queryByRole("textbox", { name: "Directory" })).toBeNull();
  await act(async () => { await new Promise((resolve) => requestAnimationFrame(resolve)); });
  expect(document.activeElement).toBe(action);
});

it.each(["Config", "Volume"])("returns focus to Add when cancelling a new %s mount", async (kind) => {
  setup();
  if (kind === "Config") {
    render(<StoreConfigMounts {...common} resourceId={config.id} configs={[{ ...config, mounts: [] }]} />);
  } else {
    render(<StoreVolumeMounts {...common} resourceId={volume.id} volumes={[{ ...volume, mounts: [] }]} replicasOf={() => 1} />);
  }
  const action = screen.getByRole("button", { name: "Mount on a service" });
  fireEvent.click(action);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await act(async () => { await new Promise((resolve) => requestAnimationFrame(resolve)); });
  expect(document.activeElement).toBe(action);
});


it.each(["Config", "Volume"].flatMap((entry) =>
  ["Add", "Edit"].map((origin) => ({ entry, origin }))))("preserves $origin focus after visiting another row menu in $entry", async ({ entry, origin }) => {
  setup();
  const services = [service, { ...service, id: "second-service", name: "worker" }, { ...service, id: "third-service", name: "jobs" }];
  if (entry.startsWith("Config")) {
    const configs = [{ ...config, mounts: [...config.mounts, { service: "worker", dir: "/etc/app" }] }];
    render(<StoreConfigMounts {...common} services={services} resourceId={config.id} configs={configs} />);
  } else {
    const volumes = [{ ...volume, mounts: [...volume.mounts, { service: "worker", path: "/data" }] }];
    render(<StoreVolumeMounts {...common} services={services} resourceId={volume.id} volumes={volumes} replicasOf={() => 1} />);
  }
  const [firstMenu, secondMenu] = screen.getAllByRole("button", { name: /Actions for .* mount/ });
  if (!firstMenu || !secondMenu) throw new Error("Expected two mounted rows");
  const openingButton = origin === "Add" ? screen.getByRole("button", { name: "Mount on a service" }) : firstMenu;
  fireEvent.click(openingButton);
  if (origin === "Edit") fireEvent.click(await screen.findByRole("menuitem", { name: "Edit directory" }));
  await act(async () => { await new Promise((resolve) => requestAnimationFrame(resolve)); });
  const directory = screen.getByRole("textbox", { name: "Directory" });
  act(() => { secondMenu.focus(); });
  expect(document.activeElement).toBe(secondMenu);
  act(() => { directory.focus(); });
  if (origin === "Add") fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  else fireEvent.keyDown(directory, { key: "Escape" });
  await act(async () => { await new Promise((resolve) => requestAnimationFrame(resolve)); });
  expect(document.activeElement).toBe(openingButton);
});
