// @vitest-environment jsdom

import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DeploymentSummary } from "@ployz/sdk";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { asTestDouble } from "#/lib/test-double";
import { AgentDock } from "#/components/agent-sidebar/agent-dock";
import { BottomBar, BottomBarSlot, HistoryStages } from "./BottomBar";

const [running, queued] = ["aaaa1111-uuid", "aaaa2222-uuid"];
const deployment = (id: string, status: DeploymentSummary["status"], number: number) =>
  asTestDouble<DeploymentSummary>()({ id, status, number, services: [], upload: null });
const replicas: ChangeGroup = asTestDouble<ChangeGroup>()({
  nodeType: "service", nodeId: "api", nodeName: "api", canDiscard: true, changeCount: 1, lifecycle: "update",
  rows: [{ label: "Replicas", currentValue: "1", newValue: "2", kind: "update", path: "api.replicas", changeKey: "api", canDiscard: true }],
});
const cache = asTestDouble<ChangeGroup>()({ ...replicas, nodeId: "cache", nodeName: "cache" });
const onDeploy = vi.fn();
const onDiscardAll = vi.fn();
const onSave = vi.fn();
/** In flight, newest first, as the Store lists them. */
let active: DeploymentSummary[] = [];
let ended: DeploymentSummary[] | undefined = [];
let storage: (deploymentId: string) => string | null = () => null;
/** Secrets without a value the next Deploy leaves out. */
let notSet = 0;
/** Moves Deployments between in flight and ended while the bar stays mounted. */
let settle: (lists: { active: DeploymentSummary[]; ended: DeploymentSummary[] | undefined }) => void = () => {};

beforeEach(() => {
  active = [];
  ended = [];
  storage = () => null;
  notSet = 0;
  localStorage.clear();
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); onDeploy.mockClear(); onSave.mockReset(); });

function open(url: string, groups: ChangeGroup[] = [], totalChanges = 0, canSave = false) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas", component: function Canvas() {
    const [slot, setSlot] = useState<HTMLElement | null>(null);
    const [lists, setLists] = useState({ active, ended });
    settle = setLists;
    const [dock, setDock] = useState<HTMLElement | null>(null);
    const [agent, setAgent] = useState(false);
    const [stages, setStages] = useState(0);
    return (
      <AgentDock.Provider value={dock}>
      <BottomBarSlot.Provider value={slot}>
      <HistoryStages.Provider value={{ count: stages, staged: () => setStages(stages + 1) }}>
        <button type="button" onClick={() => setAgent(!agent)}>Agent</button>
        <button type="button" onClick={() => setStages(stages + 1)}>Stage from History</button>
        {agent ? <aside aria-label="Agent"><div ref={setDock} /></aside> : null}
        <section aria-label="Canvas"><div ref={setSlot} /></section>
        <BottomBar groups={groups} totalChanges={totalChanges} canSave={canSave} onDeploy={onDeploy} onSave={onSave}
          onDiscardAll={onDiscardAll} onDiscardNode={() => {}} onDiscardRow={() => {}} active={lists.active} ended={lists.ended} storage={storage} notes={{}} notSet={notSet}
          proposals={{ included: [] }} />
        <Outlet />
      </HistoryStages.Provider>
      </BottomBarSlot.Provider>
      </AgentDock.Provider>
    );
  } });
  const index = createRoute({ getParentRoute: () => canvas, path: "/", component: () => null });
  const page = createRoute({ getParentRoute: () => canvas, path: "deployments/$deploymentId", component: () => <p>Deployment Page</p> });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([
      environment.addChildren([canvas.addChildren([index, page])])])])])]),
    history: createMemoryHistory({ initialEntries: [url] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}
const canvasUrl = "/cloud/acme/shop/production";
const bar = async () => within(await screen.findByRole("group", { name: "Bottom bar" }));

it("shows changes to deploy first, in one row like Railway's: the count, Details, Deploy (⇧+Enter) and Discard under ⋮", async () => {
  open(canvasUrl, [replicas], 1);
  const staged = await bar();
  expect(staged.getAllByText("1 change")).toHaveLength(2);
  // What changed is Details' and the canvas's, so the bar stays small.
  expect(staged.queryByText("api")).toBeNull();
  expect(staged.queryByText(/Replicas/)).toBeNull();
  const deployButton = staged.getByRole("button", { name: /^Deploy/ });
  expect(deployButton.textContent).toBe("Deploy");
  expect(deployButton.getAttribute("aria-keyshortcuts")).toBe("Shift+Enter");

  fireEvent.click(staged.getByRole("button", { name: "Details" }));
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));

  await act(async () => { fireEvent.keyDown(document.body, { key: "Enter", shiftKey: true }); });
  expect(onDeploy).toHaveBeenCalledOnce();
  fireEvent.click(staged.getByRole("button", { name: /^Deploy/ }));
  expect(onDeploy).toHaveBeenCalledTimes(2);

  await act(async () => { fireEvent.click(staged.getByRole("button", { name: "More change actions" })); });
  fireEvent.click(await screen.findByRole("menuitem", { name: "Discard all changes" }));
  expect(onDiscardAll).toHaveBeenCalledOnce();
});

it("saves without deploying from Alt+click on Deploy and from Save only under ⋮", async () => {
  onSave.mockResolvedValue(true);
  open(canvasUrl, [replicas], 1, true);
  const staged = await bar();
  act(() => { fireEvent.keyDown(window, { key: "Alt", altKey: true }); });
  const button = staged.getByRole("button", { name: /^Save only/ });
  await act(async () => { fireEvent.click(button, { altKey: true }); });
  expect(onSave).toHaveBeenCalledOnce();
  expect(onDeploy).not.toHaveBeenCalled();
  act(() => { fireEvent.keyUp(window, { key: "Alt", altKey: false }); });
  expect(staged.getByRole("button", { name: /^Deploy/ }).textContent).toBe("Deploy");

  await act(async () => { fireEvent.click(staged.getByRole("button", { name: "More change actions" })); });
  await act(async () => { fireEvent.click(await screen.findByRole("menuitem", { name: "Save only" })); });
  expect(onSave).toHaveBeenCalledTimes(2);
  expect(onDeploy).not.toHaveBeenCalled();
});

it("never deploys from Alt+click, even once everything is saved and the changes still wait to deploy", async () => {
  open(canvasUrl, [replicas], 1, false);
  const staged = await bar();
  await act(async () => { fireEvent.click(staged.getByRole("button", { name: /^Deploy/ }), { altKey: true }); });
  act(() => { fireEvent.keyDown(window, { key: "Alt", altKey: true }); });
  const button = staged.getByRole("button", { name: /^Save only/ });
  expect(button).toHaveProperty("disabled", true);
  await act(async () => { fireEvent.click(button, { altKey: true }); });
  expect(onDeploy).not.toHaveBeenCalled();
  expect(onSave).not.toHaveBeenCalled();
});

it("counts the secrets the Deploy leaves out beside the changes", async () => {
  notSet = 2;
  open(canvasUrl, [replicas], 1);
  expect((await bar()).getByText("1 change · 2 not set")).toBeTruthy();
});

it("keeps the review open through Discard all so its inverse draft stays reviewable", async () => {
  onDiscardAll.mockClear();
  open(canvasUrl, [replicas], 1);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Discard all" })); });
  expect(onDiscardAll).toHaveBeenCalledOnce();
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
});

it("keeps staged changes over a running Deployment, with Deploy next", async () => {
  active = [deployment(running, "running", 1)];
  open(canvasUrl, [replicas, cache], 2);
  const staged = await bar();
  expect(staged.getAllByText("2 changes")).toHaveLength(2);
  expect(staged.getByRole("button", { name: /^Deploy next/ })).toBeTruthy();
  expect(staged.queryByRole("link", { name: "Logs" })).toBeNull();
});

it("otherwise shows the running Deployment, and the queued one while the running one's page is open", async () => {
  active = [deployment(queued, "queued", 2), deployment(running, "running", 1)];
  const router = open(canvasUrl);
  const shown = await bar();
  expect(shown.getByText("Deploying · Deployment #1")).toBeTruthy();
  expect(shown.getByText("Deploys every service")).toBeTruthy();
  fireEvent.click(shown.getByRole("link", { name: "Logs" }));
  await screen.findByText("Deployment Page");
  expect(router.state.location.href).toBe(`${canvasUrl}/deployments/${running}`);
  expect((await bar()).getByText("Queued · Deployment #2")).toBeTruthy();
});

it("words a Deploy's storage step while it moves a Volume before its Services", async () => {
  active = [deployment(queued, "queued", 2)];
  storage = (id) => id === queued ? "pg-data · Moving to web-2" : null;
  open(canvasUrl);
  const shown = await bar();
  expect(shown.getByText("Queued · Deployment #2")).toBeTruthy();
  expect(shown.getByText("pg-data · Moving to web-2")).toBeTruthy();
});

it("shows how a Deployment it watched ended, until dismissed, and a Deployed one only for a few seconds", async () => {
  active = [deployment(running, "running", 1)];
  open(canvasUrl);
  expect((await bar()).getByText("Deploying · Deployment #1")).toBeTruthy();
  act(() => settle({ active: [], ended: [deployment(running, "failed", 1), deployment(queued, "applied", 0)] }));
  const failed = await bar();
  expect(failed.getByText("Failed · Deployment #1")).toBeTruthy();
  expect(failed.getByRole("link", { name: "Logs" })).toBeTruthy();
  fireEvent.click(failed.getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();

  vi.useFakeTimers({ shouldAdvanceTime: true });
  act(() => settle({ active: [deployment(queued, "running", 2)], ended: [] }));
  expect((await bar()).getByText("Deploying · Deployment #2")).toBeTruthy();
  act(() => settle({ active: [], ended: [deployment(queued, "applied", 2), deployment(running, "failed", 1)] }));
  expect((await bar()).getByText("Deployed · Deployment #2")).toBeTruthy();
  act(() => { vi.advanceTimersByTime(6_000); });
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
  vi.useRealTimers();
});

it("shows a failed Deployment above the changes it handed back, until dismissed", async () => {
  active = [deployment(running, "running", 1)];
  open(canvasUrl, [replicas], 1);
  expect((await bar()).getByRole("button", { name: /^Deploy next/ })).toBeTruthy();
  act(() => settle({ active: [], ended: [deployment(running, "failed", 1)] }));
  const both = await bar();
  const rows = both.getAllByText(/Failed · Deployment #1|^1 change$/).map((node) => node.textContent);
  expect(rows).toEqual(["Failed · Deployment #1", "1 change", "1 change"]);
  expect(both.getByRole("link", { name: "Logs" })).toBeTruthy();
  expect(both.getByRole("button", { name: /^Deploy$/ })).toBeTruthy();
  fireEvent.click(both.getByRole("button", { name: "Dismiss" }));
  const staged = await bar();
  expect(staged.queryByText("Failed · Deployment #1")).toBeNull();
  expect(staged.getAllByText("1 change")).toHaveLength(2);
});

it("never shows a Deployed one that ended before it was watched", async () => {
  open(canvasUrl);
  act(() => settle({ active: [], ended: [deployment(running, "applied", 1)] }));
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});

it("shows the newest Deployment Failed on a fresh mount, until dismissed, and keeps it dismissed after a reload", async () => {
  ended = [deployment(running, "failed", 1), deployment(queued, "failed", 0)];
  open(canvasUrl);
  const failed = await bar();
  expect(failed.getByText("Failed · Deployment #1")).toBeTruthy();
  expect(failed.getByRole("link", { name: "Logs" })).toBeTruthy();
  fireEvent.click(failed.getByRole("button", { name: "Dismiss" }));
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
  cleanup();
  open(canvasUrl);
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});

it("shows how a Deployment ended when this tab opened its page before it was listed, as Deploy does", async () => {
  const router = open(`${canvasUrl}/deployments/${running}`);
  await screen.findByText("Deployment Page");
  act(() => settle({ active: [], ended: [deployment(running, "failed", 1)] }));
  await act(async () => router.history.push(canvasUrl));
  expect((await bar()).getByText("Failed · Deployment #1")).toBeTruthy();
});

it("shows nothing after closing the page of a Deployment that had already ended", async () => {
  ended = [deployment(running, "applied", 1)];
  const router = open(`${canvasUrl}/deployments/${running}`);
  await screen.findByText("Deployment Page");
  await act(async () => router.history.push(canvasUrl));
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});

it("watches no Deployment whose page opened before the ended ones loaded", async () => {
  ended = undefined;
  const router = open(`${canvasUrl}/deployments/${running}`);
  await screen.findByText("Deployment Page");
  act(() => settle({ active: [], ended: [deployment(running, "applied", 1)] }));
  await act(async () => router.history.push(canvasUrl));
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});

it("hides while nothing is staged or running, and while the only Deployment's page is open", async () => {
  active = [deployment(running, "running", 1)];
  open(`${canvasUrl}/deployments/${running}`);
  await screen.findByText("Deployment Page");
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
  cleanup();
  active = [];
  open(canvasUrl);
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});


it("preserves the Change message when a Save is refused or fails", async () => {
  onSave.mockResolvedValue(false);
  open(canvasUrl, [replicas], 1, true);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Change message" }), { target: { value: "Explain this change" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save only" })); });
  expect(onSave).toHaveBeenCalledWith("Explain this change");
  expect(screen.getByRole("textbox", { name: "Change message" })).toHaveProperty("value", "Explain this change");
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
});

it("clears the Change message once a Save succeeds", async () => {
  onSave.mockResolvedValue(true);
  open(canvasUrl, [replicas], 1, true);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Change message" }), { target: { value: "Explain this change" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save only" })); });
  expect(onSave).toHaveBeenCalledWith("Explain this change");
  expect(screen.getByRole("textbox", { name: "Change message" })).toHaveProperty("value", "");
});

it("keeps a Change message edited while the Save is in flight", async () => {
  let saved: (value: boolean) => void = () => {};
  onSave.mockReturnValue(new Promise<boolean>((resolve) => { saved = resolve; }));
  open(canvasUrl, [replicas], 1, true);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  const box = () => screen.getByRole("textbox", { name: "Change message" });
  fireEvent.change(box(), { target: { value: "Explain this change" } });
  fireEvent.click(screen.getByRole("button", { name: "Save only" }));
  fireEvent.change(box(), { target: { value: "The next change" } });
  await act(async () => { saved(true); });
  expect(box()).toHaveProperty("value", "The next change");
});

it("clears the Change message once History stages other changes", async () => {
  open(canvasUrl, [replicas], 1, true);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Change message" }), { target: { value: "Raise api replicas" } });
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  fireEvent.click(screen.getByRole("button", { name: "Stage from History" }));
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  expect(screen.getByRole("textbox", { name: "Change message" })).toHaveProperty("value", "");
});

it("docks in the open agent and returns to the canvas when it closes, keeping the Change message", async () => {
  open(canvasUrl, [replicas], 1);
  const canvas = await screen.findByRole("region", { name: "Canvas" });
  expect((await within(canvas).findByRole("group", { name: "Bottom bar" })).dataset["docked"]).toBeUndefined();
  fireEvent.click(within(canvas).getByRole("button", { name: "Details" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Change message" }), { target: { value: "Explain this change" } });
  fireEvent.click(screen.getByRole("button", { name: "Close" }));

  fireEvent.click(screen.getByRole("button", { name: "Agent" }));
  const agent = screen.getByRole("complementary", { name: "Agent" });
  expect(within(agent).getByRole("group", { name: "Bottom bar" }).dataset["docked"]).toBe("true");
  expect(within(canvas).queryByRole("group", { name: "Bottom bar" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Agent" }));
  expect(within(canvas).getByRole("group", { name: "Bottom bar" }).dataset["docked"]).toBeUndefined();
  fireEvent.click(within(canvas).getByRole("button", { name: "Details" }));
  expect(screen.getByRole("textbox", { name: "Change message" })).toHaveProperty("value", "Explain this change");
});
